# ZetaSDR guide

The reference for running a ZetaSDR node: building it, every CLI command with worked examples,
the signal processing, the sources, decoders, transports, the web page and federation. The
[README](../README.md) is the short introduction; [AGENTS.md](../AGENTS.md) lists every operation
with its fields, and [docs/README.md](README.md) indexes the rest.

Conventions in this guide:

- Run the commands from the repository root. `scratch/` is a working directory for state and
  fixtures (git ignores it); the fixtures are written by the `gen_tone`, `gen_aprs` and
  `gen_pocsag` examples, as shown where they are first used.
- `field-host` stands for the maintainer's node with real radios (a USRP B210 and a HackRF), reached
  with `--node http://field-host:7979`; put your own node's address there. Measured values in those
  examples come from that node and its antenna.
- Layer 1 is measurement (occupied band, SNR, burst timing, symbol rate, a modulation label);
  layer 2 is knowledge: catalogue signatures, the licence register and decode-to-identify, where a
  CRC-clean decode proves a protocol; layer 3 is reserved for a trained classifier and is empty;
  layer 4 is working an unknown signal's bits by hand.

Contents:

- [Building and testing](#building-and-testing)
- [CLI walkthrough](#cli-walkthrough)
- [DSP](#dsp) ([GPU](#gpu))
- [Speech](#speech)
- [Sources](#sources)
- [Decoder modes](#decoder-modes)
- [Exploring unknown signals](#exploring-unknown-signals)
- [External stages](#external-stages)
- [Transports](#transports)
- [UI](#ui)
- [Wideband](#wideband)
- [Federation](#federation) ([Reading a source's health](#reading-a-sources-health),
  [A radio that comes back](#a-radio-that-comes-back))

## Building and testing

The toolchain is pinned by `rust-toolchain.toml` (1.97); install `rustfmt` and `clippy` for it
(`rustup component add rustfmt clippy --toolchain 1.97`), then:

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

The dev profile optimises `rustfft`, `num-complex`, `zeta-dsp` and `nnnoiseless` (see `Cargo.toml`):
the tests run sources in real time, and an unoptimised demodulate-and-transcribe chain falls behind
on a loaded host. A profile change rebuilds every dependant, and `target/debug` grows to 20-80 GB
across the workspace's test binaries: delete it when the disk fills (`target/release` is small and
worth keeping). With one `CARGO_TARGET_DIR` shared between worktrees cargo can serve a stale artifact
of a path crate; `touch crates/*/src/lib.rs` forces the rebuild.

## CLI walkthrough

One-shot commands open the node, apply one operation, persist and exit; they do not resume jobs.
`zeta serve` resumes persisted jobs and keeps them running until Ctrl-C. A one-shot `iq read` or
`classify` waits (up to 5 s) for the source ring to cover the requested span. File sources play in
real time and loop. `--source-file` is not persisted: pass it on every invocation.

While `zeta serve` runs it holds `serve.lock` (its pid) in the state directory, and the mutating
one-shot commands (`job start`, `job attach`, `job retune`, `job stop`, `graph put`, `source ensure`) exit 2 with
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
./target/debug/zeta $A job retune --job <id> --center-hz 100010000 --bandwidth-hz 10000   # same id; attached jobs follow
./target/debug/zeta $A job stop --job <id from job start>
./target/debug/zeta agents-md            # the agent guide, rendered from the operation schema
```

Taking a source from other jobs, and the node's ops log:

```bash
./target/debug/zeta job start --kind adsb_decode --source b210 --center-hz 1090000000 \
    --bandwidth-hz 2400000 --replace job-1,job-2   # stops the named holders, then starts (under the gate)
./target/debug/zeta job start --kind spectrum --source b210 --center-hz 100000000 \
    --bandwidth-hz 2000000 --elastic               # never conflicts, follows retunes (spectrum only)
./target/debug/zeta source ensure --source b210 --center-hz 100000000 --bandwidth-hz 2000000 --replace job-1
./target/debug/zeta ops tail --lines 20            # last applied ops (default 50), local state dir only
```

A `conflict` error names the holders and offers `retry with replace: [<ids>]`; `--replace` ids
must hold that source as rigid leases; naming an elastic job is the error `job <id> does not hold source <src>`, nothing is stopped and it keeps running. Replace is not transactional: if a holder you did not name still conflicts, the jobs you named stay stopped. Elastic jobs are listed under `elastic` in `source list`.
Every applied op is appended as one JSON line (the op's payload plus `t_ns`) to `<state-dir>/ops.log`
(a read-only op that succeeds, such as `source list` or `stream tail`, without its result),
rotated to `ops.log.1` past 8 MiB; `ops tail` reads it and is a usage error with `--node`. A restart
restores a source's capture (the band bounding its rigid jobs, when the radio can capture it),
resumes its jobs a scan first, then widest first, elastic after rigid, and re-expands a built-in graph if the
capture rate differs from the one recorded (`capture_rate_hz`).

Holding a capture, receive gain and transcripts:

```bash
./target/debug/zeta source pin --source b210 --center-hz 476918750 --bandwidth-hz 1000000 --replace job-1
./target/debug/zeta source unpin --source b210
./target/debug/zeta source gain --source b210 --db 45     # or --auto
./target/debug/zeta transcripts tail --lines 20 --job job-7   # default 50 lines, local state dir only
./target/debug/zeta events tail --lines 200 --protocol acars  # every job's events, local state dir only
./target/debug/zeta plans [--plan uhf_cb_au]     # channel plans with their channels expanded
./target/debug/zeta jobs stop --source b210   # every job on a source in one operation (no filter: every job; the page's spectrum stays)
./target/debug/zeta plan start --plan uhf_cb_au --kind fm_demod --source b210 --channels 1,2,5-8 --attach transcribe   # one lease decision, one job per channel
./target/debug/zeta mix start [--sources b210,hackrf]   # one audio stream of every demodulator (no lease, no transcript)
```

`source pin` holds a source at a band (the leased bandwidth may round up to what the radio can
capture) as a rigid lease `pin:<source id>` that is never a holder and is not named in `--replace`.
Jobs inside the band run without a retune; a start outside it is a `conflict` whose options include
`unpin source <id> (source_unpin)` (and "stop one of the holders" only when jobs hold the source).
The pin is saved in `<state-dir>/sources/<id>.json` and restored first on resume. One-shot
commands, read-only ones included, restore the pins and the gain of the sources they attach, so
even a one-shot `source list` tunes a pinned radio. `source unpin` on an unpinned source is an
error. `source gain` (the op `source_set_gain { source, gain_db: number | null }`) sets the
receive gain in dB within the source's range (`source_list.gain` reports `db`, `min_db`,
`max_db`, `automatic`, `automatic_available`); `--auto` (`gain_db: null`) turns automatic gain
on where the device has it. The setting is saved in the source record (`sources/<id>.json`,
beside the pin) and re-applied at the next start, where it wins over `gain=` in the device args. A resumed channel takes the shared bank (`job_context` names its `channel.path`;
`source_list.bank` shows `bin_hz` and `consumers`). Every transcript event of an attached
`transcribe` job is also appended as one JSON line to `<state-dir>/transcripts.log` (`t_ns`, `job`,
`input`, `source`, `center_hz`, `bandwidth_hz`, `start_ns`, `end_ns`, `text`, `language`,
`dropped`, `error`; rotated to `transcripts.log.1` past 8 MiB); `transcripts tail` reads it and is a
usage error with `--node`. Every event of every job on a source (what the source's feed carries:
every decoded frame, detection or transcript, whether or not a page or a `stream tail` is open; an
`fft`'s rows stay out) is appended as one JSON line to `<state-dir>/events.log` (`t_ns`, `source`,
`job`, `channel`, `center_hz`, `bandwidth_hz`, `start_ns`, `end_ns` and the event itself under
`event`; rotated to `events.log.1` past 32 MiB); `events tail [--job ID] [--source ID]
[--protocol NAME]` reads it and is a usage error with `--node`.

`fm_demod` shapes its audio as a voice receiver does: a 3.4 kHz low-pass and a 530 µs
de-emphasis after the discriminator (`audio_lowpass_hz`, `deemphasis_us` on its node; `null`
keeps the flat audio the audio-input decoder rows read). Every voice demodulator (`fm_demod`,
`am_demod`, `ssb_demod`) takes `denoise: true` on its node to run RNNoise on its audio output
(off by default: a speech model; one 10 ms frame of delay the frames' spans account for), set
with `graph put` or the page's outline menu ("Denoise on/off") and kept through a retune; a
`transcribe` attached to such a job runs without a `denoise` stage of its own.

`fm_demod` and `am_demod` carry an automatic squelch, always on: it opens 6 dB over the noise
floor it tracks (the automatic floor is never taken above -40 dBFS) and closes after 300 ms under
it. While closed the stage publishes no audio; it emits a `squelch` event (`open`, `power_db`,
`floor_db`) on the first block and on each transition. `--then-tail` tails the audio output
first, so an idle squelched demodulator gives an empty tail. To fix the threshold or turn it
off, edit the stage's params with `graph get`, `graph put`: `squelch_db` is an absolute threshold
in dBFS, -120..=0 (`null`, the default, is automatic; a value outside the range fails the job
with `squelch_db <v> out of range -120..=0`), and `squelch: false` turns it off. `aprs_decode`'s
demodulator runs with the squelch off. A put that changes only these (or `deviation_hz`,
`sideband`) keeps the job re-expandable for a later capture rate; any other `graph put` is
launched as stored from then on.

Channel plans. `zeta plans` (op `channel_plans`) lists the plans the node knows; `uhf_cb_au` is
Australian UHF CB, 80 channels on two rasters 12.5 kHz apart (the 40 channels of 25 kHz spacing
and the 40 interleaved ones, ch41-80, between them). An `fm_demod` whose centre is within 1 kHz of a
plan channel gets an adjacent-channel guard: its channel is widened to 37.5 kHz, the centre is
filtered in the stage, and its squelch stays closed while a channel `guard_hz` (12.5 kHz) away is
more than `guard_db` (12) stronger; the `squelch` event adds `adjacent_db` and `guarded`, and a
`graph put` with `guard_hz: null` on the demod turns it off. A job off every plan is unchanged.
The page tags plan channels (`ch20`) on job rows and on the rail (on the outlines in the plain theme) and numbers them on the axis when
zoomed; `view set plan auto|<id>|none` picks the plan. `transcribe` drops utterances shorter than
`min_utterance_ms` (default 700, counted in the job context as `transcribe.short`), and the speech
daemon queue holds 4 requests: a transcript event carries `queued_ms`, and a dropped one a `reason`
(`queue` or `busy`).

`examples/python/cb_watch.py` (standard library only) runs demodulators only on the channels in
use: one elastic `spectrum` job on the source, a channel whose peak is `--open-db` (10) over the
row's floor gets `fm_demod` plus `transcribe`, up to `--max-tuners` (8), and both stop after
`--idle-s` (60) quiet seconds or at SIGINT/SIGTERM. `--dry-run` lists the channels inside the
capture and starts nothing:

```bash
ZETA_SECRET=... python3 -I examples/python/cb_watch.py --node http://field-host:7979 \
  --secret-env ZETA_SECRET --source b210 --dry-run
```

[RECIPES.md](../RECIPES.md) holds the named job setups (airband data links as a set, the UHF CB plan with transcribers, the sonde watch) as the JSON ops to send in order, for an operator or an agent to copy rather than re-derive. [AGENTS.md](../AGENTS.md) is generated from the operation schema by `zeta agents-md`; `zeta agents-md --check`
fails if the committed file has drifted.

## DSP

Sixteen built-in job kinds (`transcribe` is attached with `job attach`, not started; `identify` and `watch` are described under [Decoder modes](#decoder-modes); mode rows add more), chosen with `job start --kind` (`zeta job start --help` lists them):

- `passthrough` copies the channel IQ through unchanged.
- `spectrum` emits spectrum rows at 20 per second of signal time, each the power average of every FFT since the previous row (spectrum event v2): `size` (the smallest power of two at least `bandwidth_hz / 6000`, 1024 to 8192), `bin_hz`, `rows_per_s`, `center_hz`, and the row quantised as `q` (one 0-255 level per bin, lowest frequency first) with `floor_db` and `step_db` (0.5): `db = floor_db + q * step_db`. `floor_db` is the row's 2nd-percentile bin minus 5 dB, so levels more than 127.5 dB above it clip at 255.
- `fm_demod` channelises and demodulates narrowband FM to an audio stream (48 kHz, per-frame `peak`); `--deviation-hz` sets the peak deviation (default 5000).
- `wfm_demod` does the same for wideband broadcast FM: `--deviation-hz` default 75000, 50 µs de-emphasis (the stage also takes 75 µs, set in its graph with `graph put`), 15 kHz mono audio at 48 kHz.
- `am_demod` does the same for AM voice (envelope detector, 5 kHz audio at 48 kHz, slow AGC to a steady level; give it about 10 kHz of band).
- `ssb_demod` does the same for SSB voice (`--sideband usb|lsb`, default `usb`; 2.8 kHz audio at 48 kHz, slow AGC). `--center-hz` is the suppressed-carrier (dial) frequency; give it about 6 kHz of band so both sides of the carrier are kept.
- `aprs_decode` demodulates AFSK 1200 and HDLC-deframes AX.25, emitting one event per CRC-valid packet.
- `pocsag_decode` decodes POCSAG pagers (512, 1200 and 2400 baud, detected automatically) from a 12.5 kHz FM channel: one event per message (`address`, `function`, `kind`, `text`).
- `rds_decode` reads the RDS data of a broadcast FM station from its 200 kHz band: events carrying `pi`, `ps` and `rt` as they complete.
- `adsb_decode` decodes Mode S / ADS-B at 1 090 MHz from a band of 2.4 MHz (under 2 MHz is refused): one event per message (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`, `track_deg`). It runs dump1090 on the node's host (crate `zeta-ext`: GPL, so an external process, never linked; `ZETA_DUMP1090` overrides the program path); the job fails naming dump1090 when the host has none.
- `acars_decode` decodes ACARS (AM, 2400 baud MSK) from a 10 kHz channel: one event per CRC-clean block (`reg`, `flight`, `label`, `text`).
- `measure` emits one layer-1 feature record per second of IQ (occupied band, SNR, burst, symbol rate, label).
- `denoise` removes broadband noise from audio with RNNoise (the pure-Rust `nnnoiseless` crate, BSD-3-Clause, weights embedded); it downmixes to mono and resamples to 48 kHz.

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
measurements it used. `wfm` at 0.9 is a band 40-250 kHz wide with a 19 kHz pilot line at least 10 dB
above the surrounding audio spectrum, quoted ("19 kHz pilot +16.3 dB"), whatever the envelope
(multipath ripples a real station's); without a pilot, `wfm` at 0.6 is a band 60-250 kHz wide with
cv below 0.45 and rms deviation above 5 kHz. `am` is cv at least 0.25 with a carrier line at the
centre, or, for low-index AM, cv at least 0.1 (also within the occupied band) with that line and
less rms phase deviation (rad per sample) than cv; `ssb` is cv at least 0.25, no carrier line, a
1.5-4 kHz band and 80 % of its power on one side of the centre, and its evidence gives the dial
frequency to hand to `ssb_demod`. `fm` and `carrier` remain the narrowband rules (band at most 25
kHz, cv below 0.2), checked after these.

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
cargo run -q -p zeta-dsp --features test-fixtures --example gen_pocsag -- scratch/fx
cargo run -q -p zeta-source-file --features test-fixtures --example gen_tone -- scratch/fx
Z=./target/debug/zeta
S="--state-dir scratch/state"

# APRS: one event, src VK3ABC (dst APRS, info ">hi", fcs_ok true)
$Z $S --source-file aprs=scratch/fx/aprs.sigmf-meta job start --kind aprs_decode --source aprs \
    --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000

# POCSAG: one event, address 1234567, function 3, kind alpha, text "HELLO ZETA", baud 1200
$Z $S --source-file pocsag=scratch/fx/pocsag.sigmf-meta job start --kind pocsag_decode --source pocsag \
    --center-hz 148637500 --bandwidth-hz 12500 --then-tail 1,5000

# NBFM audio from the tone: "kind": "audio" frames, 4800 samples at 48000 Hz each. The first
# frame's peak is a filter start-up transient (1.07); later frames are 0.40 (between 0.3 and 1.0).
$Z $S --source-file tone=scratch/fx/tone.sigmf-meta job start --kind fm_demod --source tone \
    --center-hz 100000000 --bandwidth-hz 12500 --then-tail 5,3000

# spectrum: one event, "size": 1024, "bin_hz": 46.875, "rows_per_s": 20, "floor_db": -190.63,
# "step_db": 0.5, "q": [18, 21, 15, ...] (the tone clips at 255 in the middle bins)
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
`gpu_matches_cpu_within_0_1_db` must be run on a GPU host to count as verification (no GPU host has run it yet): elsewhere it prints `skipped: no adapter` and passes. Software adapters (lavapipe, llvmpipe) are ignored unless `ZETA_GPU_ALLOW_SOFTWARE=1`;
`software_adapter_allowed_by_env` sets it, so a GPU-less box with Mesa still runs the GPU path. A
GPU failure at run time is logged to stderr and that spectrum is computed on the CPU.

## Speech

Transcription of a radio channel. The crate `zeta-speech` holds the speech backends
(`SocketBackend` for a daemon on a Unix socket such as `local-voice`'s `voiced`, `NodeBackend`
for another node's `POST /stt`) and the `transcribe` stage; `zeta-dsp` holds `denoise` and
`vad`; `zeta-core` holds `stream_in`. The stages `stream_in`, `denoise`, `vad` and `transcribe`
form the chain of an **attached job**, which reads another job's audio instead of the radio:

```bash
Z=./target/debug/zeta
$Z --node http://field-host:7979 job start --kind wfm_demod --source b210 --center-hz 100500000 --bandwidth-hz 200000
$Z --node http://field-host:7979 job attach --kind transcribe --job <wfm job id>
$Z --node http://field-host:7979 stream list    # the attached job's two event streams
```

`zeta job attach --kind transcribe --job ID [--language CODE]` creates the attached job; the
node needs a speech backend, chosen with the global flags `--stt-socket PATH` (default
`$XDG_RUNTIME_DIR/voiced.sock` when it exists) or `--stt-node URL` (secret in
`ZETA_STT_NODE_SECRET`), which are exclusive. `denoise` is RNNoise through `nnnoiseless`
(BSD-3-Clause, weights embedded). Source separation, for music under speech, is a slot filled
by an external audio plugin, for instance a Demucs wrapper on a GPU host:
`examples/python/separate_stub.py` shows the contract and [docs/speech.md](speech.md#separation) how to
swap it into the chain. [docs/speech.md](speech.md) also covers installing `local-voice` on
a node's host, writing your own daemon and sharing one daemon between nodes.

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
  are given. The Kiwi's own position, from its status when it has GPS, is the vantage of
  `origin_lookup`; `--source-position ID=LAT,LON[,ALT_M]` says where a source is when it does not
  report one (one that does overrides it), so a Kiwi's lookups are cut to its own reach, not the
  station's.
- **SoapySDR**, `--source-soapy ID=ARGS` (`ARGS` such as `driver=hackrf,serial=...`): a local
  device, `push` false (the node reads it). Gain starts at the driver's default and is changed
  live with `source gain` (below); `gain=DB` in `ARGS` sets the opening gain (a `DB` outside the
  device's range refuses to attach the source, exit 1; a gain set with `source gain` is persisted
  and wins over `gain=` at the next start), and `ppm=N`
  (e.g. `driver=uhd,ppm=3.4`) corrects the device's frequency error by shifting the tuning (the
  frequencies you read stay true). A job centred exactly on the tuned frequency sees the device's
  DC spike.
  The driver runs in a `zeta soapy-worker` child the node spawns (the same executable), so a driver
  crash or a pulled cable faults the source (`fault` in `source list`, jobs on it fail with
  `source <id>: <fault>`) and never ends the node; the node respawns the worker, and when the device
  is back it restores the gain and the pin and relaunches the jobs that failed with the fault, the
  `watch` and its decoders included. `source list` shows the child as `worker` (`pid`, `restarts`,
  `since_ns`, `transport`, `transport_dropped`). A device absent at startup, or whose worker
  crashes opening it (UHD still releasing it from a node that just stopped), attaches faulted
  when an earlier start saw it, else the
  flag is refused. A one-shot command that attaches a Soapy source starts a worker (and a UHD open)
  like it once opened the device in-process; `--soapy-in-process` keeps that path for diagnosing the
  worker (ignored with `--node`).
  A UHD device (`driver=uhd` in `ARGS`, or a driver key such as `b200`) reports its rate range for its current master clock (a B210 opens
  at 16 MHz), so at open the source asks for twice the reported maximum, re-reads the range
  while it grows (at most three rounds) and restores the previous rate: a B210 asserts
  `max_bandwidth_hz` 61440000 and a wide request raises its clock. `master_clock_rate=` in
  `ARGS` (e.g. `driver=uhd,type=b200,master_clock_rate=30.72e6`) is an explicit override: it
  fixes the clock, and with it the range and the rates a request can get. Rates: the source asks
  for the smallest exact divisor of the master clock (`mcr / N`, `N` up to 512) at least the
  requested bandwidth, so the rate is an exact fraction of the clock; an actual rate below the
  band is retried once at the next rate up, then refused (`soapy: device gives <actual> Hz for
  <requested> Hz, below the <bandwidth_hz> Hz band`). The resampler is exact up to a reduced
  ratio of 2 048 and above that snaps the input rate to the nearest one within 1 000 ppm that
  reduces under it (one stderr line per snap); past that it refuses.

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
test lives in the driver crate, see [its README](../crates/zeta-source-soapy-driver/README.md)).

## Decoder modes

A decoder is a data row, not code. The built-in rows live in `modes/*.json` (embedded in the binary); your own go in `--modes-dir PATH` (default `$XDG_CONFIG_HOME/zetasdr/modes`, else `~/.config/zetasdr/modes`), and a row with a built-in `id` replaces it. A row that does not parse or validate is listed `broken` (with its `file` and the offending field) and logged at startup as `zeta: mode <id> (<file>): <field>: <error>`; it never stops the node.

Themes follow the same pattern: `--themes-dir PATH` (default `$XDG_CONFIG_HOME/zetasdr/themes`, else `~/.config/zetasdr/themes`), see [docs/themes.md](themes.md).

```bash
zeta modes list [--family F] [--state ready|broken|unavailable]   # every row, with its state
zeta modes show pocsag_decode                                      # one row in full
zeta modes check [ID] [--strict]                                   # run each row's test on a synthesised capture
zeta job start --kind <mode id> --source S --center-hz C --bandwidth-hz B
```

`job start --kind` takes any ready row's id. A program row (`adsb_decode` runs dump1090) is `unavailable` when its program is missing; the startup handler report says so, `zeta: handlers: dump1090: missing (apt install dump1090-fa; <url>) — modes: adsb_decode`, and install it with the command shown (zeta never fetches or builds programs; `ZETA_HANDLER_<PROGRAM>` names one outside `PATH`). `zeta modes check` prints `<id>: ok`, `fail: <reason>` or `skipped: <reason>` per row (`--strict` fails a skip), is local only and is refused while `zeta serve` owns the state directory.

Every decoded event carries `protocol`, `t_ns` and a one-line `summary` (the page shows it). `zeta classify` is the quick answer to "what is this": it tries the best five rows with a short decode and lists them under `tried`. `zeta job start --kind identify` is the thorough one: it measures the band, then runs each fitting row as a child job in score order, publishing a `candidate` event for each, and ends with a verdict in `job_context.identify`; a proof leaves that decoder running. [Field craft: Decoding modes](agents/field-craft.md#decoding-modes) has the details and a worked operator row.

`video_decode` (family `tv`) decodes analog video (wide FM, composite sync) into a live picture: the page shows a thumbnail in the job's tag and a Video tab with the full view (`view set video smooth|sharp`), and `zeta stream save --stream ID --path still.png --node URL` saves the next frame to arrive as a PNG. A digital video link never locks and is reported not yet decodable.

A program row can also take demodulated audio instead of IQ (`decoder.demod`, `input.container` `raw` or `wav`): the radiosonde rows `rs41_decode`, `dfm_decode` and `m20_decode` run the rs1729 decoders (package `sonde-decoders`) over an NBFM channel anywhere in 400.05-406 MHz, and their events carry a `position`. `zeta source free --center-hz C --bandwidth-hz B` says which sources could take a band now (a suggestion, not a reservation). `zeta job start --kind watch` holds a band (400-406 MHz for sondes), reports each new signal that persists as a `signal` event, runs `identify` on it and keeps the decoder that proves, in its own capture or on a free second radio (`decode_on: free`); stopping the watch stops what it started, and [Field craft: Watching a band for sondes](agents/field-craft.md#watching-a-band-for-sondes) walks through it. The web page outlines a watch on the waterfall and has a Map tab beside the radios (`view set main map|rf`, `view set map on|off` the alias; `view set home LAT,LON` for distance and bearing; `view set dock right` moves the selected pane beside it) for events with a position, a marker and track per subject for a row that names one and per job otherwise: Leaflet 1.9.4 is vendored, and tiles are fetched only when the browser is online (`view set map_tiles` picks among OpenStreetMap, OpenStreetMap dark, OpenTopoMap, Esri street map and Esri imagery, all keyless, and the map says so when its tiles do not load). Lit register sites share the map: brightness is score times freshness, a dashed ring disagrees with the measured modulation, a confirmed site has its own ring; each is consistent with the signal, not an identification. The Subjects tab (`view set subjects auto|on|off`) lists a row's subjects, one line per aircraft or sonde; a click there selects the marker and the reverse. Subjects live on the page, so a refresh refills from live traffic. A `sink_sondehub` stage, added by the watch's `sondehub` param (a callsign and your position) or by `graph put`, posts sonde telemetry to SondeHub in batches; nothing is uploaded otherwise.

Decoder wave 2 (plan 29) added rows on the same frameworks. `ais_decode` (161.975 and 162.025 MHz, GMSK 9600, in-crate, vessels as subjects with positions on the map; `plan_start` with `ais_vhf` starts both channels) and `m17_decode` (4FSK 4800 baud from IQ: link setup frame and LICH, two events per stream, and Codec 2 speech decoded by the external `c2dec` as ordinary job audio, so it plays on the page and `job attach transcribe` works on it; an encrypted stream is never fed to the vocoder) are in-crate stages; `flex_decode` (FLEX 1600 bps pagers) runs `multimon-ng` as a handler, and `dfm_decode` and `m20_decode` have synthesised captures (the DFM test is not set yet, see `TODO.md`). APRS now carries a `position` (uncompressed, compressed and Mic-E) with its callsign as the subject, so stations get markers and tracks. A row's `test.expect` accepts `{near, within}` tolerances for numbers; a graph row can name `handlers` (external programs the graph needs, probed like a program row's, so the row is `unavailable` when one is missing); a row's `outputs` says whether it produces `audio` (the page offers play and Transcribe on such a job); and a row can carry `access: "restricted"`, today on the POCSAG and FLEX rows, which marks the events for a later access control and changes nothing now. Pager text is decoded and stored unfiltered; whether you may receive it is your local law (see the README's Legal section). `zeta modes check --strict flex_decode m17_decode` needs `multimon-ng` and `codec2` installed (CI installs both). The rendered row notes are in AGENTS.md under `## Mode rows`.

## Exploring unknown signals

When nothing proves a signal, the answer is `unknown` with its measured features, and the node has
the tools to look further (field craft: [Exploring an unknown signal](agents/field-craft.md#exploring-an-unknown-signal)
and [Working layer 4](agents/field-craft.md#working-layer-4)).

```bash
zeta station set --lat -27.38 --lon 153.12 --alt-m 4           # once: position, region, antennas
zeta --knowledge-acma http://HOST:3110 knowledge lookup --region AU --center-hz 404025000 --bandwidth-hz 25000
zeta job start --kind capture --source S --center-hz C --bandwidth-hz B   # write the signal's bursts as SigMF
zeta capture list                                               # the burst store, newest first
zeta capture classify --capture ID                              # "what is this?" on a stored window
zeta bits start --capture ID --then-tail 4,5000                 # capture -> demod -> clock -> slicer -> bit_view
```

`measure` is burst-aware: on a burst train it measures over the on-time and reports `duty`,
`bursts`, `burst_ms` and `period_ms`, and for on-off keying it fits the chip rate from the run
lengths (`symbol_rate_line: runs`). `--knowledge-acma URL` attaches the ACMA licence register (the
Australian Communications and Media Authority's register of radio licences, served by an MCP
server) as a provider of `knowledge_lookup`, `classify` and `identify`: licensee, site, distance
from the station, power and the decoded emission designator, a hypothesis about who transmits and
never a proof of what a signal is. `zeta origin lookup --job ID` (or `--source S --center-hz N --bandwidth-hz N [--modulation fm,fsk]`) scores the register's rows on a band: up to 50 sites, best first, each with a score from its width, modulation and reach terms (a site confirmed by a decoded identity scores 1) and an `agree` of `fits`, `disagrees` or `unknown`; for a job it also publishes an `origin` event and the page lights the sites on the map (the Origins button). The register may lack callsigns, in which case no site is ever confirmed by a decoded identity. The burst store keeps 256 MB and 7 days by default
(`--bursts-keep-mb`, `--bursts-keep-days`; pinned captures are kept); a watch with `on_unknown:
capture` fills it by itself. `bits_start` builds `capture_in -> [channel ->] resample ->
fsk|psk|ook demod -> clock_recover -> slicer -> bit_view` from the capture's feature record and
refuses rather than guesses a rate; `job_context` then holds the clock, the slicer and `bit_view`'s
hypotheses (line code, frame period, sync-word and CRC candidates). `graph_start` runs any graph
with no radio at its root (`capture_in`, `stream_in`, `mix`). The page's Explore pane lists the
captures (What is this, Download, Delete, Work the bits) and the Bits pane shows the bits and the
hypotheses. The node stops at classification: reading the content of someone else's link is the
operator's decision.

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
| `GET /health` | status, version, `build` (the build the node runs: `ZETA_BUILD_ID` if set, else version plus the executable's modification time; the web page reloads itself when it changes or the uptime goes backwards), uptime, websocket connections, `stages_running`, and the websocket counters since start: `ws_dropped_slow` (sockets closed for a slow reader), `ws_credit_skipped` (binary frames skipped for want of credit), `video_frames_sent` and `video_frames_skipped` (video frames not sent: a newer one first, lag or unencodable; the sum of the sockets' `lagged.missed`) |
| `POST /op` | apply one operation (`{"op": ...}` JSON) |
| `GET /schema` | the operation schema |
| `GET /streams` | live streams |
| `GET /agents-md` | the node's agent guide |
| `GET /stream/{id}` | websocket of one stream; a video stream in `?format=binary` is PNG pictures, `&video=smooth\|sharp\|thumb` (default `smooth`), the newest frame only, `lagged.missed` counting the skipped |
| `GET /stream/{id}` of a source's feed | every event of every job on the source over one socket; the feed is the `event` stream in `GET /streams` with that `provenance.source` and `job: null` |
| `GET /streams/{id}/tail?max_frames=N&timeout_ms=T` | the `stream_tail` operation: up to N frames arriving within T ms |

`plan_start` (`zeta plan start`) starts a kind on a plan's channels, with an optional attach, in one operation; the agent is told to use it for a kind on every channel, and never to stop or replace jobs the request did not name (`not_allowed`, unlocked per run).

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

`zeta ask` reads its request from stdin, so a `local-voice` transcript can be piped in. `--max-turns N` bounds the model turns; `--allow-stop` and `--allow-replace` let the agent stop jobs and stop the holders a conflict names, both refused otherwise. `--radio ID` (`ZETA_RADIO`) names the default radio, `--radio-policy focus|focus-fallback|free-first|ask` (`ZETA_RADIO_POLICY`) the policy and `--radio-prefer IDS` (`ZETA_RADIO_PREFER`, comma-separated) the preference order; with none of them the agent has no context and takes the first free radio (`free-first`).

## UI

`zeta serve --listen` also serves a web app at `http://HOST:PORT/ui/` (`/` redirects there). It
is plain JavaScript embedded in the binary, a client of the same HTTP and websocket API as
everything else: no build step, nothing fetched from elsewhere. It needs Chrome 120, Firefox 117 or Safari 17.2 or later. The page asks for the node's
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
  transcript and the job list, each folding on its header bar (the Jobs header shows the
  running-job count); along the bottom the command line (Ctrl-K) and the microphone. The default
  `console` theme frames the page in elbow-and-bar chrome and the selected source's tab bar
  carries its live pin, rate, overflows, gain, holders and bank from `source_list` (in `plain`
  they sit in the tab's tooltip).
- Narrow (under 700 px, a phone): the waterfall takes the screen. The transcript and job
  list move into a drawer behind ☰, and the agent's answers also appear as ribbons on the
  waterfall. The rail is 14 px of thin segments there; a tap expands one for 3 s.

The page's own `spectrum` job is elastic: it never blocks another job from taking the radio and
follows every retune, so the waterfall is never the reason a conflict appears. A job started inside
the capture it shows runs on that capture without a retune, so a demodulator started from the page
does not shrink the view; one outside it retunes the radio and the page's job follows. A conflict the page
meets offers "stop <kind> <id> and retry" (on a pinned source, `unpin <src> and retry`). Finished rows leave the job list after a while.

Two layers sit in the waterfall: the fine capture of the source, and, when the view is zoomed out
past it on a source with a scanner (a second source with a wider tune range, or a file source with
`--source-file ID=PATH,tune_min_hz=N,tune_max_hz=M`), a coarse layer from an automatic `scan`
drawn underneath. Rigid bands are outlined. The scan is started about 300 ms after the last zoom
or pan, spans the visible range clamped to the scanner's tune range, is replaced when the span
changes and stopped when the view returns inside the fine capture. A scan the page did not start
is never touched.

A pinned source's band is outlined, its source tab carries a `pin` tag (`.tag.pin`) and the pin is
counted as a holder there. The command
line takes `source pin --center-hz N --bandwidth-hz N [--replace ID]`, `source unpin` and `source
gain DB|auto` for the selected source. A `fm_demod`/`am_demod` row whose channel is idle shows a
`squelch` tag.

View policies shape this, set from the command line and kept in the tab's `sessionStorage` (except `radio_policy` and `radio_prefer`, which live in the browser's `localStorage` under `zeta.radio_prefs` and so follow every tab and restart):

| Key | Values (default first) |
|---|---|
| `follow` | `target` (ease to a new capture over about 400 ms), `free` |
| `zoom_out` | `scan`, `none` |
| `auto_scan_stop` | `inside` (stop when back inside the capture), `keep` |
| `finished_rows_s` | seconds a finished job stays listed, `60`; `0` never shows them |
| `scanner` | `auto`, or a source id |
| `theme` | a ready theme's id, set on `<html data-theme>`: the built-ins `console` (default), `plain` (tags on the outlines, level ribbons), `cyberpunk`, `terminal`, `psytrance`, `vibecoder` (the light one), and any operator theme in `--themes-dir` (`zeta themes list`; `docs/themes.md` is the author's guide) |
| `side` | `both`, `transcript`, `jobs`, `none`: which of the side panes show |
| `dock` | `auto` (the theme's `layout.dock`, else `bottom`), `bottom` (the selected pane under the waterfall or map), `right` (the selected pane as a third folding section of the side column) |
| `main` | `rf` (the waterfall), `map` (the Map tab's map in its place) |
| `map` | an alias of `main`: `on` sets `main map`, `off` sets `main rf` |
| `subjects` | `auto` (the Subjects tab shows while a running job's row has table columns), `on`, `off` |
| `subject_dim_s` | seconds of silence before a subject dims, `60` |
| `subject_drop_s` | seconds of silence before a subject goes from the table and the map, `300` |
| `origin_fade_s` | seconds over which a lit site's brightness falls to nothing after its signal was last active, `300` |
| `map_tiles` | `auto` (the theme's `layout.map_tiles`, else `osm-dark` under a dark theme and `osm` under a light one), `osm`, `osm-dark`, `opentopo`, `esri-street`, `esri-imagery` |
| `radio_policy` | `focus` (the agent uses the focused radio only; when it cannot take the band it says why, names the free radios and asks), `focus-fallback` (the first free radio in preference order, says which and why), `free-first`, `ask`; saved in the browser; `view reset` clears it; the agent cannot set it |
| `radio_prefer` | `none`, or radio ids in order, `b210,hackrf`; the fallback's preference order; saved in the browser |

`view set <key> <value>`, `view show`, `view reset` (every policy back to its default) and
`view clear` (drops the finished job rows from the list) are typed at the command line; an
unknown key or value answers with the allowed values. Other text starting with `view` (`view the
strongest signal`) is a question for the agent.

The waterfall is the UI's own `spectrum` job on the selected source (it reuses a running one,
and stops the one it started when the page closes). Overlays are pinned to their channel and
scroll with it: a `measure` job's layer-1 label with its confidence, an `aprs_decode` packet as
a ribbon (`VK3ABC > APRS: >hi` on the fixture), any other event as a short ribbon. Ribbons
ride their band: centred on the band line, tilted by the theme (-15 degrees in `console`, level in
`plain`; `layout.ribbon_tilt_deg` in a theme's `theme.json`) and in the job's colour; a new chunk takes the line and older ones step up a lane, then
fall back as it clears (a subject's keyed ribbon never steps). The job list
shows every job that is not stopped, two lines a row (kind and state, then band, numbers and the
id's tail; no sideways scroll), with a live event counter, ■ to stop it, ▶ on an
`fm_demod` row to play its audio in the browser (one job at a time) and `graph` to open the
job's graph view.

Drag across the waterfall to select a band and a time span; a context menu opens with:

| Entry | Operation |
|---|---|
| What is this? | `classify` on the selection; the labels are drawn at the selection and listed in the transcript |
| Decode › APRS | `job_start` `aprs_decode` on the selection's centre and bandwidth |
| Decode › NBFM audio | `job_start` `fm_demod` (deviation 5000 Hz) on the box's measured centre and 1.25× its occupied width, floored at 5 kHz; a box with only noise starts as drawn |
| Start background job › measure | `job_start` `measure` |
| Save selection as SigMF | `iq_read` on the selection; the transcript shows the capture path |
| Send to agent | `POST /ask` with "Look at C Hz, BW Hz wide, from START to END: what is it?" |

A running demodulator's outline has a handle at each edge (resize) and a body (move). The
dragged band is shown with a ghost and a readout and sent as `job_retune` on release, keeping
the job id, the attached `transcribe` and the ribbons; a conflict snaps it back with the usual
retry buttons. Right-click (long press on a phone) opens Fit to signal, the width presets and
Stop (a decoder's or a passthrough's menu holds Stop only; an SSB fit moves the dial and keeps
the band). Every rigid job carries a tag (kind, short id, squelch dot, channel, last transcript
snippet) linked by hover with its row in the job list, and rows show the band. In `console` the
tags move to a rail, a column left of the frequency axis, each at its band's height and showing
the plan channel (or the kind) and the centre in MHz; a tag that overlaps a full one collapses to
a thin segment, a hot or speaking tag stays full, and the outline, tag, row and ribbons share one
colour per job. Idle plan channels fold into one `<n> idle <kind> on <plan>` group when more than
8 are idle, judged only from what the page knows: a kind with no squelch, a job on the shown source (all are watched, through its feed)
whose squelch is known closed for 60 s, or an unwatched job on a source whose mix the page plays
and which that mix has not heard for 60 s. So idle channels on the shown source fold on their own
squelch evidence with no mix; only jobs on other sources need **Listen to all**. A row under the pointer stays put. A selection drag
can no longer start inside a running retunable job's band (its outline takes the pointer): start
the box outside it or on another part of the waterfall.

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

**One row per aircraft.** A mode row can name what its events are about (`subject`: the key field, table columns with units, how the waterfall shows it), and the page merges each subject's events into one state: a **Subjects** tab beside the map lists aircraft, sondes and stations in metres and km/h with distance from home, dimmed after 60 s of silence and dropped after 5 minutes (`view set subject_dim_s|subject_drop_s`); the map draws each with a heading-turned marker and a track that keeps a point only on movement and thins its older half at the row's `track_points`; a job with a table is one line on the waterfall. A click on a table row selects on the map and the reverse.

**Many channels at once.** `plan start <plan> <kind> [<attach>] [ch N,N-M]` on the command line (or `Start plan…` in the box menu) starts a kind on a plan's channels on the shown source in one operation, and `stop all` stops every job but the page's spectrum. **Listen to all**, beside the source tabs, or a typed `mix [ID,ID]`, plays one stream summing every demodulator on every tuner (or those named): silence until someone keys up, the speaking channel's row and outline lit, and the mix's row naming who is talking and who was last. **Mute in mix** on a demodulator's row or outline menu silences that channel in the mix, not its transcript; a mute follows the channel, and stopping the mix forgets it. Transcripts stay per demodulator; the mix's row offers Transcribe as an option. The agent may not stop or replace jobs, or delete captures, unless allowed: `allow stops on|off` and `allow deletes on|off` set that for the page's session, and a refused request offers a button that re-asks once with what was refused. Each answer's turns expand to the model's reasoning (when thinking is on) and the tool calls it made. Asked from the page, the agent may also change that tab's view settings ("switch to the plain theme"); running jobs never block that.

The command line takes either an operation as JSON (`{"op":"source_list"}`, applied with
`POST /op`) or free text, which goes to the agent. That needs `--llm-url` and `--llm-model` on
`serve` (the other `--llm-*` flags of `zeta ask` apply), which enable `POST /ask`: the node runs
the `zeta ask` agent against itself, so its calls appear in the transcript as they happen, and
the answer follows when the run ends; the body may carry `allow` (`{"stop_jobs": bool, "replace": bool}`, both false by default: without them the agent's `job_stop` and non-empty `replace` are refused as `not_allowed`), and the answer carries `allow` as the run had it and `turn_log`, one entry per model completion with its reasoning, content and call count (one run at a time; 503 without `--llm-url`, 504 after
300 s). The microphone records in the browser and sends a 16 kHz WAV to `POST /stt`, which hands
it to a `local-voice` speech daemon on the node's host at `--stt-socket PATH` (default
`$XDG_RUNTIME_DIR/voiced.sock` when that exists); the transcript is submitted as if typed.
Without a daemon the button says "no speech daemon on this node". Browsers allow the
microphone only on a secure origin: `localhost` qualifies, a LAN address over plain HTTP does
not.

Clicking `graph` on a job row draws its graph document (stages as boxes, streams as labelled
edges; in `console` the stages are rounded bars coloured by stage family with a params headline
under the kind). Clicking a stage shows its `params` as JSON; editing them and pressing Apply sends
`graph_put` with that stage changed, which restarts the job under the same id.

Only `/` and `/ui/*` (the static files, which hold no secret) are served without the secret.
Everything else still needs `Authorization: Bearer <secret>`. A browser cannot set that header on
a websocket, so `/stream/{id}` also accepts the secret as a subprotocol: offer
`["zeta", "zeta.bearer." + base64url(secret)]` (base64url without padding). This works for any
websocket client:

```bash
uv run --with websockets python - <<'PY'
import base64, json, os, urllib.request
from websockets.sync.client import connect
node, secret = "127.0.0.1:7979", os.environ["ZETA_SECRET"]
req = urllib.request.Request(f"http://{node}/streams", headers={"Authorization": "Bearer " + secret})
streams = json.load(urllib.request.urlopen(req))
ops = next(s["id"] for s in streams if s["provenance"]["source"] == "node")
feed = next((s["id"] for s in streams if s["provenance"]["source"] == "b210" and s["kind"] == "event" and s["provenance"]["job"] is None), None)  # connect to `feed` instead of `ops` below to watch the b210 radio (when it is not None)
proto = "zeta.bearer." + base64.urlsafe_b64encode(secret.encode()).decode().rstrip("=")
with connect(f"ws://{node}/stream/{ops}", subprotocols=["zeta", proto]) as ws:
    for msg in ws:
        print(msg)                                 # one {"type":"frame",...} per operation
PY
```

## Wideband

A wide capture is shared, not copied per job. For every source running at 2 Msps or more
(`PFB_MIN_RATE_HZ`) the node keeps one polyphase filter bank (`zeta-dsp` `PfbBank`, run on its
own thread per source and fed like the ring). The bank splits the capture into `bins` coarse
bins, the largest power of two that keeps each bin at least 750 kHz wide, clamped to 8..=256
(16 bins of 1 MHz at 16 Msps, 16 of 1.5 MHz at 24 Msps); each bin's stream is 2x oversampled.
A `channel` request whose `bandwidth_hz` fits in one bin width is **bank-served**: it reads the
nearest bin and runs its usual fine filter on that bin's stream, so a broadcast FM station
(200 kHz) costs a fine filter at a few Msps instead of a full-rate filter. A request wider than
a bin, one on a source below 2 Msps, or one whose output rate does not divide the bin rate takes
the direct path as before. Either way the job's graph, `center_hz`, `bandwidth_hz` and
provenance are the same; only the filter response differs. Stopping one job leaves the bank and
the other jobs on it untouched.

Spectrum rows (`spectrum` events, v2) are averaged to a fixed `rows_per_s` (default 20 at any
sample rate) and quantised: `{"size", "bin_hz", "rows_per_s", "center_hz", "floor_db",
"step_db", "q": [u8 x size]}` with `dB = floor_db + q * step_db`, `step_db` 0.5 and `floor_db`
the row's 2nd percentile minus 5 dB. A job's default `size` is the smallest power of two at
least `bandwidth_hz / 6000`, clamped to 1024..=8192 (4096 bins of 3.9 kHz for a 16 MHz capture),
so a row is at most 8 KB of JSON. The UI still draws an older node's float `db` rows.

In the UI the waterfall's own `spectrum` job covers the whole capture. `#band` (centre in MHz,
width in kHz, Apply), a drag on the frequency axis (pan) and the wheel (zoom x2 about the
pointer) change the view. A view inside the capture is rendering only: no operation is sent.
A view that leaves the capture retunes: if another job holds the source the transcript shows
`conflict: source <id> held by <jobs>` and the view reverts; otherwise the page stops its own
spectrum job, sends `source_ensure` for the new band and restarts the job on the capture it
gets (a file source answers `out_of_range` and the view reverts).

`scan` sweeps a span wider than one capture: `job start --kind scan --center-hz C
--bandwidth-hz W` steps the source across `C ± W/2` in captures of `min(max_bandwidth_hz,
16 MHz, W)` (a span narrower than one capture is one step of exactly that span), dwells 250 ms
per step, and emits one v2 spectrum row per step (frame `channel` `scan/<step>`, the row's
`center_hz` that step's), wrapping until stopped; a step is moved so its capture stays inside
the radio's tuning range. A scan time-shares the radio, so it holds the lease on
its whole span: while it runs every other `job_start` or `source_ensure` on that source is a
`conflict` naming the scan (no silent pre-emption), and `source_list` shows the scan as holder
and the step it is on as `current`. Stopping it frees the source and leaves `current` at the
last step. The UI's context menu has "Scan this span"; in scan mode the waterfall's axis spans
the scan and each row paints its step's segment of a new column, so a sweep fills in
progressively.

Sweeps alternate between two step grids a quarter step apart (even sweeps use the steps above,
odd sweeps shift every centre down by a quarter step, with at most one more step), so a step's
centre and its edges in one sweep fall a quarter of the way into a row of the next: read two
consecutive sweeps. On a local hardware receiver each row masks its centre notch (`notch_hz`,
50 kHz) and its outer edges (`edge_mask`, 0.02 of the bins each side; keep it under 0.25, or the
two grids' masks overlap); the row carries `masked: [[lo, hi], ...]` (bin index ranges, `hi`
exclusive), which are no data rather than low power. A one-step scan does not alternate and gets
no edge mask (no other sweep could fill it in). The job context's `scan.alternate` (`shift_hz`,
`steps_shifted`) and `scan.mask` (the values in force) report both, and `graph get`, an edit of
the scan's `fft` node (`notch_hz`, `edge_mask`) and `graph put` changes the mask.

```bash
Z="./target/release/zeta --node http://field-host:7979"   # ZETA_SECRET set
$Z job start --kind scan --source b210 --center-hz 500000000 --bandwidth-hz 1000000000
# 42 MHz (the B210's floor) to 1 GHz in 16 MHz steps: 61 steps, one sweep about 19 s
$Z job start --kind measure --source b210 --center-hz 95300000 --bandwidth-hz 200000
# {"error": "conflict", "holders": ["<scan job>"], "options": ["stop the scan holding the source"]}
```

The IQ ring is sized from a memory budget per source: seconds = budget / (rate x 8 B), clamped
to 1..10 s and recomputed on every retune (a retune to a higher rate shrinks the ring before the
first wide block arrives). The budget defaults to a quarter of `MemTotal` capped at 4 GiB (1 GiB
when `/proc/meminfo` cannot be read); `--ring-budget-mb MB` (or `ZETA_RING_BUDGET_MB`) sets it.
The node logs each sizing, e.g. `zeta: ring for b210: 10.00 s at 16000000 Hz (1280.0 MB)`.

## Federation

Upgrading: run the same version at both ends of a `--source-node` link. `job_start` requires
`elastic` and `replace`, which `--source-node` sends, and every `source_list` item carries `pin`,
`gain` and `bank`; a pre-release node without them refuses this version's `job_start`, and fails
its `source_list` round trip when it uses this node as a `--source-node`. A rigid page `spectrum` job persisted by an older
version resumes rigid (it holds its source); the page reuses an elastic spectrum job before it,
and stopping it from its row lets the page start an elastic one.

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

What crosses the wire is the stream websocket of [Transports](#transports) in binary mode, `GET /stream/{id}?format=binary`
(the default `format=text` is unchanged). IQ goes as VITA 49.2 signal-data packets (`ci16`
samples, at most 16 384 per packet) with a context packet first and then about once a second
carrying the RF centre, sample rate and bandwidth; each packet is one binary websocket message.
Audio, text and events go as the typed envelope, the plugin wire's data-frame framing
(`u32LE header_len | frame summary JSON | u32LE body_len | body`, the body `f32` LE PCM for
audio (interleaved; the header payload's `channels`, 1 when absent, and `samples` per channel, so the body is `samples × channels × 4` bytes), UTF-8 for text, empty for events). The server sends nothing until the client grants
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

A Raspberry Pi 5 at the antenna is the intended remote: [deploy/pi/README.md](../deploy/pi/README.md)
cross-builds `zeta` for aarch64, smoke-tests it under qemu and installs it as a systemd service
that refuses to start without a real secret. The operator's own machine then federates it with `--source-node`.

One caveat: in a two-host test against a USRP B210, the one-shot form (`job start ... --then-tail`) on a remote source streamed
for 10 s but produced no events in two runs while the same job under `serve` did; on one machine
with a file source the one-shot form works. Prefer a federating `serve` until that is resolved.

### Reading a source's health

`source list` shows a Soapy source's `stats` (`measured_rate_hz`, `overflows`, `timeouts`, `restarts`; `null` for a recording, a KiwiSDR, a `--source-node` or a `--source-vrt` source: `worker` shows the child that owns the device) and the bank's `dropped` count. A measured rate within 1 % of the pinned rate is healthy; `restarts` counts clock-guard stream restarts, each logged as `zeta: soapy <id>: samples ran <n> ms ahead of the clock; stream restarted`, and more than 5 in 60 s fault the source. `GET /health` carries `stages_running`, the live stage tasks; it counts every graph stage plus one forwarder per job on the radio (none for an attached job, one loop for a scan), and a number that climbs after jobs stop is a leak. A `measure` event with `partial: true` covers a window cut short by a gap, lag or retune, and `job context` shows `measure.discarded`, `partial` and `last`.

### A radio that comes back

A Soapy device runs in a `zeta soapy-worker` child that the node spawns, so a driver crash or a pulled cable faults the source and never ends the node. Expect this on a cable pull: within about a second `source list` shows `fault` as `no samples for 1000 ms (device unplugged?)` (the worker's exit 5), every job on the source fails with `source <id>: <fault>`, and `job_start` is refused with `source <id>: <fault>` (`source_ensure` and `source_pin` still answer `io: <fault>`). A driver crash reads `soapy worker for <id> died: signal 11 (SIGSEGV)` (or `exit code N`); a device that is not there reads `device not found: <args>`, and an open that fails `soapy open failed: <last stderr line>`. Nothing needs restarting: the node respawns the worker (1 s doubling to 5 s, reset after 60 s alive; every 5 s while the device is absent), logs `zeta: soapy worker for <id>: <fault>; waiting for the device` once and, when the device is back, `zeta: soapy worker for <id>: back after <n> attempts, <s> s`. Then it restores the gain and the pin and relaunches every job that failed with that fault, under its own id (a scan first, then widest first; an attached `transcribe` after its input; the `watch` re-adopting its decoders), logging `zeta: source <id>: recovered: pin <c>/<bw> Hz, gain <g> dB, <n> jobs relaunched`. Jobs you stopped, jobs that failed for their own reason and jobs that failed in an earlier episode stay as they are. The node awaits the worker's hello for up to 60 s (`soapy worker for <id>: no hello in 60 s`); on the field host the B210 says hello in 2-4 s. The worker hands its IQ to the node through a shared-memory ring the node creates for each spawn (64 MiB by default, `cf32` slots of 2 MiB, inherited by the child as fd 3), and the pipe carries only control and small block descriptors, so a burst of samples never delays a fault or a status. A worker without a ring (`memfd_create` unavailable) sends the blocks on the pipe, and `worker.transport.path` says which (`shm` or `pipe`).

`worker` in `source list` is `{pid, restarts, since_ns, transport, transport_dropped}` (`null` for a source that runs in the node): `restarts` counts respawns since the node started, `since_ns` is when the live worker said hello, and a `restarts` that climbs while the device is healthy means the driver keeps crashing: read the worker's own stderr lines, prefixed `zeta-soapy-worker[<id>]: `. A source that stays faulted while `restarts` rises is waiting for a device that is not on the bus. A device absent at startup attaches faulted (`device not found: <args>`) when an earlier start saw it, else the flag is refused (`device not found: <args> (never seen on this host; plug it in once)`, exit 1). `--soapy-in-process` opens the device in the node as before, for diagnosing the worker only: a crash then ends the node.

### A radio's ceiling

A radio can be asked for more than it can stream: a B210 will tune 61.44 Msps over USB 3 and then fall 16 % short with more than a thousand overflows a minute, so every job in the capture sees gaps. At open the worker therefore probes the device's rate candidates from the top (a UHD ladder's rungs, at most 6; `max`, `max/2`, `max/4` for a range device), each for a 1.5 s settle after the retune (a device may drop and overflow for a second or two after a rate change, a B210's master-clock switch among them) and then a 1.5 s dwell, and reports the first rung with at most one overflow in the dwell and at least 98 % of the nominal rate delivered as `capabilities.max_bandwidth_hz: {measured: N}` (one overflow is allowed because a healthy B210 still overflows now and then at every rate; a rung rejected for overflows gets one more settle and dwell before the probe moves down). The worker has 60 s to say hello, which fits the probe's worst case on a cold B210. It logs `zeta-soapy-worker[<id>]: rate cap <N> Hz measured (<k> rungs tried)`. If no rung is clean nothing is measured: the worker keeps the driver's asserted cap, logs `rate cap not measured: no rung clean (<k> rungs tried, lowest <hz> Hz); set max_rate_hz=<hz> to pin one`, and probes again at every respawn. A rung that loses the device ends the probe with `rate probe: device lost at <rate> Hz: ...` in the journal, and the worker keeps the asserted cap too. A request wider than a measured cap is refused `out_of_range` with `why: "insufficient bandwidth: <source> delivers at most <N> Hz (measured at open)"` (`why` is `null` for other refusals): the radio cannot do it, so narrow the request or take a second radio. That is a different thing from a digital link the node reports but does not yet decode, which is not a bandwidth problem. A measured cap is kept with the capabilities in `sources/<id>.json` and passed to every respawn (`--rate-cap-hz N`), so healing never re-probes; a respawn that measures a cap where the first open could not replaces the asserted one.

To state a ceiling yourself and skip the probe, put `max_rate_hz=N` among the `--source-soapy` args (zeta's own item, stripped before the driver sees it, like `gain=` and `ppm=`). That is the remedy for `no rung clean` and for a top rung that reliably kills the device (`rate probe: device lost at <rate>` at every open). An asserted cap is never cached: it is read from the args at every start, so changing or removing the item takes effect at the next one. To probe again instead (a new cable, a faster host), stop the node, remove the `capabilities` key from `sources/<id>.json` and start the node. Remove the key, not the file: the file also holds the source's pin, its gain and the record that this host has seen the device. Editing it while the node runs does nothing, since a pin or gain change rewrites the file and the running node keeps replaying the cap it holds.

Running a second node that owns the radio and attaching it with `--source-node` is federation (a radio on another host), not crash protection; one node with `--source-soapy hackrf=driver=hackrf,gain=32` is the form for a local HackRF.
