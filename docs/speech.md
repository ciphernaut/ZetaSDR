# Speech on a channel

A ZetaSDR node transcribes a channel by attaching a `transcribe` job to a running demodulator
job (`zeta job attach --kind transcribe --job ID`). The chain is `stream_in → denoise → vad →
transcribe`; the last stage hands each utterance, as a 16 kHz mono WAV, to a **speech
backend** the operator chose with a global flag:

- `--stt-socket PATH` (env `ZETA_STT_SOCKET`; default `$XDG_RUNTIME_DIR/voiced.sock` when that
  file exists): a speech daemon on a Unix socket.
- `--stt-node URL` (env `ZETA_STT_NODE`; the secret in `ZETA_STT_NODE_SECRET`): another node's
  `POST /stt`.

The two flags are exclusive (exit 1). Without either, a `transcribe` job fails with a reason
naming the missing backend. This page has three separable sections: installing the
`local-voice` daemon on a node's host, writing your own daemon, and sharing one daemon between
several nodes. The recipe for using it is in `docs/agents/field-craft.md` ("Transcribing a
channel").

## 1. Installing `local-voice` on a node's host

`local-voice` is the Parakeet-based daemon (`bin/voiced`) this project was built against. Do
all of this as the user that runs `zeta serve`, on the host that serves the node.

1. Clone it under that user's home (the laptop's copy is `/Projects/local-voice`; use its
   remote, or `git clone` from the laptop):

   ```bash
   cd ~
   git clone /Projects/local-voice local-voice      # or the remote URL
   cd local-voice
   ```

2. Install its dependencies with `uv`. The project needs Python 3.13 or newer and only pip
   wheels (`sherpa-onnx`, `sherpa-onnx-core`, `piper-tts`, `numpy`), so no apt packages are
   needed for the daemon. `uv` fetches its own Python when the system one is older:

   ```bash
   uv python install 3.13       # only if `python3 --version` is below 3.13
   uv sync
   ```

   Do not run `install.sh` on a node: it is for a desktop and installs clipboard,
   notification and PipeWire packages (ydotool and the like) the daemon does not need.

3. Copy the Parakeet model directory. It is git-ignored, so it is not in the clone: copy
   `models/parakeet-tdt-0.6b-v2-int8` from the laptop's `/Projects/local-voice/models/` into
   `~/local-voice/models/`:

   ```bash
   mkdir -p ~/local-voice/models
   scp -r laptop:/Projects/local-voice/models/parakeet-tdt-0.6b-v2-int8 ~/local-voice/models/
   ```

4. Run `bin/voiced` under a user unit. `bin/voiced` reads `VOICED_SOCKET` (default
   `$XDG_RUNTIME_DIR/voiced.sock`) and `VOICED_MODEL`. The repository's
   `systemd/voiced.service` is a desktop unit (it waits for ydotool and PipeWire); a node wants
   this instead, as `~/.config/systemd/user/voiced.service`:

   ```ini
   [Unit]
   Description=local-voice speech daemon

   [Service]
   ExecStart=%h/local-voice/.venv/bin/python %h/local-voice/bin/voiced
   Restart=on-failure

   [Install]
   WantedBy=default.target
   ```

   ```bash
   systemctl --user daemon-reload
   systemctl --user enable --now voiced.service
   sudo loginctl enable-linger "$USER"      # so the unit survives logout and starts at boot
   ```

5. Check the socket exists, then start the node on it. The socket is in the user's runtime
   directory, which is where `--stt-socket` looks by default:

   ```bash
   ls -l "$XDG_RUNTIME_DIR/voiced.sock"
   zeta serve --listen 0.0.0.0:7979 --secret "$SECRET" --stt-socket "$XDG_RUNTIME_DIR/voiced.sock"
   ```

   The node prints `zeta: speech backend: socket:/run/user/UID/voiced.sock` on start. The daemon
   answers `languages` with `error:unknown command`, so the node takes it to speak English only.

**the field host.** There the node runs as the `sdr` user, so the clone is
`~sdr/zetasdr/local-voice` (that is, `~/zetasdr/local-voice` when logged in as `sdr`), the unit
is `~sdr/.config/systemd/user/voiced.service` with
`ExecStart=%h/zetasdr/local-voice/.venv/bin/python %h/zetasdr/local-voice/bin/voiced`, and
`sudo loginctl enable-linger sdr` keeps it running when nobody is logged in. Live acceptance
(a transcript of 100.5 MHz) is pending on that host and needs the install above.

## 2. Bring your own daemon

Any daemon that speaks this protocol on a Unix socket is a backend. One connection per
request; a request is one line, the answer is one line of at most 64 KiB:

```text
file:<absolute path to a 16 kHz mono s16 WAV>\n   ->   ok:<text>\n   |   error:<message>\n
languages\n                                      ->   ok:<comma-separated codes>\n
```

- The node writes each utterance to a WAV file in its state directory and sends its absolute
  path; the node removes the file after the reply, so the daemon only reads it.
- `languages` is asked once, when the node builds the backend, with a 2 s limit. Any other
  answer (`error:unknown command`, nothing within 2 s, no socket) means `en`.
- A job's `language` that the daemon did not list fails before the daemon is contacted.

`examples/python/stt_daemon_stub.py` is a complete daemon in about 45 lines: it answers a fixed
text for every `file:` and `ok:en,de` for `languages`. It is the place a Whisper or Vosk wrapper
starts: replace `transcribe()` with a call into the model.

```bash
python3 -I examples/python/stt_daemon_stub.py "$XDG_RUNTIME_DIR/stub.sock" &
zeta serve --listen 127.0.0.1:7979 --secret "$SECRET" --stt-socket "$XDG_RUNTIME_DIR/stub.sock"
```

A backend that lives inside the node's process instead implements the
`zeta_speech::SpeechBackend` trait (`crates/zeta-speech/src/backend.rs`); `SocketBackend` and
`NodeBackend` are its two implementations.

## 3. One daemon for several nodes

Run the daemon on one host (the one with the CPU or GPU to spare) and point the other nodes at
the node that serves it:

```bash
# on the other node; the secret is the daemon node's --secret
ZETA_STT_NODE_SECRET=SECRET zeta serve --listen 0.0.0.0:7979 --secret "$LOCAL_SECRET" \
    --stt-node http://field-host:7979
```

- The secret is read from `ZETA_STT_NODE_SECRET` and never printed or put on a command line.
- Each utterance is forwarded to that node's `POST /stt?lang=CODE` (a 16 kHz mono `audio/wav`
  body, answered `{"text": "..."}`).
- A forwarded request carries `X-Zeta-Stt-Hop: 1`, and a node whose own backend would forward
  it again answers 508 (`stt forwarding loop`): a request is forwarded once, so two nodes
  pointing at each other fail instead of looping.
- `GET /stt/languages` answers `{"backend": NAME, "languages": [CODES]}` (503 without a
  backend); a node using `--stt-node` takes its languages from that answer, or `en` when the
  node is unreachable at start.
- `--stt-socket` and `--stt-node` are exclusive.

## Separation

`vad` gates the audio by energy over the noise floor, so music and other loud non-speech pass
the gate: they are gated, not separated, and are likely to be transcribed as nonsense. Source
separation (isolating the voice stem, for instance with Demucs on a GPU host) is a slot in the
chain: an external plugin stage (audio in, audio out) replaces the `denoise` node with
`graph get` and `graph put` on the attached job. `examples/python/separate_stub.py` is that
plugin with a pass-through body; its docstring says what a real separator returns (the stem
at 48 kHz mono, same length). Seconds of latency are fine, because `vad` and `transcribe` work
on the frames' `t0`, not on arrival time. `crates/zeta/tests/speech_chain.rs` runs the stub in
the chain. Register a plugin with `--plugin separate=/abs/path/separate_stub.py`.
