//! `zeta agents-md`: render AGENTS.md from the operation schema plus the hand-written field craft.

use std::process::ExitCode;

use serde_json::Value;

const FIELD_CRAFT: &str = include_str!("../../../docs/agents/field-craft.md");

const INTRO: &str = "ZetaSDR is an agentic SDR runtime. You drive it through a small typed operation set. \
The CLI form and the JSON form of every operation are equivalent: `zeta <group> <verb> --flag value` \
and `{\"op\": \"<name>\", ...}` do the same thing.";

const RULES: &[&str] = &[
    "One unit per field: frequencies and bandwidths are integers in hertz in fields ending `_hz`, rates are in baud in fields ending `_baud`, times are integer nanoseconds since the UNIX epoch in fields ending `_ns`, durations are integer milliseconds in fields ending `_ms`, gains are decibels in fields ending `_db`.",
    "Every field is required and unknown fields are rejected.",
    "Call operations, do not explain.",
    "Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it.",
    "Unknown signals are reported as unknown, never guessed.",
    "Ids are never invented: `source` values come from `source list`, `job` values from `job list`.",
    "Jobs belong to the operator: never `job_stop` a job or put one in `replace` unless the request names it, the run allows it or, for `job_stop`, you started it in this run. A `conflict` is reported with its holders; the operator decides.",
    "For a kind on every channel of a plan use `plan_start`, never one `job_start` per channel.",
    "Plan ids come from `channel_plans`: read it before naming a plan. A band with no plan takes one `job_start` per measured station; never invent a plan.",
    "To stop every job, or every job of a source, kind or plan, use `jobs_stop`, never one `job_stop` per job.",
];

const RUNNING: &str = "Every command takes these global flags:

- `--state-dir PATH`: where jobs and captures persist (default `$XDG_STATE_HOME/zetasdr`, else `~/.local/state/zetasdr`). Pass the same directory on every invocation.
- `--source-file ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]`: attach a SigMF recording as source `ID`. With the range it plays as a radio tunable over `N`-`M` Hz (its `tune_min_hz`/`tune_max_hz` in `source list`): a band inside the range moves the capture's centre there, the samples staying the recording's (a test and demo aid); without it only the recording's own band is accepted.
- `--source-kiwi ID=HOST:PORT[/PASSWORD]`: attach a KiwiSDR receiver (0-30 MHz, at most 12 kHz wide) as source `ID`; the port is required and the password is never printed. Without `/PASSWORD` the password comes from `ZETA_KIWI_PASSWORD_<ID>` (`ID` upper-cased, other characters as `_`), which keeps it out of `ps`; the flag wins when both are given.
- `--source-node ID=SOURCE@URL[,max_bandwidth_hz=N]`: use source `SOURCE` of another ZetaSDR node, `URL` being its `http://HOST:PORT` (a `zeta serve --listen` node), as source `ID`. The node's secret comes from `ZETA_NODE_SECRET_<ID>` (`ID` mapped as for Kiwi passwords) and is never printed; without it the command is a usage error (exit 1). A remote source ships narrowband by default: `bandwidth_hz` is capped at 250 000 Hz (or the remote source's own maximum when smaller) unless `max_bandwidth_hz=N` (1-20 000 000) widens it explicitly. Each tune holds a `passthrough` job on the other node, so its `source list` and `job list` show who is using the source; the job is stopped when the tuning changes or this process exits normally (a killed process leaves it running; stop it there with `job stop`).
- `--source-vrt ID=udp://ADDR:PORT`: receive VITA 49.2 (VRT) packets, one per UDP datagram, on `ADDR:PORT` as source `ID`; `ADDR` must be a LAN address (`0.0.0.0` listens on every interface). The sender sets the frequency and rate: a job's band must lie inside the sender's last context packet (`source list` shows it as `current`; a job started before any context packet waits up to 1.5 s for one), otherwise the job fails with `unsupported` saying where the sender is; retune the sender, not the job. Packets that do not decode are skipped.
- `--source-soapy ID=ARGS`: attach a SoapySDR device as source `ID`, `ARGS` being SoapySDR device arguments such as `driver=hackrf,serial=...`. The device runs in a `zeta soapy-worker` child the node spawns and respawns: a driver crash or an unplugged device faults the source (`source list` shows `fault`, jobs on it fail with `source <id>: <fault>`) and never ends the node. The node then respawns the worker and waits for the device; when it is back it restores the pin and the gain and relaunches every job that failed with that fault under its own id, the `watch` and its decoders and an attached `transcribe` included, logging `zeta: source <id>: recovered: ...`. `worker` in `source list` shows the child's pid and how many times it was respawned. A device absent at startup is attached faulted when the host has seen it before, else the flag is refused (exit 1). `zeta soapy-worker --id ID --args ARGS` is what the node spawns; it is not run by hand. The worker inherits the node's environment (the driver reads `UHD_IMAGES_DIR`, `SOAPY_SDR_PLUGIN_PATH`) minus the secrets: every `ZETA_*` variable whose name contains `SECRET`, `PASSWORD` or `KEY` (`ZETA_SECRET`, `ZETA_NODE_SECRET_<ID>`, `ZETA_KIWI_PASSWORD_<ID>`, `ZETA_STT_NODE_SECRET`, `ZETA_LLM_KEY` among them). A `gain=DB` item in `ARGS` is zeta's own: automatic gain is turned off and the overall receive gain set to `DB` dB when the device opens (logged with the device's range as `zeta: gain for <id>: ...`), and a `DB` outside that range refuses to attach the source (exit 1); without it the driver's default gain is used, which on a USRP is its minimum. A gain set live with `source_set_gain` is persisted and wins over `gain=` on the next start. `ppm=N` corrects the device's frequency error: the hardware is tuned `N` parts per million higher than every frequency zeta reports, so a device that reads high by `N` ppm (a carrier on 146.975 MHz seen at 146.9754) reads true. `zeta soapy find [--filter ARGS]` lists the devices attached to this machine as JSON. Only in a zeta built with `RUSTFLAGS=\"--cfg soapy\"` (needs libsoapysdr-dev); otherwise both exit 1 saying so.
- `--soapy-in-process`: open `--source-soapy` devices inside the node instead of a worker child; for diagnosis only, as a driver crash then ends the node.
- `--ring-budget-mb N` (env `ZETA_RING_BUDGET_MB`): memory budget for one source's IQ history (default a quarter of RAM, at most 4 GiB). The ring keeps 10 s at narrow rates and shrinks to as little as 1 s at wide ones (a ring holds 8 bytes per sample, so 16 Msps gets 8 s of a 1 GiB budget); `iq read` and `classify` spans older than that fail `too_old`. The size chosen is logged to stderr as `zeta: ring for <source>: <s> s at <rate> Hz (<MB> MB)`.
- `--knowledge-acma URL[,reach_km=N]` (env `ZETA_KNOWLEDGE_ACMA`): attach the ACMA licence register service (an MCP server over HTTP): the service's `http://HOST:PORT` (a trailing `/mcp` is accepted) as the `acma` knowledge provider of `knowledge_lookup` and `classify`. `reach_km=N` (a positive number) replaces the reach rule's kilometres. The service is probed once at startup (one-shot commands pay at most 5 s when the host is unreachable); an unreachable one is logged as `zeta: knowledge acma: <url> unreachable: <e>` and the provider still attaches, giving no allocations until it answers. Needs `station set` for distances. It belongs to the serving node: with `--node` it is a usage error (exit 1).
- `--plugin KIND=PATH`: register the program at absolute `PATH` as stage kind `KIND` (lowercase, not a built-in kind). It is probed at startup; a failed probe exits 1 naming the kind and the reason (a `hello` of a protocol other than 1 or 2 is refused). A plugin runs with a cleared environment: only `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM` and `PYTHONPATH` pass through, and each `ZETA_PLUGIN_ENV_<NAME>` variable reaches it as `<NAME>`; nothing else (no `ZETA_SECRET`) does.
- `--modes-dir PATH`: the operator's decoder mode rows, one `*.json` file each, read after the built-in ones (default `$XDG_CONFIG_HOME/zetasdr/modes`, else `~/.config/zetasdr/modes`; a missing directory is no error). A row with a built-in mode's `id` replaces it. A row that does not parse or validate never stops the node: it is listed `broken` by `modes list` and reported at startup as `zeta: mode <id> (<file>): <field>: <error>`, and a broken file that meant to replace a built-in row leaves that row in force.

Sources and plugins are not persisted: repeat the flags, with the same ids and kinds, on every invocation, `serve` included. A source id may be used once across all the source flags. `source list` shows each source's capability record: `remote` is true for a KiwiSDR, a `--source-node` and a `--source-vrt` source, `push` false for SoapySDR devices, and `fault` names a source that has failed (a full Kiwi, an unplugged device); a job on a faulted source fails with that reason.

There are two ways to run:

- One-shot, `zeta <group> <verb> ...`: opens the node, applies one operation, prints the JSON result and exits (0 on success, 2 with a JSON error). It never resumes jobs: a job started one-shot is persisted as `running` and runs once `zeta serve` starts. `job start --then-tail MAX_FRAMES,TIMEOUT_MS` keeps that one-shot node up after the start, tails the job's audio output when it has one (a demodulator), else its first event stream, then stops the job and prints `[job_result, frames_result, {\"stopped\": true}]`; add `--keep` to leave it running (`{\"stopped\": false}`). If the job is not running (e.g. failed) it prints `[job_result, job_record]`, the record's `state` carrying `status` and `reason`, and exits 2; `stream tail` on its own only sees streams of jobs running in the same process, and `job attach` needs its input job running in the same node, so attach against `zeta serve` (`--node`). A one-shot `iq read` or `classify` reads the ring that the command itself fills from the source, waiting up to 5 s for it to cover the span. One-shot commands, read-only ones included, restore the pins and the gain of the sources they attach (so even a one-shot `source list` tunes a pinned radio). One-shot commands, like `zeta serve`, start and probe every `--plugin` program at startup, so with plugins configured even a one-shot `source list` costs one process start per plugin.
- `zeta serve`: resumes persisted jobs and keeps them running until Ctrl-C.
- Against a running node, `zeta --node URL --secret SECRET <group> <verb> ...` (or the `ZETA_NODE` and `ZETA_SECRET` variables): the operation is sent to that node over HTTP and applied there, so jobs started this way keep running; `--state-dir`, the `--source-*` flags, `--plugin` and `--modes-dir` belong to the serving node and are rejected (a usage error, exit 1); `--stt-socket`, `--stt-node` and `--soapy-in-process` belong to it too but are ignored. `zeta soapy find` always runs locally. `--then-tail` is not supported with `--node` yet (a usage error, exit 1): start the job, then `stream list` and `stream tail` against the node. Start that node with `zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) and a secret. `zeta-mcp --node URL` offers the same operations as MCP tools, and `zeta ask \"REQUEST\"` lets a local model call them.
- `zeta serve --listen PORT --llm-url URL --llm-model NAME` (the other `--llm-*` flags of `zeta ask` apply too) enables `POST /ask` with `{\"request\": \"TEXT\", \"max_turns\": 8, \"allow\": {\"stop_jobs\": false, \"replace\": false}}` (`max_turns` 1-32, default 8; `allow` optional, both false by default: without them the agent's `job_stop` and non-empty `replace` calls are refused as `not_allowed`): the node runs the agent against itself, so every operation it applies appears on the `ops` stream, and answers the transcript (`request`, `answer`, `calls`, `turns`) as `zeta ask` prints it, plus `allow` as the run had it and `turn_log`, one entry per model completion with its reasoning, content and call count. One run at a time (409 while one is in progress); 503 when serve has no `--llm-url`; a failed run is 502 with the error and the transcript so far; 504 after 300 s.
- `zeta serve --listen PORT --stt-socket PATH` (env `ZETA_STT_SOCKET`; default `$XDG_RUNTIME_DIR/voiced.sock` when that file exists) enables `POST /stt?lang=CODE` (`lang` defaults to `en`): the body is a 16 kHz mono `audio/wav` (at most 4 MiB), the node hands it to the speech daemon at that Unix socket and answers `{\"text\": \"TRANSCRIPT\"}`. The daemon is `local-voice` or any daemon speaking its protocol: one line per connection, `file:<absolute wav path>` answered `ok:<text>` or `error:<message>`, and `languages` answered `ok:en,de,...` (a daemon answering `error:` there is taken to know English only). Instead of a daemon, `--stt-node URL` (env `ZETA_STT_NODE`, secret in `ZETA_STT_NODE_SECRET`, never printed) forwards each request to that node's `POST /stt`; the two flags are exclusive, for every command (exit 1). A forwarded request carries `X-Zeta-Stt-Hop: 1`, and a node that would forward it again answers 508 (`stt forwarding loop`). `GET /stt/languages` answers `{\"backend\": NAME, \"languages\": [CODES]}` (503 without a backend). 415 when the body is not WAV, 413 when too large, 422 `{\"error\": \"unsupported\", \"language\", \"supported\"}` for a language the backend lacks, 502 when the daemon reports an error, 503 when serve has no backend or cannot reach it, 503 `{\"error\": \"busy\", \"message\", \"wait_ms\"}` when every one of the daemon's 4 slots stayed taken for 30 s (a node forwarding with `--stt-node` reports that as a `busy` drop, not an error), 504 when the daemon does not answer within 30 s of its turn.

While `zeta serve` runs it owns the state directory (it holds `serve.lock` there). Mutating one-shot commands (`job start`, `job attach`, `job retune`, `job stop`, `jobs stop`, `plan start`, `mix start`, `graph put`, `station set`, `station clear`, `source ensure`, `source pin`, `source unpin`, `source gain`) are then refused with an `other` error naming the server's pid; read-only commands still work. Use `--node` to change a node that `zeta serve` owns.

Every operation the node applies is appended to `ops.log` in the state directory (rotated at 8 MiB) with its caller's transport (read-only operations are logged without their result); `zeta ops tail --lines N` prints the last N (local only). Every `transcript` event of a `transcribe` job is appended to `transcripts.log` in the state directory (rotated at 8 MiB) with the job, its input, the source and the channel's centre; `zeta transcripts tail --lines N [--job ID]` prints the last N (local only).";

/// Hand-written sentence appended to an operation's section.
fn note(op: &str) -> Option<&'static str> {
    match op {
        "graph_put" => Some(
            "Fetch the current graph with `graph get`, edit it, and put it back; stage kinds and their params are listed under Stage kinds below and by `schema`. A put that changes only a demodulator's `squelch`/`squelch_db`, its guard's `guard_hz`/`guard_db`/`channel_hz`, the channel node's `wide_hz` (or `deviation_hz`/`sideband`) keeps the job re-expandable: a later capture change (another rate after a restart) re-expands it with those values carried. Any other change launches the graph as stored from then on.",
        ),
        "job_start" => Some(
            "`fm_demod` is narrowband FM voice; `wfm_demod` is wideband broadcast FM (mono, 50 us de-emphasis, 15 kHz audio; give it a band of about 200000 Hz). `am_demod` is AM voice (envelope detector, 5 kHz audio, slow AGC; give it a band of about 10000 Hz). `ssb_demod` is SSB voice (2800 Hz audio, slow AGC; `center_hz` is the suppressed-carrier (dial) frequency, not the middle of the voice band, and the band must cover both sides of it: give it about 6000 Hz). All four output 48 kHz mono audio: an audio frame's payload is `{\"sample_rate_hz\", \"channels\", \"samples\": N, \"peak\"}`, `channels` 1 here, the samples interleaved and `samples` the per-channel count (a frame without `channels` means 1). `fm_demod` shapes its audio as a voice receiver does (a 3.4 kHz low-pass and a 530 us de-emphasis after the discriminator: `audio_lowpass_hz` and `deemphasis_us` on its node, `null` for the flat audio a decoder wants), and every voice demodulator (`fm_demod`, `am_demod`, `ssb_demod`) takes `denoise: true` on its node to run RNNoise on its audio output (off by default; speech only; one 10 ms frame of delay the frames' spans account for; set with `graph put`, kept through a retune; a `transcribe` attached to such a job skips its own `denoise` stage). `fm_demod` and `am_demod` carry an automatic squelch: it opens 6 dB over the channel's tracked noise floor (never taken above -40 dBFS: with no fixed threshold `floor_db` reads at most -40) and closes after 300 ms under it. While closed the job publishes no audio (an attached transcriber idles; `--then-tail` tails the audio, so an idle channel's tail is empty) and the first block and each change publish a `squelch` event (`open`, `power_db`, `floor_db`) on the job's event stream. The demod node's params set it with `graph get` and `graph put`: `squelch_db` is an absolute threshold in dBFS, -120..=0 (`null`, the default, is the automatic one; a value outside the range fails the job with `squelch_db <v> out of range -120..=0`), and `squelch: false` turns it off. `aprs_decode`'s demodulator runs with the squelch off. `fm_demod` on a channel of a plan (`channel_plans`) carries an adjacent-channel guard: its squelch stays closed while a channel `guard_hz` away is more than `guard_db` (12) dB stronger, so a transmission on ch20 does not open ch59 or ch60. The `squelch` event carries `adjacent_db` and `guarded`; once guarded, the gate stays held until the neighbour falls 2 dB under `guard_db`. The job's `job_context` keeps the last `squelch` event under `squelch` (removed when a run has the squelch off), so a client that subscribes after the gate changed reads it there. `graph put` with `guard_hz: null` on the demod node turns it off. `pocsag_decode` decodes POCSAG pagers (512, 1200 and 2400 baud detected automatically) from a 12.5 kHz FM channel into one event per message (`address`, `function`, `kind`, `text`). `rds_decode` reads the RDS data of a broadcast FM station (give it the same 200000 Hz band as `wfm_demod`) into events carrying `pi`, `ps` (station name) and `rt` (radio text) as they complete; the first event takes a few seconds. `adsb_decode` decodes Mode S / ADS-B at 1090000000 Hz with `bandwidth_hz` 2400000 (anything under 2000000 is refused) by running dump1090 on the node's host; one event per decoded message (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`, `track_deg`), position and identity arriving in separate messages. When the host has no dump1090 the node's startup handler report names it (`zeta: handlers: dump1090: missing (apt install dump1090-fa; <url>) — modes: adsb_decode`), `modes_list` shows `adsb_decode` `unavailable` with that line as its `reason`, and `job_start` fails with `mode adsb_decode is unavailable: <reason>`. `acars_decode` decodes ACARS (VHF, AM, 2400 baud MSK) from a 10000 Hz channel on 131550000 Hz (worldwide primary), 131725000 / 131825000 (Europe), 130025000 / 130450000 / 131125000 (North America), 131450000 (Asia) into one event per CRC-clean block (`reg`, `flight`, `label`, `text`). `rs41_decode` decodes a Vaisala RS41 radiosonde anywhere in 400050000-406000000 Hz (`dfm_decode` and `m20_decode` a Graw DFM and a Meteomodem M20 alike) by running the rs1729 decoder on the node's host over NBFM audio: start it on the sonde's measured centre (its frequency varies by flight; never a raster) with `bandwidth_hz` 12500, one sonde per job, and each event carries `id`, `position` and the frame's GPS time as `t_ns`. `deviation_hz` is the FM peak deviation for `fm_demod` and `wfm_demod` (100-75000 Hz; `null` means the kind's default: 5000 Hz for `fm_demod`, right for standard voice FM, use 2500 for narrow 12.5 kHz channels; 75000 Hz for `wfm_demod`, right for broadcast FM). Every other kind takes `null`. `sideband` is `usb` or `lsb` for `ssb_demod` (`null` means `usb`; amateur convention is `lsb` below 10 MHz, `usb` above); every other kind takes `null`. On the CLI, omit `--deviation-hz` and `--sideband` for `null`. `scan` sweeps a band wider than one capture: the node steps the source across it in captures of at most 16 MHz, dwells 250 ms on each, and emits `spectrum` rows whose `center_hz` is that step's (frame `channel` `scan/<step index>`), wrapping until stopped. Alternate sweeps shift the step grid down by a quarter step (the odd sweep has at most one more step), so a step's centre and edges in one sweep fall a quarter of the way into a row of the next and every frequency is seen clean in one of two sweeps; `scan.alternate` in the job context gives `shift_hz` and `steps_shifted` (`null` for a one-step span). A scan on a local hardware receiver masks each row's centre notch (`notch_hz`, 50000 Hz) and its outer edges (`edge_mask`, 0.02 of the bins each side); every other source gets none. A row with masked bins carries `masked: [[lo, hi], ...]` (bin index ranges, `lo` inclusive, `hi` exclusive): those bins are no data, not low power; their `q` is still present. `scan.mask` reports the values in force (a step narrower than four notches gets no notch; a one-step scan does not alternate and gets no edge mask: nothing could fill it in), and `graph get`, edit the `fft` node's `notch_hz`/`edge_mask`, `graph put` changes them (keep `edge_mask` under 0.25, or the two grids' masks overlap). Of the scan's stored `fft` node only `notch_hz` and `edge_mask` are read: its `size`, `window` and `rows_per_s` are information only (the scan derives them per step), and a `graph put` whose graph has no `fft` node is re-expanded with the default mask on the next launch rather than refused. A scan time-shares the radio: while it runs, every other `job_start` and `source_ensure` on the source is a `conflict` naming it, decoders included, and `source_list` shows the step it is on as `current`. Stop the scan to decode. A scan's `job_context` holds `scan.steps` (how many captures one sweep takes), `scan.step_hz`, `scan.dwell_ms`, `scan.alternate` and `scan.mask`: report step counts from it, never from guesswork. `identify` is the thorough answer to \"what is this?\": started like a decoder (`job_start` with `kind: identify` on the box's centre and band), it holds a rigid lease on that band, measures it for 1000 ms (layer 1), scores every mode row against it and tries each ready row that fits, one at a time in score order, as an ordinary child job on the same band (`job_list` shows it, `source_list.holders` shows both while it runs) for the row's `identify.dwell_ms` (5000 ms for APRS, POCSAG and the sonde rows, 10000 for RDS and ACARS, 15000 for ADS-B). Each row it considers publishes one `candidate` event on its event stream: `mode`, `score`, `outcome` (`proved`, `no_frame`, `skipped_band` for a row wider than the band, `unavailable` for a row whose program is missing or whose job would not start), `evidence`, `summary` (`<mode>: <outcome>`) and `job` (the child's id, `null` when none ran). A proof ends the job `stopped` with `job_context.identify` = `{\"verdict\": {\"mode\", \"label\", \"summary\", \"evidence\", \"job\"}, \"attempts\": n}` and leaves that mode's job running under the id in `verdict.job`, already decoding; stop it with `job_stop` when done. A list exhausted with no proof ends it `stopped` with `verdict` `{\"mode\": null, \"label\": <layer-1 name>, \"features\": <the layer-1 record>}`: report that as unknown with those measured features. Stopping an `identify` job stops the child it is trying; a source that faults fails it with the fault as its reason. With `--then-tail`, an `identify` that ends with a verdict during the tail is a success: the candidates are printed and the proved job (`verdict.job`) is stopped, or left running with `--keep` (`{\"stopped\": false}`). `watch` keeps an eye on a band and decodes what appears in it: `job_start` with `kind: watch` on the band (400-406 MHz is `center_hz: 403025000`, `bandwidth_hz: 5950000`; put the centre off any channel of interest, the receiver's DC spike sits there). It holds a rigid lease on the band like `identify` and runs `fft → detect` over the capture: `detect` publishes a `signal` event (`state` `new` or `gone`, `center_hz`, `bandwidth_hz`, `snr_db`, `first_ns`, `last_ns`) for each run of bins that persists, and each new signal no other job's band overlaps gets an `identify` run on its measured centre, one at a time, best-scoring first, on the band the best-scoring fitting row expects (12500 Hz for the sondes). A proof leaves the decoder running: inside the watch's capture with `decode_on: capture` (the default; one radio does both), or with `decode_on: free` on a free source other than the watch's (`source_free`), the ids in `prefer` first, back in the capture when none takes it. A signal no row proves is identified again after 60000 ms while it persists, at most 3 times; a decoder whose signal is `gone` and that has sent no event for 120000 ms is stopped (a landed sonde). The watch's own events, `protocol: watch`, go on the same event stream: `state` `identifying` (`signal`, `job` the identify run), `decoding` (`mode`, `job`, `source`, `placed` `capture` or `free`; the `summary` says why it fell back to the capture), `unknown` (`features`, the layer-1 record: report it as unknown with them) and `lost`, each with `summary` and `t_ns`. `job_context.watch` holds `signals` (each with `tries`, `next_retry_ns`, `handled`), `decoding` (`mode`, `job`, `source`, `signal`, `since_ns`, `placed`) and `current` (the identify run in progress, else `null`). `detect` also honours a row's `masked` ranges (those bins take no part in a run). Tune it with `graph get`, edit the `detect` node's params, `graph put`: `threshold_db` (10), `persist_ms` (3000), `width_hz` (`[5000, 30000]`), `dc_mask_hz` (25000), `edge_mask` (0.05), `spurs_hz` (`[]`), `gone_ms` (30000), and the policy `decode_on`, `prefer` and `sondehub`. `sondehub` (`{callsign, lat, lon, alt_m, batch_ms, url}`, the last two optional: 5000 ms and SondeHub's telemetry endpoint) appends a `sink_sondehub` node to every decoder the watch keeps, fed by its events; without it nothing is uploaded. The `sondehub` block is checked like the detect params: a bad one is refused at `job_start` and `graph put`, naming `sondehub.<field>`, before anything starts or restarts. If the sink cannot be added to a kept decoder it runs without it (the `decoding` event's `summary` says so and its `decoding` entry carries `sink_error`); one that cannot be relaunched at all is `lost`. `sink_sondehub` can be put on any sonde job with `graph put` (an `event` edge from the decoder's event node): it sends the `rs41`, `dfm` and `m20` events that carry a `position` (with a numeric altitude), a `datetime` and a `frame` to SondeHub in a `PUT` every `batch_ms`, and `job_context.sondehub` holds `sent`, `failed` and `last_error` (a failed batch is counted and dropped, not retried). The identify runs and their trial jobs are the watch's (`owner` is its id in `job_list`) and leave no history; the decoders it keeps are ordinary jobs. `job_stop` on the watch stops its identify run and every decoder it keeps, and so does a watch that ends `failed`; a `graph put` on it or a node restart re-adopts them instead, stopping any whose signal does not reappear within `gone_ms`. A `job_start` or `graph put` with an `edge_mask` outside 0..0.5, a `width_hz` not ordered `[min, max]`, a bad `sondehub` or a `prefer` naming a source not in `source_list` (`prefer[<i>]: unknown source <id>`) is refused naming the field, before anything starts or restarts. `transcribe` is not started on the radio: `job_start` refuses it; use `job_attach`. `kind` also takes the id of any ready mode row (`modes_list`), an operator's own rows included; an id that is neither a built-in kind nor a row's fails with an `unknown` error (`field: \"kind\"`, `known` the built-in kinds and every row id), a row that is not ready with `mode <id> is <state>: <reason>`. `elastic: true` (spectrum only) makes the job follow the source's capture instead of holding it: it never conflicts, is relaunched on every retune and is listed under `elastic` in `source_list`; a rigid request inside the capture elastic jobs follow is granted on it without a retune, one outside it retunes the source; the web page's own spectrum job is elastic. `replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder. An identical rigid start (same source, kind, band, `deviation_hz` and `sideband`) on a running job is refused as `duplicate` naming that job; stop it or use it.",
        ),
        "source_ensure" => Some(
            "`replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder.",
        ),
        "source_pin" => Some(
            "Hold the source at a band without a job: a rigid holder that is not a job, so `replace` cannot name it and a `conflict` offers `unpin source <id> (source_unpin)`. A pin retunes the source to its band (the page's spectrum follows) unless a decoder holds the source inside that band, when it joins the current capture. Persisted; restored before anything else on a restart. Put `center_hz` between two channels of the grid you listen to so the receiver's DC spike falls in a guard band (the UHF CB plan's midpoint 476918750 Hz with 1000000 Hz holds all 80 channels).",
        ),
        "station_set" => Some(
            "A node-level record, set once: the position, region and antennas the node listens from. The ACMA provider's distance and reach, `sink_sondehub`'s defaults and the page's home read it. `antennas: null` takes the defaults (a 0-30 MHz dipole and a 30 MHz-6 GHz discone, both 5 m up); an antenna is `{id, kind, min_hz, max_hz, height_m, azimuth_deg, elevation_deg}` with `kind` one of `discone`, `dipole`, `skyloop`, `vertical`, `yagi`, `dish`, `other` and the two angles `null` for an omnidirectional one. A bad value is refused naming the field (`lat`, `antennas[<i>].<field>`), with nothing written. On the CLI, `--region` defaults to `AU` and each `--antenna KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]` adds one to the defaults (its id is the kind, numbered when taken); a declared antenna wins over a default on its own band. `station_get` answers the station or `null`; `station_clear` forgets it.",
        ),
        "source_unpin" => Some("Release the pin; the source idles if nothing else holds it."),
        "source_free" => Some(
            "Ask which radio could take a band before starting a job on it, then start the job on one of the `free` ids. A free source is a suggestion, not a reservation: nothing is leased, and the next `job_start` may still conflict. A source on another node (`--source-node`) can look free here while that node runs its own jobs on it; that surfaces as a `conflict` on `job_start`.",
        ),
        "source_set_gain" => Some(
            "Set the source's overall receive gain live (`gain_db` in dB within `source list`'s `gain.min_db..=max_db`), or `null` for the driver's automatic gain where `gain.automatic_available`. Persisted: it is re-applied when the node restarts and wins over `gain=` in the device args. A source without adjustable gain (a recording, a KiwiSDR, another node) answers `source has no adjustable gain`. On the CLI, `--auto` passes `null`.",
        ),
        "job_attach" => Some(
            "Attach a processing chain to a running job's output instead of the radio: `transcribe` reads the job's audio (any demodulator, later any digital voice decoder), cleans it (denoise, voice gate) and emits one `transcript` event per utterance (`text`, `language`, `start_ns`, `end_ns`) plus `vad` events as speech starts and stops; it holds no lease, follows its input through a `job_retune` or a `graph_put` (relaunched under its own id) and ends when the input job stops. `language` is a BCP-47 code (`null` means `en`; the backend's languages are in `GET /stt/languages`). To transcribe a station: `job_start` a `wfm_demod` on it, then `job_attach` with `kind: transcribe` and that job's id. The attached job fails with a reason naming the speech backend when the node has none or it cannot serve the language. Every other kind (`identify`, `watch`, the demodulators and decoders) runs on the radio and is refused here as `<kind> is not attachable`.",
        ),
        "job_retune" => Some(
            "`replace`: as on `job_start`, the holders that may be stopped to make room, stopped under the gate right before the request; a `conflict` offers `retry with replace: [<ids>]` and the page shows the same retry buttons. A demodulator keeps its `squelch`, `squelch_db`, `deviation_hz` and `sideband` through a retune; its adjacent-channel guard (`guard_hz`, `guard_db`, `channel_hz`, the channel node's `wide_hz`) is derived again from the channel plan at the new band, never carried, so a width preset changes what the filter passes and a move to a neighbouring channel takes that channel's geometry; a guard turned off with `graph_put` (`guard_hz: null`) comes back on a retune onto a plan channel. Every attached job (`job_attach`) reading the retuned job is relaunched under its own id once the input runs again (one that cannot relaunch is `failed` with the reason; the retune still succeeds). Centre the band on the signal's measured centre (`measure`, `classify`), never on a raster: see Fitting a demod to a signal under Field craft. A one-shot `zeta job retune` cannot reach a job `zeta serve` runs (`job <id> is not running in this node`): use `--node`. On the CLI, `--replace ID[,ID]`.",
        ),
        "job_stop" => Some(
            "Stopping a job is the operator's decision: never `job_stop` a job the request did not name, except one you started in this run (`job_start`, `plan_start`, `job_attach`, or the `verdict.job` of your own `identify` once you have read it): stop your own `measure` and `identify` leftovers when done. Through `ask` any other stop needs the run to allow stops, else it is refused as `not_allowed`: report that, do not retry.",
        ),
        "jobs_stop" => Some(
            "One call however many jobs match; the page's elastic spectrum keeps running. Stopping jobs is the operator's decision: use it only when the request asks for all of them, or names the source, kind or plan. Through `ask` the run must allow stops, else the call is refused as `not_allowed`: report that, do not retry. Report how many stopped and any `failed`. A mix stops only with no filter or `kind: mix`.",
        ),
        "mix_start" => Some(
            "Starts one `mix` job that plays the audio of every job on `sources` (`null`: every source) as one stream, so whatever is talking is heard. It holds no lease, tunes nothing and blocks no job; jobs started or stopped later join and leave by themselves. Its `event` stream publishes `active` (who is talking: `job`, `source`, `center_hz`) when it changes, with a hangover after a channel falls silent. It is idempotent on the same set of sources: a mix of that set already running answers its id (`null` matches only `null`). To mute a channel, `graph get` the mix, edit `muted` on the `mix` node (a list of `{source, center_hz}`), `graph put`, which restarts it under its id.",
        ),
        "plan_start" => Some(
            "Starts `kind` on the channels of a plan (`channels`, `null` for all of them) at each channel's `center_hz` with the plan's `bandwidth_hz`, under one lease decision for the band that bounds them: inside the current capture nothing retunes, otherwise the source is tuned when only elastic jobs hold it, and a scan or a rigid holder elsewhere is a `conflict` naming the holders with nothing started. Check `source_list` first. `kind` must run on the radio (not `transcribe`, `spectrum`, `scan`, `watch` or `identify`). `deviation_hz` and `sideband` are passed to every start as on `job_start` (`null` is the kind's default; give 2500 for `fm_demod` on 12.5 kHz channels). `attach` is `null` or `transcribe`, attached to every channel started or already running, and needs a ready stage (a speech backend for `transcribe`), else the op is refused with the stage's reason: start without `attach`. `language` must be `null` without `attach`. `replace`: as on `job_start`. The result lists `started` (channel, `center_hz`, `job`, `attached`, `attach_error`), `existing` (channels an identical running job already holds, skipped), `refused` (channels whose own start failed, with the error; the others went on) and `warnings`: repeat the `warnings` to the operator. Run it again to complete a partial set: running channels are `existing`, so nothing doubles.",
        ),
        "job_list" => Some(
            "Stopped and failed jobs are kept as history, newest 20 only (older ones are forgotten with their files); a running job is never dropped. Through `ask` (the agent transport) each record is returned without its `graph`; `graph_get` fetches one.",
        ),
        "knowledge_lookup" => Some(
            "Two providers answer: the built-in seed signatures, and `catalogue`, the 594-signal iNTERCEPT catalogue (Apache-2.0; one signature per frequency range of each signal overlapping the band, `bandwidth_hz` its widest bandwidth, `modulation` the first it lists). A signature's `decoder` is the id of the mode (`modes_list`) that proves it, the `kind` to start on the band to decode it; `null` means no mode row proves it yet. `classify` runs only the modes with a quick attempt (APRS, POCSAG, ACARS, RDS) as decode-to-identify. The built-in two have no allocations. With `--knowledge-acma` a third provider, `acma`, answers: the register's plan services and assignments overlapping the band, each with licensee, site, distance and bearing from the station, power, role, the decoded emission designator and `reach` (whether a signal from that site can plausibly arrive here). It needs the station (`station set`): without one it gives no distances. An unreachable service gives no allocations, so an empty `allocations` list means no provider data, not nothing allocated.",
        ),
        "channel_plans" => Some(
            "`channel_plans` lists the channel plans the node knows (`uhf_cb_au`: Australian UHF CB, 80 channels, two rasters 12.5 kHz apart). Name a channel by its number to the user (\"ch20\") and start jobs at its `center_hz` with the plan's `bandwidth_hz`.",
        ),
        "modes_list" => Some(
            "The decoder modes the node knows, one row each: `id` (the `kind` a decoder job starts with), `name`, `family`, `band` (`bandwidth_hz` the decoder expects, well-known `centers_hz`, `min_hz` the narrowest band it accepts or `null`, `range_hz` the span `[lo, hi]` it is found anywhere in or `null`), `modulation` (the layer-1 labels it presents as), `decoder_kind` (`graph`, run in-process, or `program`, an external decoder), `file` (`modes/<id>.json` for a built-in row, the path for an operator row), `program`, `checked` (UNIX seconds at which `zeta modes check` last passed the row on this host; `null` if never or for a graph row), `subject` (the event field naming what events are about, its table columns and how the page folds it; `null` for none), and `state` ready|broken|unavailable with `reason` (`null` when ready). A broken row's `file` is the file to fix and its `reason` names the field (`<field>: <error>`); an unavailable one names the program it lacks. `family` (and the `family` filter) is one of `tv`, `broadcast`, `aviation`, `maritime`, `satellite`, `two_way`, `cellular_rail`, `pagers`, `iot`, `weather_science`, `amateur_digital`, `amateur_voice`, `hf_utility`, `general`. On the CLI, `--family` and `--state` narrow the list. `zeta modes check [ID] [--strict]` (local only, not with `--node`) runs each row's test on a node over a synthesised capture and prints `<id>: ok`, `<id>: fail: <reason>` or `<id>: skipped: <reason>` (no test, or the program missing: `--strict` makes that a fail); it exits 0 when no row failed.",
        ),
        "modes_show" => Some(
            "One mode's full row as loaded (`row`: band, decoder graph or program block (an audio program row adds its `demod` and `input.container`), event schema and `summary` template (`{field:.N}` and `{field:+.N}` format numbers), `identify` rule, catalogue ids, `position` (the event fields of a fix, added to each event as `position: {lat, lon, alt_m}`; `alt_unit` `ft` converts a feet field), `subject` (`key`, `name`, `waterfall` `summary` or `fold`, `columns` of `field`, `label` and `unit`, `heading`, `label`) and `time` (the event field whose ISO 8601 UTC time becomes `t_ns`), each `null` for none) with its `state`, `reason`, `file`, `program` and `checked`. An unknown id is an `unknown` error (`field: \"id\"`) listing the known ids in `known`; take ids from `modes list`, never invent them.",
        ),
        "classify" => Some(
            "Layer 1 labels the measured band (`wfm`, `fm`, `am`, `ssb`, `fsk`, `carrier`, `noise` or `unknown`; an `ssb` label's evidence gives the dial frequency). Layer 2 first sweeps the decoder modes (`modes_list`) as decode-to-identify: each ready or unavailable row is scored against the layer-1 measurement (0.4 for the measured centre within `band.bandwidth_hz` of one of its `band.centers_hz` or inside its `band.range_hz`, or always when it has neither; 0.3 for a measured bandwidth within 0.5-1.5 times its `band.bandwidth_hz`; 0.3 for the layer-1 label in its `modulation`; plus its best catalogue score, below) and the best 5 are considered in score order (ties by `identify.priority`, highest first, then `family`, then `id`). The result is `{labels, tried}`: `tried` lists those 5 as `{mode, score, outcome}`, `outcome` one of `proved` (its quick decode passed its CRC/BCH check), `no_frame` (tried, nothing decoded), `skipped_span` (the span is shorter than the row's `identify.min_span_ms`, 1000 ms for the built-in rows), `skipped_quick` (the row has no quick attempt: a program row such as `adsb_decode` is tried only by identify, never by `classify`; use `adsb_decode`), `skipped_band` (the selection is narrower than the row's `band.min_hz`, else its `band.bandwidth_hz`: `rds_decode` needs 150000 Hz, `adsb_decode` 2000000; on a whole block the measured occupied band counts, read as 150000 Hz once it reaches 100000) or `unavailable` (its program is missing). A proof adds a layer-2 label named by the row's `label` (`APRS`, `POCSAG`, `ACARS`, `RDS`) at confidence 1.0, evidence `decode <mode>: ...`; a protocol is labelled only then. A `no_frame` does not prove the protocol absent: the span may have held no frame. Layer 2 also adds up to three signal names from the catalogue (evidence `catalogue <id>: ...`), scored 0.4 for the measured centre lying in one of the signal's frequency ranges (required), 0.3 for a matching bandwidth (required for signals wider than 50 kHz) and 0.3 for a matching layer-1 modulation. Layer 3 is an empty slot.",
        ),
        _ => None,
    }
}

/// An operation's example value for a top-level field, when the type's first value
/// would be wrong (only `transcribe` can be attached).
fn example_for(op: &str, field: &str) -> Option<Value> {
    match (op, field) {
        ("job_attach", "kind") => Some("transcribe".into()),
        ("plan_start", "plan") => Some("uhf_cb_au".into()),
        ("plan_start", "attach") => Some("transcribe".into()),
        _ => None,
    }
}

/// Where a field's value comes from, when the type alone does not say.
fn hint(field: &str) -> Option<&'static str> {
    match field {
        "source" => Some("an id from `source list`"),
        "plan" => Some("an id from `channel_plans`"),
        "sources" => Some("ids from `source list` (strings), `null` for every source"),
        "channels" => Some("channel numbers of the plan (integers), `null` for all"),
        "job" => Some("an id from `job list`"),
        "stream" => Some("an id from `stream list`"),
        "replace" => Some("ids from `job list` (strings), `[]` for none"),
        "region" => Some("free-text region code such as `AU`"),
        "id" => Some("an id from `modes list`"),
        _ => None,
    }
}

/// Unit label of a named integer type, or of a field named for its unit
/// (`_db`: decibels), stated explicitly.
fn unit(name: &str) -> Option<&'static str> {
    match name {
        "Hz" => Some("Hz"),
        "Baud" => Some("baud"),
        "Timestamp" => Some("ns since epoch"),
        n if n.ends_with("_db") => Some("dB"),
        _ => None,
    }
}

/// Render the guide from the node schema (`op`, `result`, `error`, `stages`)
/// and the field-craft text.
pub fn render(schema: &Value, field_craft: &str) -> String {
    let op = &schema["op"];
    let defs = &op["$defs"];
    let mut out = format!("# ZetaSDR agent guide\n\n{INTRO}\n\n## Rules\n\n");
    for r in RULES {
        out.push_str(&format!("- {r}\n"));
    }
    out.push_str(&format!("\n## Running the node\n\n{RUNNING}\n"));
    out.push_str("\n## Operations\n");
    for variant in op["oneOf"].as_array().into_iter().flatten() {
        let name = variant["properties"]["op"]["const"].as_str().unwrap_or("?");
        let fields: Vec<(&str, &Value)> = variant["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|f| *f != "op")
            .map(|f| (f, &variant["properties"][f]))
            .collect();
        out.push_str(&format!("\n### {name}\n\n"));
        if let Some(d) = variant["description"].as_str() {
            out.push_str(&format!("{}\n\n", d.replace('\n', " ")));
        }
        for (f, s) in &fields {
            let h = hint(f).map(|h| format!("; {h}")).unwrap_or_default();
            let u = unit(f).map(|u| format!(" ({u})")).unwrap_or_default();
            out.push_str(&format!("- {f}: {}{u}{h}\n", type_of(s, defs)));
        }
        if !fields.is_empty() {
            out.push('\n');
        }
        let mut json = format!("{{\"op\":\"{name}\"");
        for (f, s) in &fields {
            let ex =
                example_for(name, f).map_or_else(|| example_json(f, s, defs), |v| v.to_string());
            json.push_str(&format!(",\"{f}\":{ex}"));
        }
        json.push('}');
        out.push_str(&format!(
            "JSON: `{json}`\n\nCLI: `{}`\n",
            cli_form(name, &fields, defs)
        ));
        if let Some(n) = note(name) {
            out.push_str(&format!("\n{n}\n"));
        }
    }
    out.push_str("\n## Stage kinds\n\nA graph node's `kind` names one of these; its `params` must match the listed fields (an optional one may be left out for its default).\n");
    for st in schema["stages"].as_array().into_iter().flatten() {
        let list = |k: &str| -> String {
            let v: Vec<&str> = st[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            v.join(", ")
        };
        let ps = &st["params_schema"];
        out.push_str(&format!(
            "\n### {}\n\n- inputs: {}\n- outputs: {}\n",
            st["kind"].as_str().unwrap_or("?"),
            list("inputs"),
            list("outputs")
        ));
        let req = required(ps);
        for p in &req {
            out.push_str(&format!(
                "- params.{p}: {}\n",
                type_of(&ps["properties"][p], &ps["$defs"])
            ));
        }
        // A param with a schema default may be left out (older graphs lack it).
        for (p, s) in ps["properties"].as_object().into_iter().flatten() {
            if let (false, Some(d)) = (req.contains(&p.as_str()), s.get("default")) {
                out.push_str(&format!(
                    "- params.{p}: {} (optional, default {d})\n",
                    type_of(s, &ps["$defs"])
                ));
            }
        }
    }
    out.push_str("\n## Field craft\n\n");
    out.push_str(field_craft);
    out
}

/// The `$defs` entry a `$ref` points at, with its name.
fn resolve<'a>(s: &'a Value, defs: &'a Value) -> (Option<&'a str>, &'a Value) {
    match s["$ref"].as_str().and_then(|r| r.strip_prefix("#/$defs/")) {
        Some(n) => (Some(n), &defs[n]),
        None => (None, s),
    }
}

fn required(def: &Value) -> Vec<&str> {
    def["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn type_of(s: &Value, defs: &Value) -> String {
    let (name, def) = resolve(s, defs);
    // A nullable scalar (`["integer","null"]`) reads as `integer or null`.
    if let Some(a) = def["type"].as_array() {
        let t: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
        return t.join(" or ");
    }
    let ty = def["type"].as_str().unwrap_or("any");
    match (ty, name) {
        ("integer", Some(n)) => format!("integer ({})", unit(n).unwrap_or(n)),
        ("string", Some(n)) if def["enum"].is_array() => {
            let vals: Vec<&str> = def["enum"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            if n == "JobKind" {
                // The static list holds the built-ins and the embedded rows; a node's
                // own `schema` adds the operator rows it loaded.
                format!(
                    "string ({n}: {}; the built-in kinds and the embedded mode rows, a node's own `schema` adds its operator rows)",
                    vals.join("|")
                )
            } else {
                format!("string ({n}: {})", vals.join("|"))
            }
        }
        ("object", Some(n)) => {
            let parts: Vec<String> = required(def)
                .into_iter()
                .map(|p| {
                    let (_, pd) = resolve(&def["properties"][p], defs);
                    let (item_name, item) = resolve(&pd["items"], defs);
                    if pd["type"] == "object" {
                        let subs = required(pd);
                        let units: Vec<Option<&str>> = subs
                            .iter()
                            .map(|q| resolve(&pd["properties"][*q], defs).0.and_then(unit))
                            .collect();
                        let tail = match units.first() {
                            Some(Some(u)) if units.iter().all(|x| *x == Some(*u)) => {
                                format!(": integer ({u})")
                            }
                            _ => String::new(),
                        };
                        format!("{p}{{{}{tail}}}", subs.join(","))
                    } else if let (Some(n), "object") =
                        (item_name, item["type"].as_str().unwrap_or(""))
                    {
                        format!("{p}[{n}: {}]", required(item).join(", "))
                    } else {
                        p.to_string()
                    }
                })
                .collect();
            format!("object ({n}: {})", parts.join(", "))
        }
        _ => ty.to_string(),
    }
}

fn example(field: &str, s: &Value, defs: &Value) -> Value {
    let (name, def) = resolve(s, defs);
    match def["type"].as_str() {
        _ if name == Some("NodeId") => "<node-id>".into(),
        Some("integer") => match (field, name) {
            ("bandwidth_hz", _) => 12500.into(),
            (_, Some("Hz")) => 144800000u64.into(),
            ("start_ns", _) => 1760000000000000000u64.into(),
            ("end_ns", _) => 1760000001000000000u64.into(),
            ("max_frames", _) => 1.into(),
            ("timeout_ms", _) => 5000.into(),
            _ => 0.into(),
        },
        Some("string") => match def["enum"].as_array().and_then(|a| a.first()) {
            Some(first) => first.clone(),
            None if field == "region" => "AU".into(),
            None if field == "kind" => "passthrough".into(),
            None => "<id>".into(),
        },
        Some("object") => {
            let mut m = serde_json::Map::new();
            for p in required(def) {
                m.insert(p.into(), example(p, &def["properties"][p], defs));
            }
            Value::Object(m)
        }
        Some("array") if field == "nodes" => Value::Array(vec![example("node", &s["items"], defs)]),
        Some("array") => Value::Array(vec![]),
        Some("boolean") => false.into(),
        _ if field == "params" => serde_json::json!({"kind": "iq"}),
        _ => Value::Null,
    }
}

/// Example value as JSON text, object keys in schema (required) order.
fn example_json(field: &str, s: &Value, defs: &Value) -> String {
    let (_, def) = resolve(s, defs);
    if def["type"] != "object" {
        return example(field, s, defs).to_string();
    }
    let parts: Vec<String> = required(def)
        .into_iter()
        .map(|p| format!("\"{p}\":{}", example_json(p, &def["properties"][p], defs)))
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// Flag name and shell-ready value for each leaf the CLI takes.
fn cli_flags(field: &str, s: &Value, defs: &Value, out: &mut Vec<(String, String)>) {
    let (name, def) = resolve(s, defs);
    match (field, name) {
        ("selection", _) | ("span", _) => {
            for p in required(def) {
                if p == "span" {
                    cli_flags(p, &def["properties"][p], defs, out);
                } else {
                    out.push((p.replace('_', "-"), value(p, &def["properties"][p], defs)));
                }
            }
        }
        ("graph", _) => out.push(("file".into(), "graph.json".into())),
        _ => out.push((field.replace('_', "-"), value(field, s, defs))),
    }
}

fn value(field: &str, s: &Value, defs: &Value) -> String {
    match example(field, s, defs) {
        Value::String(t) => t,
        v => v.to_string(),
    }
}

fn cli_form(op: &str, fields: &[(&str, &Value)], defs: &Value) -> String {
    // The CLI names this one for what it sets; `--auto` stands for `null`.
    if op == "source_set_gain" {
        return "zeta source gain --source <id> --db 40".into();
    }
    if op == "channel_plans" {
        return "zeta plans [--plan ID]".into();
    }
    if op == "modes_list" {
        return "zeta modes list [--family F] [--state S]".into();
    }
    if op == "modes_show" {
        return "zeta modes show <id>".into();
    }
    if op == "jobs_stop" {
        return "zeta jobs stop [--source ID] [--kind KIND] [--plan ID]".into();
    }
    if op == "station_set" {
        return "zeta station set --lat -27.38 --lon 153.12 --alt-m 4 [--region AU] [--antenna KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]]...".into();
    }
    if op == "plan_start" {
        return "zeta plan start --plan uhf_cb_au --kind fm_demod --source <id> [--channels 1,2,5-8] [--attach transcribe] [--language de] [--replace ID,ID]".into();
    }
    let mut line = String::from("zeta ");
    line.push_str(&match op.split_once('_') {
        Some((g, v)) => format!("{g} {v}"),
        None => op.to_string(),
    });
    let mut flags = vec![];
    for (f, s) in fields {
        // A nullable field is an optional flag; the example omits it.
        if s["type"]
            .as_array()
            .is_some_and(|a| a.contains(&"null".into()))
        {
            continue;
        }
        // A boolean is a switch (`--elastic`); the example's `false` omits it.
        if s["type"] == "boolean" {
            continue;
        }
        // A list flag (`--replace ID[,ID]`); the example's empty list omits it.
        if s["type"] == "array" {
            continue;
        }
        match example_for(op, f) {
            Some(Value::String(v)) => flags.push((f.replace('_', "-"), v)),
            _ => cli_flags(f, s, defs, &mut flags),
        }
    }
    for (flag, v) in flags {
        line.push_str(&format!(" --{flag} {v}"));
    }
    line
}

/// The full AGENTS.md text for this build.
pub fn rendering() -> String {
    let mut schema = zeta_core::ops::schema();
    schema["stages"] = crate::node_setup::registry(None, &Default::default(), None).describe();
    render(&schema, FIELD_CRAFT)
}

/// The Claude Code skill: front matter naming when to use it, then the full guide.
pub fn render_skill(md: &str) -> String {
    format!(
        "---\nname: zetasdr\ndescription: Use when driving a ZetaSDR node (sources, jobs, IQ captures, classification, knowledge lookup) through the zeta CLI or the zeta-mcp tools.\n---\n\n{md}"
    )
}

/// Print the rendering, or with `check` compare it to the committed file
/// (`AGENTS.md`, or `skills/zetasdr/SKILL.md` with `skill`).
pub fn run(check: bool, skill: bool) -> ExitCode {
    let guide = rendering();
    let (text, path, name, regen) = if skill {
        (
            render_skill(&guide),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills/zetasdr/SKILL.md"),
            "skills/zetasdr/SKILL.md",
            "zeta agents-md --skill > skills/zetasdr/SKILL.md",
        )
    } else {
        (
            guide,
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../AGENTS.md"),
            "AGENTS.md",
            "zeta agents-md > AGENTS.md",
        )
    };
    if !check {
        print!("{text}");
        return ExitCode::SUCCESS;
    }
    match std::fs::read_to_string(path) {
        Ok(on_disk) if on_disk == text => ExitCode::SUCCESS,
        Ok(on_disk) => {
            let line = text.lines().zip(on_disk.lines()).position(|(a, b)| a != b);
            let at = line.unwrap_or_else(|| text.lines().count().min(on_disk.lines().count())) + 1;
            eprintln!(
                "zeta: {name} differs from the rendering (first difference at line {at}); regenerate with `{regen}`"
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("zeta: reading {path}: {e}");
            ExitCode::FAILURE
        }
    }
}
