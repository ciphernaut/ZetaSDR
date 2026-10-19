# ZetaSDR

ZetaSDR is an agentic RF runtime: a node that owns signal sources, keeps a live picture of the
spectrum and exposes "what is there, what is it, decode it" as typed operations that a human, a
script or an LLM can invoke. This branch is the core node (ports and leases, IQ ring, stream bus,
jobs, the operation set, a SigMF file source and the `zeta` CLI) and the DSP slice (channel
extraction, spectrum, NBFM demodulation, APRS decoding, layer-1 measurement and classifiers),
with the transports and agent of plan 3, the hardware and foreign sources of plan 4 (KiwiSDR,
SoapySDR, external-process stages), the federation of plan 5 (another node as a source, VITA
49.2 over the websocket and UDP, a Pi 5 build), the web UI of plan 6 (waterfall, context
menu, agent transcript, graph view) and the audio path of plan 7 (48 kHz audio from every
demodulator, wideband FM, AM and SSB, a signal catalogue for layer 2).

Design: [docs/superpowers/specs/2026-10-18-zetasdr-design.md](docs/superpowers/specs/2026-10-18-zetasdr-design.md).
Plans: [docs/superpowers/plans/2026-10-18-core-node.md](docs/superpowers/plans/2026-10-18-core-node.md),
[docs/superpowers/plans/2026-10-18-dsp-slice.md](docs/superpowers/plans/2026-10-18-dsp-slice.md),
[docs/superpowers/plans/2026-10-18-transports-agent.md](docs/superpowers/plans/2026-10-18-transports-agent.md),
[docs/superpowers/plans/2026-10-19-hardware-sources.md](docs/superpowers/plans/2026-10-19-hardware-sources.md),
[docs/superpowers/plans/2026-10-19-federation.md](docs/superpowers/plans/2026-10-19-federation.md),
[docs/superpowers/plans/2026-10-19-ui.md](docs/superpowers/plans/2026-10-19-ui.md),
[docs/superpowers/plans/2026-10-19-audio-wfm.md](docs/superpowers/plans/2026-10-19-audio-wfm.md).

## Tests

The toolchain is pinned by `rust-toolchain.toml` (1.97); install `rustfmt` and `clippy` for it
(`rustup component add rustfmt clippy --toolchain 1.97`), then:

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## CLI walkthrough

One-shot commands open the node, apply one operation, persist and exit; they do not resume jobs.
`zeta serve` resumes persisted jobs and keeps them running until Ctrl-C. A one-shot `iq read` or
`classify` waits (up to 5 s) for the source ring to cover the requested span. File sources play in
real time and loop. `--source-file` is not persisted: pass it on every invocation.

While `zeta serve` runs it holds `serve.lock` (its pid) in the state directory, and the mutating
one-shot commands (`job start`, `job stop`, `graph put`, `source ensure`) exit 2 with
`{"error": "other", "message": "zeta serve (pid N) owns this state dir"}`. A lock whose pid is no
longer alive is treated as stale. Liveness is checked through `/proc/<pid>`, so this is Linux only.

```bash
cargo build -q -p zeta
cargo run -q -p zeta-source-file --features test-fixtures --example gen_tone -- scratch/fx
A="--state-dir scratch/state --source-file tone=scratch/fx/tone.sigmf-meta"
./target/debug/zeta $A source list
./target/debug/zeta $A job start --kind passthrough --source tone --center-hz 100000000 --bandwidth-hz 48000
./target/debug/zeta $A job list
./target/debug/zeta $A iq read --source tone --start-ns 0 --end-ns 1000000000 --center-hz 100000000 --bandwidth-hz 48000
ls scratch/state/captures               # a .sigmf-meta / .sigmf-data pair
./target/debug/zeta $A job stop --job <id from job start>
./target/debug/zeta agents-md            # the agent guide, rendered from the operation schema
```

`AGENTS.md` is generated from the operation schema by `zeta agents-md`; `zeta agents-md --check`
fails if the committed file has drifted.

## DSP

Eight job kinds, chosen with `job start --kind` (`zeta job start --help` lists them):

- `passthrough` copies the channel IQ through unchanged.
- `spectrum` emits one power-spectrum event per 100 ms frame (`size` 1024, `bin_hz`, `db` array).
- `fm_demod` channelises and demodulates narrowband FM to an audio stream (48 kHz, per-frame `peak`); `--deviation-hz` sets the peak deviation (default 5000).
- `wfm_demod` does the same for wideband broadcast FM: `--deviation-hz` default 75000, 50 µs de-emphasis (the stage also takes 75 µs, set in its graph with `graph put`), 15 kHz mono audio at 48 kHz.
- `am_demod` does the same for AM voice (envelope detector, 5 kHz audio at 48 kHz, slow AGC to a steady level; give it about 10 kHz of band).
- `ssb_demod` does the same for SSB voice (`--sideband usb|lsb`, default `usb`; 2.8 kHz audio at 48 kHz, slow AGC). `--center-hz` is the suppressed-carrier (dial) frequency; give it about 6 kHz of band so both sides of the carrier are kept.
- `aprs_decode` demodulates AFSK 1200 and HDLC-deframes AX.25, emitting one event per CRC-valid packet.
- `measure` emits one layer-1 feature record per second of IQ (occupied band, SNR, burst, symbol rate, label).

`--then-tail N,MS` on `job start` starts the job, tails up to N frames from its event (else audio)
stream, waiting up to MS ms, then stops the job unless `--keep` is given, and prints
`[job, frames, {"stopped": bool}]`. If the job is not running (for example it failed), it prints
`[job, job_record]` and exits 2. `zeta stream tail` tails a stream on its own. `--then-tail` is
not supported with `--node` yet: start the job remotely, then `stream list` and `stream tail`.
`zeta classify` runs every classifier (layer 1 measurement, layer 2 signature and
decode-to-identify) over a span of a source ring. Outputs below are trimmed.

Every demodulator outputs mono f32 audio at 48 kHz in ±1, whatever the channel rate: the channel
stage picks its rate for the band, and a streaming rational resampler (polyphase, reduced ratio
L/M, Hann-windowed sinc at 0.45 of the lower rate) takes the audio from there to 48 kHz, so 200
kHz, 250 kHz or 1 Msps channels need no integer relation to the audio rate. `--deviation-hz` is
accepted by `fm_demod` and `wfm_demod` only, `--sideband` by `ssb_demod` only; any other kind
refuses them.

Layer 1 (`measure`, and the first label of `classify`) names a band from its occupied bandwidth,
envelope coefficient of variation (cv) and discriminator audio, and its evidence names the
measurements it used. `wfm` at 0.9 is a band 40-250 kHz wide with a 19 kHz pilot line at least
10 dB above the surrounding audio spectrum, quoted ("19 kHz pilot +16.3 dB"), whatever the envelope
(multipath ripples a real station's); without a pilot, `wfm` at 0.6 is a band 60-250 kHz wide with
cv below 0.45 and rms deviation above 5 kHz. `am` is cv at least 0.25 with a carrier
line at the centre; `ssb` is cv at least 0.25, no carrier line, a 1.5-4 kHz band and 80 % of its
power on one side of the centre, and its evidence gives the dial frequency to hand to `ssb_demod`.
`fm` and `carrier` remain the narrowband rules (band at most 25 kHz, cv below 0.2), checked after
these.

Layer 2 adds a catalogue: iNTERCEPT's `signals.json` (594 known signals, Apache-2.0, copied
unchanged into `crates/zeta-dsp/data` with its `NOTICE`). It is a knowledge provider, so
`knowledge lookup` returns the signatures whose frequency ranges overlap the band, and a
classifier, so `classify` returns up to three layer-2 labels with catalogue names after the
layer-1 label. An entry scores 0.4 when one of its frequency ranges holds the measured centre
(required), 0.3 when the measured bandwidth lies within half its minimum to 1.5 times its maximum
bandwidth (required when that maximum is above 50 kHz, so a 12.5 kHz channel in the broadcast
band is not "FM Broadcast Radio"), and 0.3 when the layer-1 label maps to one of its modulations;
ties go to the entry with the narrower frequency range. A band layer 1 calls noise gets no
catalogue label.

```bash
cargo build -q -p zeta
cargo run -q -p zeta-dsp --features test-fixtures --example gen_aprs -- scratch/fx
cargo run -q -p zeta-source-file --features test-fixtures --example gen_tone -- scratch/fx
Z=./target/debug/zeta
S="--state-dir scratch/state"

# APRS: one event, src VK3ABC (dst APRS, info ">hi", fcs_ok true)
$Z $S --source-file aprs=scratch/fx/aprs.sigmf-meta job start --kind aprs_decode --source aprs \
    --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000

# NBFM audio from the tone: "kind": "audio" frames, 4800 samples at 48000 Hz each. The first
# frame's peak is a filter start-up transient (1.07); later frames are 0.40 (between 0.3 and 1.0).
$Z $S --source-file tone=scratch/fx/tone.sigmf-meta job start --kind fm_demod --source tone \
    --center-hz 100000000 --bandwidth-hz 12500 --then-tail 5,3000

# spectrum: one event, "size": 1024, "bin_hz": 46.875, "db": [-180.99, -179.14, -181.65, ...]
$Z $S --source-file tone=scratch/fx/tone.sigmf-meta job start --kind spectrum --source tone \
    --center-hz 100000000 --bandwidth-hz 12500 --then-tail 1,3000

# measure on the tone: the tone is an unmodulated carrier, so label.name is "carrier"
# (confidence 0.7, snr_db 157.4, symbol_rate_baud null)
$Z $S --source-file tone=scratch/fx/tone.sigmf-meta job start --kind measure --source tone \
    --center-hz 100000000 --bandwidth-hz 12500 --then-tail 1,5000

# measure on the APRS fixture: symbol_rate_baud 1200 in both records. The first second is labelled
# "fsk" (audio tones 1125 Hz and 2203 Hz under fm), the next "fm" (rms deviation 894 Hz); the
# packet does not fill the whole window, so the label depends on the second measured.
$Z $S --source-file aprs=scratch/fx/aprs.sigmf-meta job start --kind measure --source aprs \
    --center-hz 144800000 --bandwidth-hz 12500 --then-tail 2,5000

# classify: layer 1 "fm" (0.6) plus layer 2 "APRS" (1.0, "1 AX.25 frames with valid FCS; first VK3ABC>APRS")
$Z $S --source-file aprs=scratch/fx/aprs.sigmf-meta classify --source aprs \
    --start-ns 0 --end-ns 2000000000 --center-hz 144800000 --bandwidth-hz 12500
```

A broadcast station on a B210 (104.9 MHz, a node on the field host serving it; outputs trimmed):

```bash
N="--node http://field-host:7979"   # ZETA_SECRET in the environment
# audio frames at 48000 Hz, 786-787 samples each, peak 0.32-0.50
$Z $N job start --kind wfm_demod --source b210 --center-hz 104900000 --bandwidth-hz 200000
$Z $N stream list                        # the job's "audio" stream
$Z $N stream tail --stream <id> --max-frames 3 --timeout-ms 8000
$Z $N job stop --job <id>
# classify a 2 s span inside the ring's last 10 s: L1 "wfm" (0.9, "... 19 kHz pilot +16.2 dB ..."),
# L2 "FM Broadcast Radio" (1.0) and "CBET" (0.7, bandwidth off)
$Z $N classify --source b210 --center-hz 104900000 --bandwidth-hz 200000 --start-ns <t-2s> --end-ns <t>
# signatures: FM Broadcast Radio (87.5-108 MHz, 250 kHz, WFM), CBET, HD Radio (FM)
$Z $N knowledge lookup --region AU --center-hz 104900000 --bandwidth-hz 200000
```

### GPU

The FFT stage has a GPU implementation behind the `gpu` cargo feature (wgpu compute shader;
`cargo build -p zeta --features gpu`, tests with `cargo test -p zeta-dsp --features gpu`). With
the feature on, the node adds the GPU backend only when a hardware adapter is found, and the
scheduler then prefers it; without one the CPU FFT runs, so a missing GPU never fails a job.
`gpu_matches_cpu_within_0_1_db` must be run on a GPU host to count as verification (hardware equivalence is pending on the field host): elsewhere it prints `skipped: no adapter` and passes. Software adapters (lavapipe, llvmpipe) are ignored unless `ZETA_GPU_ALLOW_SOFTWARE=1`;
`software_adapter_allowed_by_env` sets it, so a GPU-less box with Mesa still runs the GPU path. A
GPU failure at run time is logged to stderr and that spectrum is computed on the CPU.

## Sources

Every command takes the source flags of the node it opens (`zeta serve` included); with `--node`
they belong to the serving node and are rejected. Source ids are unique across the source flags
(the two federation flags, `--source-node` and `--source-vrt`, are under [Federation](#federation)).
`source list` shows each source's capability record: tuning range, `max_bandwidth_hz`, formats,
`remote` and `push`, all `asserted` (taken from the device or the protocol, not measured).

- **File**, `--source-file ID=PATH.sigmf-meta`: a SigMF recording (`cf32_le`, `ci16_le` or `ci8`)
  replayed in real time and repeated.
- **KiwiSDR**, `--source-kiwi ID=HOST:PORT[/PASSWORD]`: 0-30 MHz, one 12 kHz IQ channel
  (`max_bandwidth_hz` 12000, so a wider job is refused `out_of_range` naming `bandwidth_hz`).
  `remote` and `push`: it connects on the first tune and holds one of the Kiwi's user slots
  while tuned, so a one-shot command holds the slot only while that process runs. The password
  is never printed, but like `--secret` it is visible in `ps` to every user of the machine;
  leave out `/PASSWORD` and set `ZETA_KIWI_PASSWORD_<ID>` instead (`ID` upper-cased, other
  characters as `_`, so `kiwi-0` reads `ZETA_KIWI_PASSWORD_KIWI_0`). The flag wins when both
  are given.
- **SoapySDR**, `--source-soapy ID=ARGS` (`ARGS` such as `driver=hackrf,serial=...`): a local
  device, `push` false (the node reads it). Gains are left at the driver's defaults (no gain
  operation yet), and a job centred exactly on the tuned frequency sees the device's DC spike.

```bash
./target/debug/zeta --source-kiwi kiwi0=kiwisdr.example:8073 source list   # remote true, current null
./target/debug/zeta --source-kiwi kiwi0=kiwisdr.example:8073 job start --kind measure \
  --source kiwi0 --center-hz 7100000 --bandwidth-hz 10000 --then-tail 2,5000
```

SoapySDR links libSoapySDR, so its driver (`crates/zeta-source-soapy-driver`, outside the
workspace) is behind a compiler cfg rather than a cargo feature: neither the default build nor
`--all-features` needs the headers. Without the cfg, `--source-soapy` and `zeta soapy find` exit
1 saying how to rebuild. With it:

```bash
sudo apt install libsoapysdr-dev soapysdr-module-hackrf
RUSTFLAGS="--cfg soapy" cargo build -p zeta --release
./target/release/zeta soapy find --filter driver=hackrf
./target/release/zeta --source-soapy hackrf0=driver=hackrf source list
```

Without the package, `DOCS_RS=1 RUSTFLAGS="--cfg soapy" cargo check -p zeta --all-targets`
type-checks that path (use a separate `CARGO_TARGET_DIR`; the flags rebuild everything). The
port logic is tested without hardware (`cargo test -p zeta-source-soapy --features test-mock`,
`cargo test -p zeta-source-kiwi --features test-fake`); the live tests are `--ignored`
(`cargo test -p zeta-source-kiwi --test live -- --ignored` needs a reachable Kiwi; the HackRF
test lives in the driver crate, see its README).

## External stages

`--plugin KIND=/abs/path` registers a program as stage kind `KIND`, usable in any graph
(`graph put`) like a built-in stage. The program is a separate process speaking plugin protocol 1
on its stdio (`crates/zeta-plugin/src/wire.rs` has the full definition):

- control messages are one line of JSON; data frames are a 0x00 byte, a length-prefixed JSON
  header (the websocket's frame message) and a length-prefixed body (`iq`: interleaved `f32` LE,
  SigMF `cf32_le`; `audio`: `f32` LE; `event` and `text`: no body);
- the plugin's first message is `hello` (kind, its one input stream kind, output stream kinds,
  params schema), within 5 s; a `hello` with more than one input kind is refused; the host
  answers `run` with the params and output stream ids, then streams frames, `lagged` and
  `closed`, and ends with `stop`. stderr is copied to the node's stderr prefixed `[plugin KIND]`.

The node probes every `--plugin` at startup (one-shot commands too, so each costs a process
start); a failed probe, or a `hello` whose kind differs from `KIND`, exits 1 naming the flag.
Each job run spawns a fresh process. Plugins are untrusted: the path must be absolute (no `PATH`
lookup, no shell), the process leads its own process group and the whole group is killed when
the stage ends, its output frames are checked against the run's output streams and get the run's
`source` and `job`, and its environment is cleared except `PATH`, `HOME`, `LANG`, `LC_ALL`,
`LC_CTYPE`, `TERM` and `PYTHONPATH` (plus `PYTHONUNBUFFERED=1`). To hand a plugin a variable,
set `ZETA_PLUGIN_ENV_<NAME>`; it arrives as `<NAME>`. Nothing else reaches it, `ZETA_SECRET`
included.

`sdk/python/zeta_plugin` is a standard-library-only Python SDK: a plugin calls
`zeta_plugin.run(kind, inputs, outputs, params_schema, on_frame)`. `examples/python/gain.py`
(iq in, iq out, every sample times `params.factor`) is the example:

```bash
A="--state-dir scratch/st --source-file tone=scratch/fx/tone.sigmf-meta"
ZETA_SECRET=correct-horse-battery ./target/debug/zeta $A \
  --plugin gain=$PWD/examples/python/gain.py serve --listen 127.0.0.1:7373

export ZETA_NODE=http://127.0.0.1:7373 ZETA_SECRET=correct-horse-battery   # second terminal
./target/debug/zeta job start --kind passthrough --source tone --center-hz 100000000 --bandwidth-hz 12500
echo '{"id":"gain-graph","nodes":[{"id":"g","kind":"gain","params":{"factor":0.5}}],"edges":[]}' \
  > scratch/gain-graph.json
./target/debug/zeta graph put --job <JOB> --file scratch/gain-graph.json
./target/debug/zeta stream list                     # the job's two iq streams: source and gain
./target/debug/zeta stream tail --stream <STREAM> --max-frames 3 --timeout-ms 3000
```

## Transports

Every transport is a thin adapter over the same `Node::apply`, so the CLI, HTTP, MCP and the
in-process agent run the same operations. Nothing is exposed without a shared secret.

`zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) also serves
an authenticated HTTP and websocket API. It needs a bearer secret, from `--secret` or
`ZETA_SECRET` (prefer the variable: `--secret` is visible in `ps`; secrets under 12 characters
draw a warning). Every request, `/health` included, must carry `Authorization: Bearer <secret>`,
else it gets 401.

| Route | |
|---|---|
| `GET /health` | status, version, uptime, websocket connections |
| `POST /op` | apply one operation (`{"op": ...}` JSON) |
| `GET /schema` | the operation schema |
| `GET /streams` | live streams |
| `GET /agents-md` | the node's agent guide |
| `GET /stream/{id}` | websocket of one stream |
| `GET /streams/{id}/tail?max_frames=N&timeout_ms=T` | the `stream_tail` operation: up to N frames arriving within T ms |

`POST /op` answers 504 if an operation takes longer than 30 s. `GET /streams/{id}/tail` applies
`stream_tail` and returns exactly what `POST /op` returns for it (`{"result":"frames","value":
{"frames":[...],"missed":N}}`, limits 1..=1000 frames and 1..=10000 ms), so a tail over HTTP is
bounded by the operation's own 10 s, well inside the 30 s.

The websocket sends text messages tagged by `type`: `{"type":"frame", ...}` carries one frame in
the same shape as a `stream_tail` frame (`stream`, `kind`, `source`, `job`, `channel`, `start_ns`,
`end_ns` and a `payload` summary; sample data does not cross the wire in this mode, see
[Federation](#federation) for `?format=binary`),
`{"type":"lagged","missed":N}` reports dropped frames and `{"type":"closed","reason":...}` ends
the stream.

`zeta serve --listen` runs in the foreground, so run the commands that follow in a second terminal.

```bash
A="--state-dir scratch/state3 --source-file tone=scratch/fx/tone.sigmf-meta"
ZETA_SECRET=correct-horse-battery ./target/debug/zeta $A serve --listen 127.0.0.1:7979

export ZETA_NODE=http://127.0.0.1:7979 ZETA_SECRET=correct-horse-battery
./target/debug/zeta job start --kind passthrough --source tone --center-hz 100000000 --bandwidth-hz 48000
./target/debug/zeta job list        # applied in the serving node; the job keeps running
curl -s -H "Authorization: Bearer $ZETA_SECRET" $ZETA_NODE/health
```

`--node URL --secret SECRET` (or `ZETA_NODE` and `ZETA_SECRET`) sends any CLI operation to a
running node instead of opening a local one. This is the way to change a node that `zeta serve`
owns. `--state-dir`, the `--source-*` flags and `--plugin` belong to the serving node and are
rejected.

`zeta-mcp` is a stdio MCP server with one tool per operation, generated from the schema, with
`AGENTS.md` as its instructions. Register it with Claude Code:

```bash
claude mcp add zeta -e ZETA_SECRET=… -- zeta-mcp --node http://127.0.0.1:7979
```

This needs `zeta-mcp` on `PATH` (for example `cargo install --path crates/zeta-mcp`), or give the
full path to `target/debug/zeta-mcp` in its place.

To give Claude Code the field guide as a skill, install `skills/zetasdr` (generated by
`zeta agents-md --skill`) into `~/.claude/skills/` or a project's `.claude/skills/`.

`zeta ask` lets a local model satisfy a request by calling the operations, against any
OpenAI-compatible endpoint (thinking off, token budget per turn, bounded turns, readiness wait):

```bash
./target/debug/zeta ask "what sources are there?" --llm-url http://127.0.0.1:8080 --llm-model gemma-4-12b
```

`zeta ask` reads its request from stdin, so a `local-voice` transcript can be piped in.

## UI

`zeta serve --listen` also serves a web app at `http://HOST:PORT/ui/` (`/` redirects there). It
is plain JavaScript embedded in the binary, a client of the same HTTP and websocket API as
everything else: no build step, nothing fetched from elsewhere. The page asks for the node's
secret once and keeps it in the tab's `sessionStorage`; a wrong secret shows "secret rejected"
and opens nothing, and a secret the node later rejects (a restart with a new one) returns to the
box.

```bash
ZETA_SECRET=correct-horse-battery ./target/debug/zeta --state-dir scratch/state6 \
    --source-file tone=scratch/fx/tone.sigmf-meta --source-file aprs=scratch/fx/aprs.sigmf-meta \
    serve --listen 127.0.0.1:7979 --llm-url http://127.0.0.1:8080 --llm-model gemma-4-12b
# then open http://127.0.0.1:7979/ui/ and paste the secret
```

The page picks its layout from the viewport width (`?footprint=wide|narrow` forces one):

- Wide (700 px and up): source tabs along the top, one per `source_list` entry; the waterfall
  for the selected source, newest column at the right, frequency vertical; on the side the
  transcript and the job list; along the bottom the command line (Ctrl-K) and the microphone.
- Narrow (under 700 px, a phone): the waterfall takes the screen. The transcript and job
  list move into a drawer behind ☰, and the agent's answers also appear as ribbons on the
  waterfall.

The waterfall is the UI's own `spectrum` job on the selected source (it reuses a running one,
and stops the one it started when the page closes). Overlays are pinned to their channel and
scroll with it: a `measure` job's layer-1 label with its confidence, an `aprs_decode` packet as
a ribbon (`VK3ABC > APRS: >hi` on the fixture), any other event as a short ribbon. The job list
shows every job that is not stopped, with a live event counter, ■ to stop it, ▶ on an
`fm_demod` row to play its audio in the browser (one job at a time) and `graph` to open the
job's graph view.

Drag across the waterfall to select a band and a time span; a context menu opens with:

| Entry | Operation |
|---|---|
| What is this? | `classify` on the selection; the labels are drawn at the selection and listed in the transcript |
| Decode › APRS | `job_start` `aprs_decode` on the selection's centre and bandwidth |
| Decode › NBFM audio | `job_start` `fm_demod` (deviation 5000 Hz) |
| Start background job › measure | `job_start` `measure` |
| Save selection as SigMF | `iq_read` on the selection; the transcript shows the capture path |
| Send to agent | `POST /ask` with "Look at C Hz, BW Hz wide, from START to END: what is it?" |

These are the same operations an agent, the CLI or MCP would send, through the same
`Node::apply`. The menu's footer shows the selection in the operations' own terms
(`center_hz=… bandwidth_hz=… start_ns=… end_ns=…`); the span is clamped to the newest 8 s so it
stays inside the source ring.

The transcript is fed by the node's `ops` stream: one event per applied operation, from any
client (the page, the CLI, MCP, the agent), with its outcome and elapsed time, so a call the
agent makes shows there verbatim, as `job_start kind=measure source=tone center_hz=… → job …`,
and an error shows its corrective detail. The page polls `job_list` every 2 s and the waterfall
looks up sources and streams, so successful `job_list`, `stream_list` and `source_list` lines
are hidden unless you typed that operation yourself (the agent's own list calls are hidden too,
since the stream does not say who sent them); errors always show. The `ops` stream is an
ordinary stream (`stream list` shows it with provenance source `node`), so a script can follow
it too.

The command line takes either an operation as JSON (`{"op":"source_list"}`, applied with
`POST /op`) or free text, which goes to the agent. That needs `--llm-url` and `--llm-model` on
`serve` (the other `--llm-*` flags of `zeta ask` apply), which enable `POST /ask`: the node runs
the `zeta ask` agent against itself, so its calls appear in the transcript as they happen, and
the answer follows when the run ends (one run at a time; 503 without `--llm-url`, 504 after
300 s). The microphone records in the browser and sends a 16 kHz WAV to `POST /stt`, which hands
it to a `local-voice` speech daemon on the node's host at `--stt-socket PATH` (default
`$XDG_RUNTIME_DIR/voiced.sock` when that exists); the transcript is submitted as if typed.
Without a daemon the button says "no speech daemon on this node". Browsers allow the
microphone only on a secure origin: `localhost` qualifies, a LAN address over plain HTTP does
not.

Clicking `graph` on a job row draws its graph document (stages as boxes, streams as labelled
edges). Clicking a stage shows its `params` as JSON; editing them and pressing Apply sends
`graph_put` with that stage changed, which restarts the job under the same id.

Only `/` and `/ui/*` (the static files, which hold no secret) are served without the secret.
Everything else still needs `Authorization: Bearer <secret>`. A browser cannot set that header on
a websocket, so `/stream/{id}` also accepts the secret as a subprotocol: offer
`["zeta", "zeta.bearer." + base64url(secret)]` (base64url without padding). This works for any
websocket client:

```bash
python3 - <<'PY'
import base64, json, os, urllib.request
from websockets.sync.client import connect   # pip install websockets
node, secret = "127.0.0.1:7979", os.environ["ZETA_SECRET"]
req = urllib.request.Request(f"http://{node}/streams", headers={"Authorization": "Bearer " + secret})
ops = next(s["id"] for s in json.load(urllib.request.urlopen(req)) if s["provenance"]["source"] == "node")
proto = "zeta.bearer." + base64.urlsafe_b64encode(secret.encode()).decode().rstrip("=")
with connect(f"ws://{node}/stream/{ops}", subprotocols=["zeta", proto]) as ws:
    for msg in ws:
        print(msg)                                 # one {"type":"frame",...} per operation
PY
```

## Federation

A node can use a source of another node. `--source-node ID=SOURCE@URL` (URL is
`http://HOST:PORT` of the other node's `serve --listen`) registers source `SOURCE` of that node
as local source `ID`. The other node's secret comes from `ZETA_NODE_SECRET_<ID>` (`ID`
upper-cased, other characters as `_`, so `pi-0` reads `ZETA_NODE_SECRET_PI_0`); there is no flag
for it, so it never shows in `ps`. The source is `remote` and `push`, with the remote's
capabilities except `max_bandwidth_hz`, which is capped at 250 kHz so that a node on WiFi ships
narrowband by default. Append `,max_bandwidth_hz=N` (1-20000000) to widen it; the remote's own
limit still applies, so a 48 kHz file source stays 48 kHz. The cap also holds for the rate the
remote actually ships: a remote that cannot go that low (a HackRF's minimum is 1 Msps) is stopped
again and the tune fails `out_of_range`, saying the rate it ships and the `max_bandwidth_hz` that
would admit it.

Each tune starts a `passthrough` job on the remote source, centred and as wide as the local
request, and subscribes to its IQ stream; layer 1 runs on the local node, the remote only
channelises. A retune to another band stops the previous remote job first (a tune to the same
band while the stream is live reuses it), and the remote job holds the remote source's lease, so
the remote's `source list` shows the `passthrough` job as a holder. When the last local job on
the source stops, the remote job is stopped after a 2 s grace, so a job restarted within it
keeps the same remote stream. On exit (Ctrl-C or SIGTERM on `serve`, so `systemctl stop` too, or
the end of a one-shot command) the remote job is stopped and the exit waits for the remote node
to confirm. Packets lost in transit and `lagged` notices are counted and logged to stderr at most
every 10 s (`zeta-source-node: ID: N packets lost, M lag events`).

```bash
A="--state-dir scratch/A --source-file tone=scratch/fx/tone.sigmf-meta"
ZETA_SECRET=correct-horse-battery ./target/debug/zeta $A serve --listen 127.0.0.1:7979

# second terminal: node B uses node A's tone as its source a
ZETA_SECRET=correct-horse-battery ZETA_NODE_SECRET_A=correct-horse-battery ./target/debug/zeta \
    --state-dir scratch/B --source-node a=tone@http://127.0.0.1:7979 serve --listen 127.0.0.1:7980

# third terminal
export ZETA_NODE=http://127.0.0.1:7980 ZETA_SECRET=correct-horse-battery
./target/debug/zeta source list       # a: remote true, push true, max_bandwidth_hz 48000 (the file's rate)
./target/debug/zeta job start --kind measure --source a --center-hz 100000000 --bandwidth-hz 12500
./target/debug/zeta stream list       # the job's event stream; `stream tail` shows "carrier" records
./target/debug/zeta --node http://127.0.0.1:7979 source list   # tone held by a passthrough job
```

What crosses the wire is the plan-3 websocket in binary mode, `GET /stream/{id}?format=binary`
(the default `format=text` is unchanged). IQ goes as VITA 49.2 signal-data packets (`ci16`
samples, at most 16 384 per packet) with a context packet first and then about once a second
carrying the RF centre, sample rate and bandwidth; each packet is one binary websocket message.
Audio, text and events go as the typed envelope, the plugin wire's data-frame framing
(`u32LE header_len | frame summary JSON | u32LE body_len | body`, the body `f32` LE PCM for
audio, UTF-8 for text, empty for events). The server sends nothing until the client grants
credit with a text message `{"type":"credit","packets":N}` (N in 1..=4096; at most 65 536
outstanding); each binary message spends one. A frame that finds no credit is skipped, not
queued, and the count goes out as `{"type":"lagged","missed":K}` before the next frame sent.
`--source-node` grants 256 at open and 128 more every 128 messages consumed.

`--source-vrt ID=udp://ADDR:PORT` receives VITA 49.2 packets on UDP, one packet per datagram,
from any sender that speaks it (the packets `--source-node` receives are the same encoding).
`ADDR` must be a LAN address (`0.0.0.0` listens on every interface). The sender sets the
frequency and rate from its context packets, so a job's band must lie inside the band last
announced; before the first context packet a tune waits up to 1.5 s for one. The port follows
one sender: the stream id of the first context packet is pinned, packets of another stream id
are dropped once data has arrived, and so is a data packet whose timestamp runs backwards (a
duplicated or reordered datagram); both are counted. The `--source-vrt` ingest is
unauthenticated: anyone who can reach the port can feed it, so bind a specific LAN address
rather than `0.0.0.0`, on a trusted segment.

A Raspberry Pi 5 at the antenna is the intended remote: [deploy/pi/README.md](deploy/pi/README.md)
cross-builds `zeta` for aarch64, smoke-tests it under qemu and installs it as a systemd service
that refuses to start without a real secret. The laptop then federates it with `--source-node`.

One caveat: in a two-host test against a USRP B210, the one-shot form (`job start ... --then-tail`) on a remote source streamed
for 10 s but produced no events in two runs while the same job under `serve` did; on one machine
with a file source the one-shot form works. Prefer a federating `serve` until that is resolved.
