---
name: zetasdr
description: Use when driving a ZetaSDR node (sources, jobs, IQ captures, classification, knowledge lookup) through the zeta CLI or the zeta-mcp tools.
---

# ZetaSDR agent guide

ZetaSDR is an agentic SDR runtime. You drive it through a small typed operation set. The CLI form and the JSON form of every operation are equivalent: `zeta <group> <verb> --flag value` and `{"op": "<name>", ...}` do the same thing.

## Rules

- One unit per field: frequencies and bandwidths are integers in hertz in fields ending `_hz`, rates are in baud in fields ending `_baud`, times are integer nanoseconds since the UNIX epoch in fields ending `_ns`, durations are integer milliseconds in fields ending `_ms`.
- Every field is required and unknown fields are rejected.
- Call operations, do not explain.
- Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it.
- Unknown signals are reported as unknown, never guessed.
- Ids are never invented: `source` values come from `source list`, `job` values from `job list`.

## Running the node

Every command takes these global flags:

- `--state-dir PATH`: where jobs and captures persist (default `$XDG_STATE_HOME/zetasdr`, else `~/.local/state/zetasdr`). Pass the same directory on every invocation.
- `--source-file ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]`: attach a SigMF recording as source `ID`. With the range it plays as a radio tunable over `N`-`M` Hz (its `tune_min_hz`/`tune_max_hz` in `source list`): a band inside the range moves the capture's centre there, the samples staying the recording's (a test and demo aid); without it only the recording's own band is accepted.
- `--source-kiwi ID=HOST:PORT[/PASSWORD]`: attach a KiwiSDR receiver (0-30 MHz, at most 12 kHz wide) as source `ID`; the port is required and the password is never printed. Without `/PASSWORD` the password comes from `ZETA_KIWI_PASSWORD_<ID>` (`ID` upper-cased, other characters as `_`), which keeps it out of `ps`; the flag wins when both are given.
- `--source-node ID=SOURCE@URL[,max_bandwidth_hz=N]`: use source `SOURCE` of another ZetaSDR node, `URL` being its `http://HOST:PORT` (a `zeta serve --listen` node), as source `ID`. The node's secret comes from `ZETA_NODE_SECRET_<ID>` (`ID` mapped as for Kiwi passwords) and is never printed; without it the command is a usage error (exit 1). A remote source ships narrowband by default: `bandwidth_hz` is capped at 250 000 Hz (or the remote source's own maximum when smaller) unless `max_bandwidth_hz=N` (1-20 000 000) widens it explicitly. Each tune holds a `passthrough` job on the other node, so its `source list` and `job list` show who is using the source; the job is stopped when the tuning changes or this process exits normally (a killed process leaves it running; stop it there with `job stop`).
- `--source-vrt ID=udp://ADDR:PORT`: receive VITA 49.2 (VRT) packets, one per UDP datagram, on `ADDR:PORT` as source `ID`; `ADDR` must be a LAN address (`0.0.0.0` listens on every interface). The sender sets the frequency and rate: a job's band must lie inside the sender's last context packet (`source list` shows it as `current`; a job started before any context packet waits up to 1.5 s for one), otherwise the job fails with `unsupported` saying where the sender is; retune the sender, not the job. Packets that do not decode are skipped.
- `--source-soapy ID=ARGS`: attach a SoapySDR device as source `ID`, `ARGS` being SoapySDR device arguments such as `driver=hackrf,serial=...`; `zeta soapy find [--filter ARGS]` lists the devices attached to this machine as JSON. Only in a zeta built with `RUSTFLAGS="--cfg soapy"` (needs libsoapysdr-dev); otherwise both exit 1 saying so.
- `--ring-budget-mb N` (env `ZETA_RING_BUDGET_MB`): memory budget for one source's IQ history (default a quarter of RAM, at most 4 GiB). The ring keeps 10 s at narrow rates and shrinks to as little as 1 s at wide ones (a ring holds 8 bytes per sample, so 16 Msps gets 8 s of a 1 GiB budget); `iq read` and `classify` spans older than that fail `too_old`. The size chosen is logged to stderr as `zeta: ring for <source>: <s> s at <rate> Hz (<MB> MB)`.
- `--plugin KIND=PATH`: register the program at absolute `PATH` as stage kind `KIND` (lowercase, not a built-in kind). It is probed at startup; a failed probe exits 1 naming the kind and the reason. A plugin runs with a cleared environment: only `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM` and `PYTHONPATH` pass through, and each `ZETA_PLUGIN_ENV_<NAME>` variable reaches it as `<NAME>`; nothing else (no `ZETA_SECRET`) does.

Sources and plugins are not persisted: repeat the flags, with the same ids and kinds, on every invocation, `serve` included. A source id may be used once across all the source flags. `source list` shows each source's capability record: `remote` is true for a KiwiSDR, a `--source-node` and a `--source-vrt` source, `push` false for SoapySDR devices, and `fault` names a source that has failed (a full Kiwi, an unplugged device); a job on a faulted source fails with that reason.

There are two ways to run:

- One-shot, `zeta <group> <verb> ...`: opens the node, applies one operation, prints the JSON result and exits (0 on success, 2 with a JSON error). It never resumes jobs: a job started one-shot is persisted as `running` and runs once `zeta serve` starts. `job start --then-tail MAX_FRAMES,TIMEOUT_MS` keeps that one-shot node up after the start, tails the job's first event stream (else its audio stream), then stops the job and prints `[job_result, frames_result, {"stopped": true}]`; add `--keep` to leave it running (`{"stopped": false}`). If the job is not running (e.g. failed) it prints `[job_result, job_record]`, the record's `state` carrying `status` and `reason`, and exits 2; `stream tail` on its own only sees streams of jobs running in the same process, and `job attach` needs its input job running in the same node, so attach against `zeta serve` (`--node`). A one-shot `iq read` or `classify` reads the ring that the command itself fills from the source, waiting up to 5 s for it to cover the span. One-shot commands, like `zeta serve`, start and probe every `--plugin` program at startup, so with plugins configured even a one-shot `source list` costs one process start per plugin.
- `zeta serve`: resumes persisted jobs and keeps them running until Ctrl-C.
- Against a running node, `zeta --node URL --secret SECRET <group> <verb> ...` (or the `ZETA_NODE` and `ZETA_SECRET` variables): the operation is sent to that node over HTTP and applied there, so jobs started this way keep running; `--state-dir`, the `--source-*` flags and `--plugin` belong to the serving node and are rejected (a usage error, exit 1); `--stt-socket` and `--stt-node` belong to it too but are ignored. `zeta soapy find` always runs locally. `--then-tail` is not supported with `--node` yet (a usage error, exit 1): start the job, then `stream list` and `stream tail` against the node. Start that node with `zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) and a secret. `zeta-mcp --node URL` offers the same operations as MCP tools, and `zeta ask "REQUEST"` lets a local model call them.
- `zeta serve --listen PORT --llm-url URL --llm-model NAME` (the other `--llm-*` flags of `zeta ask` apply too) enables `POST /ask` with `{"request": "TEXT", "max_turns": 8}` (`max_turns` 1-32, default 8): the node runs the agent against itself, so every operation it applies appears on the `ops` stream, and answers the transcript (`request`, `answer`, `calls`, `turns`) as `zeta ask` prints it. One run at a time (409 while one is in progress); 503 when serve has no `--llm-url`; a failed run is 502 with the error and the transcript so far; 504 after 300 s.
- `zeta serve --listen PORT --stt-socket PATH` (env `ZETA_STT_SOCKET`; default `$XDG_RUNTIME_DIR/voiced.sock` when that file exists) enables `POST /stt?lang=CODE` (`lang` defaults to `en`): the body is a 16 kHz mono `audio/wav` (at most 4 MiB), the node hands it to the speech daemon at that Unix socket and answers `{"text": "TRANSCRIPT"}`. The daemon is `local-voice` or any daemon speaking its protocol: one line per connection, `file:<absolute wav path>` answered `ok:<text>` or `error:<message>`, and `languages` answered `ok:en,de,...` (a daemon answering `error:` there is taken to know English only). Instead of a daemon, `--stt-node URL` (env `ZETA_STT_NODE`, secret in `ZETA_STT_NODE_SECRET`, never printed) forwards each request to that node's `POST /stt`; the two flags are exclusive, for every command (exit 1). A forwarded request carries `X-Zeta-Stt-Hop: 1`, and a node that would forward it again answers 508 (`stt forwarding loop`). `GET /stt/languages` answers `{"backend": NAME, "languages": [CODES]}` (503 without a backend). 415 when the body is not WAV, 413 when too large, 422 `{"error": "unsupported", "language", "supported"}` for a language the backend lacks, 502 when the daemon reports an error, 503 when serve has no backend or cannot reach it, 504 after 30 s.

While `zeta serve` runs it owns the state directory (it holds `serve.lock` there). Mutating one-shot commands (`job start`, `job attach`, `job stop`, `graph put`, `source ensure`) are then refused with an `other` error naming the server's pid; read-only commands still work. Use `--node` to change a node that `zeta serve` owns.

Every operation the node applies is appended to `ops.log` in the state directory (rotated at 8 MiB) with its caller's transport; `zeta ops tail --lines N` prints the last N (local only).

## Operations

### source_list

List the sources: id, capabilities, `current` band, `holders` (the rigid lease holders' job ids), `elastic` (the jobs following the capture, see `job_start`) and `fault`, the source's unrecoverable failure (`null` when healthy). Jobs on a faulted source fail with `source ID: FAULT` as their reason; a successful tune (`source_ensure`, `job_start`) clears it.

JSON: `{"op":"source_list"}`

CLI: `zeta source list`

### source_ensure

Check a band against the source and tune it if no job holds it. Takes no lease: an unheld source is tuned directly (a later `job_start` may retune it); a held source is never retuned, and the request either fits the current band (`Tuning`) or is a `Conflict`. Runs under the lease manager's per-source gate, so it never interleaves with a job's lease-and-tune. Elastic jobs do not hold the source: a tune relaunches them on the new capture.

- source: string; an id from `source list`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"source_ensure","source":"<id>","center_hz":144800000,"bandwidth_hz":12500,"replace":[]}`

CLI: `zeta source ensure --source <id> --center-hz 144800000 --bandwidth-hz 12500`

`replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder.

### job_start

- kind: string (JobKind: passthrough|spectrum|fm_demod|wfm_demod|am_demod|ssb_demod|aprs_decode|pocsag_decode|rds_decode|acars_decode|adsb_decode|measure|scan|transcribe)
- source: string; an id from `source list`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)
- deviation_hz: integer or null
- sideband: string or null
- elastic: boolean
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"job_start","kind":"passthrough","source":"<id>","center_hz":144800000,"bandwidth_hz":12500,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}`

CLI: `zeta job start --kind passthrough --source <id> --center-hz 144800000 --bandwidth-hz 12500`

`fm_demod` is narrowband FM voice; `wfm_demod` is wideband broadcast FM (mono, 50 us de-emphasis, 15 kHz audio; give it a band of about 200000 Hz). `am_demod` is AM voice (envelope detector, 5 kHz audio, slow AGC; give it a band of about 10000 Hz). `ssb_demod` is SSB voice (2800 Hz audio, slow AGC; `center_hz` is the suppressed-carrier (dial) frequency, not the middle of the voice band, and the band must cover both sides of it: give it about 6000 Hz). All four output 48 kHz mono audio: an audio frame's payload is `{"sample_rate_hz", "channels", "samples": N, "peak"}`, `channels` 1 here, the samples interleaved and `samples` the per-channel count (a frame without `channels` means 1). `pocsag_decode` decodes POCSAG pagers (512, 1200 and 2400 baud detected automatically) from a 12.5 kHz FM channel into one event per message (`address`, `function`, `kind`, `text`). `rds_decode` reads the RDS data of a broadcast FM station (give it the same 200000 Hz band as `wfm_demod`) into events carrying `pi`, `ps` (station name) and `rt` (radio text) as they complete; the first event takes a few seconds. `adsb_decode` decodes Mode S / ADS-B at 1090000000 Hz with `bandwidth_hz` 2400000 (anything under 2000000 is refused) by running dump1090 on the node's host; one event per decoded message (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`, `track_deg`), position and identity arriving in separate messages. The job fails with a reason naming dump1090 when the host has none. `acars_decode` decodes ACARS (VHF, AM, 2400 baud MSK) from a 10000 Hz channel on 131550000 Hz (worldwide primary), 131725000 / 131825000 (Europe), 130025000 / 130450000 / 131125000 (North America), 131450000 (Asia) into one event per CRC-clean block (`reg`, `flight`, `label`, `text`). `deviation_hz` is the FM peak deviation for `fm_demod` and `wfm_demod` (100-75000 Hz; `null` means the kind's default: 5000 Hz for `fm_demod`, right for standard voice FM, use 2500 for narrow 12.5 kHz channels; 75000 Hz for `wfm_demod`, right for broadcast FM). Every other kind takes `null`. `sideband` is `usb` or `lsb` for `ssb_demod` (`null` means `usb`; amateur convention is `lsb` below 10 MHz, `usb` above); every other kind takes `null`. On the CLI, omit `--deviation-hz` and `--sideband` for `null`. `scan` sweeps a band wider than one capture: the node steps the source across it in captures of at most 16 MHz, dwells 250 ms on each, and emits `spectrum` rows whose `center_hz` is that step's (frame `channel` `scan/<step index>`), wrapping until stopped. A scan time-shares the radio: while it runs, every other `job_start` and `source_ensure` on the source is a `conflict` naming it, decoders included, and `source_list` shows the step it is on as `current`. Stop the scan to decode. A scan's `job_context` holds `scan.steps` (how many captures one sweep takes), `scan.step_hz` and `scan.dwell_ms`: report step counts from it, never from guesswork. `transcribe` is not started on the radio: `job_start` refuses it; use `job_attach`. `elastic: true` (spectrum only) makes the job follow the source's capture instead of holding it: it never conflicts, is relaunched on every retune and is listed under `elastic` in `source_list`; the web page's own spectrum job is elastic. `replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder.

### job_attach

Attach a job of an attachable `kind` (`transcribe`) to the running job `job`: it reads that job's audio output stream (the last one in its graph order when it has several; audio passed between two of its stages does not count) instead of the radio, holds no lease, copies the job's `source` and `tuning`, and records it as its `input`. It ends `stopped` when the input job's stream closes for any reason, a restart by `graph_put` included (attach again to follow the new graph). On a node restart it resumes after its input, or is `failed` with `input job <id> not running` when the input does not run.

- kind: string (JobKind: passthrough|spectrum|fm_demod|wfm_demod|am_demod|ssb_demod|aprs_decode|pocsag_decode|rds_decode|acars_decode|adsb_decode|measure|scan|transcribe)
- job: string; an id from `job list`
- language: string or null

JSON: `{"op":"job_attach","kind":"transcribe","job":"<id>","language":null}`

CLI: `zeta job attach --kind transcribe --job <id>`

Attach a processing chain to a running job's output instead of the radio: `transcribe` reads the job's audio (any demodulator, later any digital voice decoder), cleans it (denoise, voice gate) and emits one `transcript` event per utterance (`text`, `language`, `start_ns`, `end_ns`) plus `vad` events as speech starts and stops; it holds no lease and ends when the input job stops. `language` is a BCP-47 code (`null` means `en`; the backend's languages are in `GET /stt/languages`). To transcribe a station: `job_start` a `wfm_demod` on it, then `job_attach` with `kind: transcribe` and that job's id. The attached job fails with a reason naming the speech backend when the node has none or it cannot serve the language.

### job_list

JSON: `{"op":"job_list"}`

CLI: `zeta job list`

Stopped and failed jobs are kept as history, newest 20 only (older ones are forgotten with their files); a running job is never dropped. Through `ask` (the agent transport) each record is returned without its `graph`; `graph_get` fetches one.

### job_stop

- job: string; an id from `job list`

JSON: `{"op":"job_stop","job":"<id>"}`

CLI: `zeta job stop --job <id>`

### job_context

- job: string; an id from `job list`

JSON: `{"op":"job_context","job":"<id>"}`

CLI: `zeta job context --job <id>`

### iq_read

Save a selection under `state_dir/captures`: writes the whole ring bandwidth over the span as a SigMF pair; the selection band is recorded as an annotation. The capture is labelled with the ring's centre, and the selection band must lie inside the ring's band.

- selection: object (Selection: source, span{start_ns,end_ns: integer (ns since epoch)}, center_hz, bandwidth_hz)

JSON: `{"op":"iq_read","selection":{"source":"<id>","span":{"start_ns":1760000000000000000,"end_ns":1760000001000000000},"center_hz":144800000,"bandwidth_hz":12500}}`

CLI: `zeta iq read --source <id> --start-ns 1760000000000000000 --end-ns 1760000001000000000 --center-hz 144800000 --bandwidth-hz 12500`

### stream_list

List the streams: id, kind and provenance. One is always the node's own `ops` event stream (provenance source `node`): one event per applied operation with its outcome and caller (`transport`: `local`, `http`, `ui` or `agent`), logged after the operation completes.

JSON: `{"op":"stream_list"}`

CLI: `zeta stream list`

### stream_tail

Collect frames from a stream until `max_frames` (1..=1000) arrive or `timeout_ms` (1..=10000) passes, whichever is first; frames published before the call are not seen. Frames skipped because the caller fell behind are dropped and counted in the result's `missed`.

- stream: string; an id from `stream list`
- max_frames: integer
- timeout_ms: integer

JSON: `{"op":"stream_tail","stream":"<id>","max_frames":1,"timeout_ms":5000}`

CLI: `zeta stream tail --stream <id> --max-frames 1 --timeout-ms 5000`

### graph_get

- job: string; an id from `job list`

JSON: `{"op":"graph_get","job":"<id>"}`

CLI: `zeta graph get --job <id>`

### graph_put

Replace a job's graph; restarts a running job under the same id with the new graph.

- job: string; an id from `job list`
- graph: object (Graph: id, nodes[Node: id, kind, params], edges[Edge: from, to, kind])

JSON: `{"op":"graph_put","job":"<id>","graph":{"id":"<id>","nodes":[{"id":"<node-id>","kind":"passthrough","params":{"kind":"iq"}}],"edges":[]}}`

CLI: `zeta graph put --job <id> --file graph.json`

Fetch the current graph with `graph get`, edit it, and put it back; stage kinds and their params are listed under Stage kinds below and by `schema`.

### knowledge_lookup

Band-plan allocations in `region` and known signatures overlapping the band, from every registered knowledge provider.

- region: string; free-text region code such as `AU`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"knowledge_lookup","region":"AU","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta knowledge lookup --region AU --center-hz 144800000 --bandwidth-hz 12500`

Two providers answer: the built-in seed signatures, and `catalogue`, the 594-signal iNTERCEPT catalogue (Apache-2.0; one signature per frequency range of each signal overlapping the band, `bandwidth_hz` its widest bandwidth, `modulation` the first it lists). A signature's `decoder` names the job kind that proves it: start that kind on the band to decode it; `classify` attempts only the APRS, POCSAG, ACARS and RDS ones as decode-to-identify. Neither has allocations, so an empty `allocations` list means no provider data, not nothing allocated.

### classify

Run every registered classifier over the selection's band and span of the source ring (no upstream features).

- selection: object (Selection: source, span{start_ns,end_ns: integer (ns since epoch)}, center_hz, bandwidth_hz)

JSON: `{"op":"classify","selection":{"source":"<id>","span":{"start_ns":1760000000000000000,"end_ns":1760000001000000000},"center_hz":144800000,"bandwidth_hz":12500}}`

CLI: `zeta classify --source <id> --start-ns 1760000000000000000 --end-ns 1760000001000000000 --center-hz 144800000 --bandwidth-hz 12500`

Layer 1 labels the measured band (`wfm`, `fm`, `am`, `ssb`, `fsk`, `carrier`, `noise` or `unknown`; an `ssb` label's evidence gives the dial frequency). Layer 2 first tries decode-to-identify: `APRS`, `POCSAG` and `ACARS` on a selection narrower than 150000 Hz (100000 Hz when the whole block is measured), `RDS` on a wider one, and labels a protocol only when its CRC/BCH-clean decode passes; ADS-B is never attempted by `classify` (use `adsb_decode`). Layer 2 also adds up to three signal names from the catalogue (evidence `catalogue <id>: ...`), scored 0.4 for the measured centre lying in one of the signal's frequency ranges (required), 0.3 for a matching bandwidth (required for signals wider than 50 kHz) and 0.3 for a matching layer-1 modulation. Layer 3 is an empty slot.

### schema

JSON: `{"op":"schema"}`

CLI: `zeta schema`

## Stage kinds

A graph node's `kind` names one of these; its `params` must match the listed fields.

### acars_decode

- inputs: iq
- outputs: event

### adsb_decode

- inputs: iq
- outputs: event

### am_demod

- inputs: iq
- outputs: audio
- params.audio_rate_hz: integer

### aprs_decode

- inputs: audio
- outputs: event

### channel

- inputs: iq
- outputs: iq
- params.center_hz: integer
- params.bandwidth_hz: integer
- params.out_rate_hz: integer

### denoise

- inputs: audio
- outputs: audio

### fft

- inputs: iq
- outputs: event
- params.size: integer
- params.window: string (Window: hann|rect)

### measure

- inputs: iq
- outputs: event
- params.bandwidth_hz: integer or null

### nbfm_demod

- inputs: iq
- outputs: audio
- params.deviation_hz: integer
- params.audio_rate_hz: integer

### passthrough

- inputs: iq, audio, video, text, event, sink
- outputs: iq, audio, video, text, event, sink
- params.kind: string (StreamKind: iq|audio|video|text|event|sink)

### pocsag_decode

- inputs: iq
- outputs: event

### rds_decode

- inputs: iq
- outputs: event

### ssb_demod

- inputs: iq
- outputs: audio
- params.sideband: string (Sideband: usb|lsb)
- params.audio_rate_hz: integer
- params.audio_bandwidth_hz: integer

### stream_in

- inputs: 
- outputs: audio
- params.job: string
- params.kind: string (StreamKind: iq|audio|video|text|event|sink)
- params.stream: string

### transcribe

- inputs: audio
- outputs: event

### vad

- inputs: audio
- outputs: audio, event

### wfm_demod

- inputs: iq
- outputs: audio
- params.deviation_hz: integer
- params.audio_rate_hz: integer
- params.deemphasis_us: integer

## Field craft

Measured facts first, then method. Verify against the node's `knowledge lookup` before relying on any of this.

### Where to look

- Radiosondes: 400-406 MHz. RS41 is GFSK at 4800 baud, about 10 kHz wide.
- APRS: 144.800 MHz in AU and EU, 144.390 MHz in the US. AFSK at 1200 baud.
- POCSAG: the preamble is 576 bits of alternating 1/0, then the 32-bit sync word 0x7CD215D8. Common rates are 512, 1200 and 2400 baud.
- ADS-B: 1090 MHz, PPM at 1 Mbaud, 112-bit extended squitter.
- MeshCore: LoRa around 915 MHz in AU and 869 MHz in EU, 250 kHz wide. Not decodable without the network key; classify and report, do not try to decode.

### Decoding APRS and reading measurements

To decode APRS from a source, start the job and tail its events in one call: `zeta job start --kind aprs_decode --source S --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000`. It prints `[job_result, frames_result, {"stopped": true}]` (the job is stopped after the tail; add `--keep` to leave it running); each frame's `payload` is one decoded packet (`src`, `dst`, `info`, `fcs_ok`). An empty frame list means no packet arrived within the timeout, not that none exist. Exit 2 with `[job_result, job_record]` means the job is not running: read `state.reason` (e.g. a stage that failed on the source's blocks). A `measure` job emits one event per second of IQ: `center_hz` and `bandwidth_hz` are the occupied band (spectrum bins at least 6 dB above the noise floor, the 15th-percentile bin of the band), `snr_db` is the peak above that floor, `burst` is true when the loudest 10 ms is more than 3 times the median 10 ms power, `symbol_rate_baud` is the keying rate when a symbol-rate line stands out and `null` otherwise, and `label.name` is one of `noise` (SNR under 6 dB), `carrier` (steady envelope, no modulation), `fm` (steady envelope with deviation), `fsk` (two tones 500-3000 Hz apart, on RF or in FM audio) or `unknown`. Report `unknown` as unknown, with the measured fields. `classify` and `measure` analyse only the selection/job band (a `measure` event's `center_hz` is absolute RF, within that band); `spectrum` covers the whole source band. A `spectrum` event is one row of power-averaged spectrum (20 per second of signal time) with `size` bins lowest frequency first, `bin_hz`, `center_hz` and the levels quantised as `q` (0-255 per bin): the dB of bin k is `floor_db + q[k] * step_db`; `floor_db` sits 5 dB under the row's 2nd-percentile bin, so a `q` near 10 is the noise floor and 255 means at least 127.5 dB above `floor_db`.

### Decoding pagers, aircraft and FM data

Where to find them:

- POCSAG: 148-149 MHz in AU (Telstra at 148 637 500 Hz among them), 466 075 000 Hz in much of Europe, 929-932 MHz in the US. Use `pocsag_decode` on a 12 500 Hz band.
- ACARS: 131 550 000 Hz (worldwide primary), 131 725 000 and 131 825 000 Hz (Europe), 130 025 000, 130 450 000 and 131 125 000 Hz (North America), 131 450 000 Hz (Asia). Use `acars_decode`, 10 000 Hz each.
- RDS: any FM broadcast station. Use `rds_decode` with the station's frequency and a 200 000 Hz band. The first event takes a few seconds.
- ADS-B: 1 090 000 000 Hz with a 2 400 000 Hz band. Use `adsb_decode`. It needs dump1090 on the node's host; the job fails naming dump1090 when it is missing. A band under 2 000 000 Hz is refused.

"Set up a background decoder for all ACARS and ADS-B activity":

1. `source_ensure` the radio at 130 925 000 Hz with 2 400 000 Hz. That band covers every ACARS channel above.
2. Start one `acars_decode` per channel inside it. They share the capture.
3. Start `adsb_decode` on a second source. A radio serves one band at a time, so on one radio it must wait until the ACARS jobs are stopped.

`knowledge_lookup` says which decoder proves a band's signature (`decoder` names the job kind). `classify` tries APRS, POCSAG and ACARS on narrow bands and RDS on wide ones, as decode-to-identify; it never attempts ADS-B (use `adsb_decode`). A CRC-clean decode is the strongest layer 2 evidence there is. A signature match without one is a guess: say so.

### Remote HF receivers (KiwiSDR)

A KiwiSDR source covers 0-30 MHz and delivers 12 kHz of IQ per connection, so `bandwidth_hz` is at most 12 000: work one HF channel at a time with `measure` jobs or `classify` selections inside the band you tuned (e.g. 7 100 000 Hz with 10 000 Hz for 40 m), not wide `spectrum` sweeps. A Kiwi has a limited number of user slots; when they are all taken the source faults with `too_busy` and the job fails with that reason: report it, do not retry in a loop. The connection opens on the first tune and holds a slot until the node exits. Reception is site-dependent: the receiver's location, antenna, local noise and the time of day decide what is audible, so an empty band on one Kiwi says nothing about the band elsewhere.

### Sources on another node (federation)

A `--source-node` source is a receiver attached to another ZetaSDR node, typically a Raspberry Pi at the antenna. That node ships narrowband IQ by default (250 kHz at most), which a LAN or WiFi link carries comfortably: work one channel at a time with `measure`, `aprs_decode` or `classify` inside it. Widen it only when the job needs it, with `max_bandwidth_hz=N` on the flag, and expect a slow link to drop blocks rather than slow down. A `bandwidth_hz` over the cap is `out_of_range` with `suggested` set to the cap. Each tune runs a `passthrough` job on the remote node, so that node's `source list` and `job list` show the source as held: another caller there sees the lease conflict instead of a silent retune. If the remote node goes away, the local source faults naming its URL and the job fails with that reason: report it, do not retry in a loop.

### External stages

A plugin registered with `--plugin KIND=PATH` is a stage kind like any other: it is listed in `schema` under `stages` with its `params_schema`, and you use it by adding a node of that `kind` to a job's graph with `graph get`, edit, `graph put`. No job kind starts it on its own. A plugin that crashes or misbehaves fails its job with a reason naming the plugin.

### Transcribing a channel

Start the demodulator, then attach the transcription to it; the attached job reads the
demodulator's audio, holds no radio lease and ends when that job stops:

1. `job start --kind wfm_demod --source S --center-hz 100500000 --bandwidth-hz 200000` (`fm_demod`, `am_demod` or `ssb_demod` for voice channels). Note the job id.
2. `job attach --kind transcribe --job THAT_ID`. Its chain is `stream_in → denoise → vad → transcribe`.
3. `stream list` shows two event streams on the attached job (`vad`'s and `transcribe`'s); `stream tail` them. A `transcript` event is one utterance (`text`, `language`, `start_ns`, `end_ns`); `vad` events mark where speech starts and stops. An empty tail means no utterance ended in the window, not that nothing was said.

The node needs a speech backend (`--stt-socket` or `--stt-node`); without one the attached job fails naming it, so report that and do not retry. Language: the attached job transcribes `en` unless it was attached with `language`; a language the backend does not list (see `GET /stt/languages`) fails the job naming the languages it has. Do not guess a language from the band.

Music and noise are gated, not separated: `vad` passes any loud audio, so a music station or a broadcast with a bed under the voice yields poor or empty text. Say so rather than reporting the text as the speech. Separation is a slot: an external audio-to-audio plugin (`--plugin separate=PATH`, a Demucs wrapper for instance) replaces the `denoise` node with `graph get` of the attached job, edit, `graph put`. The job restarts on `graph put`. `docs/speech.md` has the daemon install and the plugin stub.

Digital voice decoders (DMR, D-STAR, M17) will be transcribed the same way: they produce audio, so `job attach --kind transcribe` to them.

### Taking the radio from another job

A source holds one tuning, and a second job that needs a different one gets a `conflict` error. Read it before acting: it names the holders (job ids, kinds and what each is doing) and lists options, the last being `retry with replace: [<ids>]`. Decide whether the holders matter. If they do not, or the user asked for the new job, retry the same request with `--replace ID[,ID]` (`job start`, `source ensure`; `replace` is an array in the op). The named jobs are stopped and the new job started in one step under the source's gate, so nothing else can slip in between. Every id must hold that source (`unknown_job` or `job <id> does not hold source <src>` otherwise), and a holder you did not name still blocks the start and is named in the new conflict. Never replace a holder you have not read, and say in the report what you stopped.

The web page's own `spectrum` job is elastic (`job start --elastic`, `spectrum` only): it never conflicts, never blocks a retune and follows each retune of its source, so it is not a holder to wait for or replace. `source list` shows such jobs under `elastic`, not `holders`. A scan on the source stops it, and the page restarts its view afterwards. After a restart of the node, jobs resume widest first and their capture is restored. `zeta ops tail --lines N` shows the last applied ops with the transport that sent them (`local`, `http`, `ui`, `agent`): use it to see who changed the radio.

### Working layer 4

When a signal has no known decoder, work it in this order:

1. Isolate the channel.
2. Resample to a rate suited to the symbol rate.
3. Recover the clock.
4. Slice symbols.
5. Inspect the bit stream.
6. Hypothesise framing (preamble, sync word, length, checksum).
7. Attempt a decode.
8. Write the result back as a signature, so the next encounter is a layer 2 match.

### The rule

When nothing matches, say "unknown" and report the measured features (centre, bandwidth, modulation, symbol rate, burst timing). Do not guess.
