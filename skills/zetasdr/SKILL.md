---
name: zetasdr
description: Use when driving a ZetaSDR node (sources, jobs, IQ captures, classification, knowledge lookup) through the zeta CLI or the zeta-mcp tools.
---

# ZetaSDR agent guide

ZetaSDR is an agentic SDR runtime. You drive it through a small typed operation set. The CLI form and the JSON form of every operation are equivalent: `zeta <group> <verb> --flag value` and `{"op": "<name>", ...}` do the same thing.

## Rules

- One unit per field: frequencies and bandwidths are integers in hertz in fields ending `_hz`, rates are in baud in fields ending `_baud`, times are integer nanoseconds since the UNIX epoch in fields ending `_ns`, durations are integer milliseconds in fields ending `_ms`, gains are decibels in fields ending `_db`.
- Every field is required and unknown fields are rejected.
- Call operations, do not explain.
- Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it.
- Unknown signals are reported as unknown, never guessed.
- Ids are never invented: `source` values come from `source list`, `job` values from `job list`.
- Jobs belong to the operator: never `job_stop` a job or put one in `replace` unless the request names it, the run allows it or, for `job_stop`, you started it in this run. A `conflict` is reported with its holders; the operator decides.
- For a kind on every channel of a plan use `plan_start`, never one `job_start` per channel.
- Plan ids come from `channel_plans`: read it before naming a plan. A band with no plan takes one `job_start` per measured station; never invent a plan.
- To stop every job, or every job of a source, kind or plan, use `jobs_stop`, never one `job_stop` per job.

## Running the node

Every command takes these global flags:

- `--state-dir PATH`: where jobs and captures persist (default `$XDG_STATE_HOME/zetasdr`, else `~/.local/state/zetasdr`). Pass the same directory on every invocation.
- `--source-file ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]`: attach a SigMF recording as source `ID`. With the range it plays as a radio tunable over `N`-`M` Hz (its `tune_min_hz`/`tune_max_hz` in `source list`): a band inside the range moves the capture's centre there, the samples staying the recording's (a test and demo aid); without it only the recording's own band is accepted.
- `--source-kiwi ID=HOST:PORT[/PASSWORD]`: attach a KiwiSDR receiver (0-30 MHz, at most 12 kHz wide) as source `ID`; the port is required and the password is never printed. Without `/PASSWORD` the password comes from `ZETA_KIWI_PASSWORD_<ID>` (`ID` upper-cased, other characters as `_`), which keeps it out of `ps`; the flag wins when both are given.
- `--source-node ID=SOURCE@URL[,max_bandwidth_hz=N]`: use source `SOURCE` of another ZetaSDR node, `URL` being its `http://HOST:PORT` (a `zeta serve --listen` node), as source `ID`. The node's secret comes from `ZETA_NODE_SECRET_<ID>` (`ID` mapped as for Kiwi passwords) and is never printed; without it the command is a usage error (exit 1). A remote source ships narrowband by default: `bandwidth_hz` is capped at 250 000 Hz (or the remote source's own maximum when smaller) unless `max_bandwidth_hz=N` (1-20 000 000) widens it explicitly. Each tune holds a `passthrough` job on the other node, so its `source list` and `job list` show who is using the source; the job is stopped when the tuning changes or this process exits normally (a killed process leaves it running; stop it there with `job stop`).
- `--source-vrt ID=udp://ADDR:PORT`: receive VITA 49.2 (VRT) packets, one per UDP datagram, on `ADDR:PORT` as source `ID`; `ADDR` must be a LAN address (`0.0.0.0` listens on every interface). The sender sets the frequency and rate: a job's band must lie inside the sender's last context packet (`source list` shows it as `current`; a job started before any context packet waits up to 1.5 s for one), otherwise the job fails with `unsupported` saying where the sender is; retune the sender, not the job. Packets that do not decode are skipped.
- `--source-soapy ID=ARGS`: attach a SoapySDR device as source `ID`, `ARGS` being SoapySDR device arguments such as `driver=hackrf,serial=...`. The device runs in a `zeta soapy-worker` child the node spawns and respawns: a driver crash or an unplugged device faults the source (`source list` shows `fault`, jobs on it fail with `source <id>: <fault>`) and never ends the node. The node then respawns the worker and waits for the device; when it is back it restores the pin and the gain and relaunches every job that failed with that fault under its own id, the `watch` and its decoders and an attached `transcribe` included, logging `zeta: source <id>: recovered: ...`. `worker` in `source list` shows the child's pid and how many times it was respawned. A device absent at startup is attached faulted when the host has seen it before, else the flag is refused (exit 1). `zeta soapy-worker --id ID --args ARGS` is what the node spawns; it is not run by hand. The worker inherits the node's environment (the driver reads `UHD_IMAGES_DIR`, `SOAPY_SDR_PLUGIN_PATH`) minus the secrets: every `ZETA_*` variable whose name contains `SECRET`, `PASSWORD` or `KEY` (`ZETA_SECRET`, `ZETA_NODE_SECRET_<ID>`, `ZETA_KIWI_PASSWORD_<ID>`, `ZETA_STT_NODE_SECRET`, `ZETA_LLM_KEY` among them). A `gain=DB` item in `ARGS` is zeta's own: automatic gain is turned off and the overall receive gain set to `DB` dB when the device opens (logged with the device's range as `zeta: gain for <id>: ...`), and a `DB` outside that range refuses to attach the source (exit 1); without it the driver's default gain is used, which on a USRP is its minimum. A gain set live with `source_set_gain` is persisted and wins over `gain=` on the next start. `ppm=N` corrects the device's frequency error: the hardware is tuned `N` parts per million higher than every frequency zeta reports, so a device that reads high by `N` ppm (a carrier on 146.975 MHz seen at 146.9754) reads true. `zeta soapy find [--filter ARGS]` lists the devices attached to this machine as JSON. Only in a zeta built with `RUSTFLAGS="--cfg soapy"` (needs libsoapysdr-dev); otherwise both exit 1 saying so.
- `--soapy-in-process`: open `--source-soapy` devices inside the node instead of a worker child; for diagnosis only, as a driver crash then ends the node.
- `--ring-budget-mb N` (env `ZETA_RING_BUDGET_MB`): memory budget for one source's IQ history (default a quarter of RAM, at most 4 GiB). The ring keeps 10 s at narrow rates and shrinks to as little as 1 s at wide ones (a ring holds 8 bytes per sample, so 16 Msps gets 8 s of a 1 GiB budget); `iq read` and `classify` spans older than that fail `too_old`. The size chosen is logged to stderr as `zeta: ring for <source>: <s> s at <rate> Hz (<MB> MB)`.
- `--bursts-keep-mb N` (env `ZETA_BURSTS_KEEP_MB`) and `--bursts-keep-days N` (env `ZETA_BURSTS_KEEP_DAYS`): how much the burst capture store keeps (defaults 256 MB and 7 days; zero is a usage error). Expiry removes the oldest unpinned captures until the unpinned rest fit the megabytes, then every unpinned capture written more than the days ago; it runs when `zeta serve` opens (a one-shot expires at open only when given one of these flags while no serve owns the state directory, so a plain one-shot never drops what serve's policy keeps) and after each write, and a capture just written is spared by its own write. A pinned capture is never expired. An unreadable `bursts/index.json` is moved aside as `index.json.broken-<ulid>` and the index rebuilt from the `.sigmf-meta` files (pins are lost), logged as `zeta: bursts: index unreadable (<e>); moved aside`. Both belong to the serving node: with `--node` they are a usage error (exit 1).
- `--knowledge-acma URL[,reach_km=N]` (env `ZETA_KNOWLEDGE_ACMA`): attach the ACMA licence register service (an MCP server over HTTP): the service's `http://HOST:PORT` (a trailing `/mcp` is accepted) as the `acma` knowledge provider of `knowledge_lookup` and `classify`. `reach_km=N` (a positive number) replaces the reach rule's kilometres. The service is probed once at startup (one-shot commands pay at most 5 s when the host is unreachable); an unreachable one is logged as `zeta: knowledge acma: <url> unreachable: <e>` and the provider still attaches, giving no allocations until it answers. Needs `station set` for distances. It belongs to the serving node: with `--node` it is a usage error (exit 1).
- `--plugin KIND=PATH`: register the program at absolute `PATH` as stage kind `KIND` (lowercase, not a built-in kind). It is probed at startup; a failed probe exits 1 naming the kind and the reason (a `hello` of a protocol other than 1 or 2 is refused). A plugin runs with a cleared environment: only `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM` and `PYTHONPATH` pass through, and each `ZETA_PLUGIN_ENV_<NAME>` variable reaches it as `<NAME>`; nothing else (no `ZETA_SECRET`) does.
- `--modes-dir PATH`: the operator's decoder mode rows, one `*.json` file each, read after the built-in ones (default `$XDG_CONFIG_HOME/zetasdr/modes`, else `~/.config/zetasdr/modes`; a missing directory is no error). A row with a built-in mode's `id` replaces it. A row that does not parse or validate never stops the node: it is listed `broken` by `modes list` and reported at startup as `zeta: mode <id> (<file>): <field>: <error>`, and a broken file that meant to replace a built-in row leaves that row in force.

Sources and plugins are not persisted: repeat the flags, with the same ids and kinds, on every invocation, `serve` included. A source id may be used once across all the source flags. `source list` shows each source's capability record: `remote` is true for a KiwiSDR, a `--source-node` and a `--source-vrt` source, `push` false for SoapySDR devices, and `fault` names a source that has failed (a full Kiwi, an unplugged device); a job on a faulted source fails with that reason.

There are two ways to run:

- One-shot, `zeta <group> <verb> ...`: opens the node, applies one operation, prints the JSON result and exits (0 on success, 2 with a JSON error). It never resumes jobs: a job started one-shot is persisted as `running` and runs once `zeta serve` starts. `job start --then-tail MAX_FRAMES,TIMEOUT_MS` keeps that one-shot node up after the start, tails the job's audio output when it has one (a demodulator), else its first event stream, then stops the job and prints `[job_result, frames_result, {"stopped": true}]`; add `--keep` to leave it running (`{"stopped": false}`). If the job is not running (e.g. failed) it prints `[job_result, job_record]`, the record's `state` carrying `status` and `reason`, and exits 2; `stream tail` on its own only sees streams of jobs running in the same process, and `job attach` needs its input job running in the same node, so attach against `zeta serve` (`--node`). A one-shot `iq read` or `classify` reads the ring that the command itself fills from the source, waiting up to 5 s for it to cover the span. One-shot commands, read-only ones included, restore the pins and the gain of the sources they attach (so even a one-shot `source list` tunes a pinned radio). One-shot commands, like `zeta serve`, start and probe every `--plugin` program at startup, so with plugins configured even a one-shot `source list` costs one process start per plugin.
- `zeta serve`: resumes persisted jobs and keeps them running until Ctrl-C.
- Against a running node, `zeta --node URL --secret SECRET <group> <verb> ...` (or the `ZETA_NODE` and `ZETA_SECRET` variables): the operation is sent to that node over HTTP and applied there, so jobs started this way keep running; `--state-dir`, the `--source-*` flags, `--plugin`, `--modes-dir`, `--knowledge-acma` and `--bursts-keep-*` belong to the serving node and are rejected (a usage error, exit 1); `--stt-socket`, `--stt-node` and `--soapy-in-process` belong to it too but are ignored, and so are `ZETA_KNOWLEDGE_ACMA`, `ZETA_BURSTS_KEEP_MB`, `ZETA_BURSTS_KEEP_DAYS` and `ZETA_RING_BUDGET_MB` from the environment. `zeta soapy find` always runs locally. `--then-tail` is not supported with `--node` yet (a usage error, exit 1): start the job, then `stream list` and `stream tail` against the node. Start that node with `zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) and a secret. `zeta-mcp --node URL` offers the same operations as MCP tools, and `zeta ask "REQUEST"` lets a local model call them.
- `zeta serve --listen PORT --llm-url URL --llm-model NAME` (the other `--llm-*` flags of `zeta ask` apply too) enables `POST /ask` with `{"request": "TEXT", "max_turns": 8, "allow": {"stop_jobs": false, "replace": false, "delete_captures": false}}` (`max_turns` 1-32, default 8; `allow` optional, all false by default: without them the agent's `job_stop` and non-empty `replace` calls and its `capture_delete` are refused as `not_allowed`): the node runs the agent against itself, so every operation it applies appears on the `ops` stream, and answers the transcript (`request`, `answer`, `calls`, `turns`) as `zeta ask` prints it, plus `allow` as the run had it and `turn_log`, one entry per model completion with its reasoning, content and call count. One run at a time (409 while one is in progress); 503 when serve has no `--llm-url`; a failed run is 502 with the error and the transcript so far; 504 after 300 s.
- `zeta serve --listen PORT --stt-socket PATH` (env `ZETA_STT_SOCKET`; default `$XDG_RUNTIME_DIR/voiced.sock` when that file exists) enables `POST /stt?lang=CODE` (`lang` defaults to `en`): the body is a 16 kHz mono `audio/wav` (at most 4 MiB), the node hands it to the speech daemon at that Unix socket and answers `{"text": "TRANSCRIPT"}`. The daemon is `local-voice` or any daemon speaking its protocol: one line per connection, `file:<absolute wav path>` answered `ok:<text>` or `error:<message>`, and `languages` answered `ok:en,de,...` (a daemon answering `error:` there is taken to know English only). Instead of a daemon, `--stt-node URL` (env `ZETA_STT_NODE`, secret in `ZETA_STT_NODE_SECRET`, never printed) forwards each request to that node's `POST /stt`; the two flags are exclusive, for every command (exit 1). A forwarded request carries `X-Zeta-Stt-Hop: 1`, and a node that would forward it again answers 508 (`stt forwarding loop`). `GET /stt/languages` answers `{"backend": NAME, "languages": [CODES]}` (503 without a backend). 415 when the body is not WAV, 413 when too large, 422 `{"error": "unsupported", "language", "supported"}` for a language the backend lacks, 502 when the daemon reports an error, 503 when serve has no backend or cannot reach it, 503 `{"error": "busy", "message", "wait_ms"}` when every one of the daemon's 4 slots stayed taken for 30 s (a node forwarding with `--stt-node` reports that as a `busy` drop, not an error), 504 when the daemon does not answer within 30 s of its turn.

While `zeta serve` runs it owns the state directory (it holds `serve.lock` there). Mutating one-shot commands (`job start`, `job attach`, `job retune`, `job stop`, `jobs stop`, `plan start`, `mix start`, `graph put`, `station set`, `station clear`, `capture delete`, `source ensure`, `source pin`, `source unpin`, `source gain`) are then refused with an `other` error naming the server's pid; read-only commands still work. Use `--node` to change a node that `zeta serve` owns.

Every operation the node applies is appended to `ops.log` in the state directory (rotated at 8 MiB) with its caller's transport (read-only operations are logged without their result); `zeta ops tail --lines N` prints the last N (local only). Every `transcript` event of a `transcribe` job is appended to `transcripts.log` in the state directory (rotated at 8 MiB) with the job, its input, the source and the channel's centre; `zeta transcripts tail --lines N [--job ID]` prints the last N (local only).

## Operations

### source_list

List the sources: id, capabilities, `current` band, `holders` (the rigid lease holders' job ids), `elastic` (the jobs following the capture, see `job_start`), `pin` (the band `source_pin` holds the source at, `null` for none), `fault`, the source's unrecoverable failure (`null` when healthy; a healed fault stays until recovery has seen the jobs end, normally up to one poll after the worker's hello, and a `job_start` in that hold is accepted, its job possibly failing and being relaunched under its id), `bank`, the channel bank shared by the capture's channels (`bin_hz`, `consumers`, `dropped`, the input blocks it dropped because it could not keep up; `null` for none), `gain`, the overall receive gain (`db`, `null` while automatic; `min_db`, `max_db`, `automatic`, `automatic_available`; `null` for a source without adjustable gain), and `stats`, the stream's counters since it opened (`measured_rate_hz`, the rate delivered over the last 10 s, `null` before any; `overflows`, `timeouts`, `restarts`, streams reopened because their samples ran ahead of the clock; `null` for a source that keeps none), and `worker`, the child process the source runs in (`pid`, `restarts`, `since_ns`; `null` for a source that runs in the node), and `ring`, the IQ history (`seconds`, `null` before the first block; a `classify` or `iq_read` span older than it fails `too_old`). Jobs on a faulted source fail with `source ID: FAULT` as their reason; a successful tune (`source_ensure`, `job_start`) clears it. A channelised job's `job_context` holds `channel.path`: `bank` (with `bin_hz`) or `direct` (with `reason`, why not the bank).

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

### source_pin

Pin the source at a band: a rigid lease held by no job (`source_list` shows it as `pin`, never among `holders`), granted like a rigid `job_start`: inside the capture other rigid jobs hold it is leased without a tune, elsewhere it tunes when only elastic jobs hold the source (they are relaunched on the new capture), else it is a `conflict`. Replaces any pin the source had (a conflict keeps the old one). Persisted in the state directory and restored before any job resumes. The result is the capture.

- source: string; an id from `source list`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"source_pin","source":"<id>","center_hz":144800000,"bandwidth_hz":12500,"replace":[]}`

CLI: `zeta source pin --source <id> --center-hz 144800000 --bandwidth-hz 12500`

Hold the source at a band without a job: a rigid holder that is not a job, so `replace` cannot name it and a `conflict` offers `unpin source <id> (source_unpin)`. A pin retunes the source to its band (the page's spectrum follows) unless a decoder holds the source inside that band, when it joins the current capture. Persisted; restored before anything else on a restart. Put `center_hz` between two channels of the grid you listen to so the receiver's DC spike falls in a guard band (the UHF CB plan's midpoint 476918750 Hz with 1000000 Hz holds all 80 channels).

### source_unpin

Release the source's pin (see `source_pin`).

- source: string; an id from `source list`

JSON: `{"op":"source_unpin","source":"<id>"}`

CLI: `zeta source unpin --source <id>`

Release the pin; the source idles if nothing else holds it.

### source_free

The sources that could take the band now: `free` lists, in id order, every source whose `tune_min_hz..=tune_max_hz` holds both edges of the band and whose `max_bandwidth_hz` its width, with no rigid holder, no pin and no fault (elastic jobs never make a source busy); `busy` lists every other source that covers the band with why: `holders`, `elastic`, `pin` and `fault` as `source_list` gives them. A source that cannot cover the band is in neither list. Takes no lease.

- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"source_free","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta source free --center-hz 144800000 --bandwidth-hz 12500`

Ask which radio could take a band before starting a job on it, then start the job on one of the `free` ids. A free source is a suggestion, not a reservation: nothing is leased, and the next `job_start` may still conflict. A source on another node (`--source-node`) can look free here while that node runs its own jobs on it; that surfaces as a `conflict` on `job_start`.

### source_set_gain

Set the source's overall receive gain live; the result is the gain after the change (`source_list` shows it as `gain`). Persisted in the state directory and re-applied when the node opens.

- source: string; an id from `source list`
- gain_db: number or null (dB)

JSON: `{"op":"source_set_gain","source":"<id>","gain_db":null}`

CLI: `zeta source gain --source <id> --db 40`

Set the source's overall receive gain live (`gain_db` in dB within `source list`'s `gain.min_db..=max_db`), or `null` for the driver's automatic gain where `gain.automatic_available`. Persisted: it is re-applied when the node restarts and wins over `gain=` in the device args. A source without adjustable gain (a recording, a KiwiSDR, another node) answers `source has no adjustable gain`. On the CLI, `--auto` passes `null`.

### job_start

- kind: string (JobKind: passthrough|spectrum|fm_demod|wfm_demod|am_demod|ssb_demod|aprs_decode|pocsag_decode|rds_decode|acars_decode|adsb_decode|measure|scan|identify|watch|transcribe|mix|capture|dfm_decode|m20_decode|rs41_decode; the built-in kinds and the embedded mode rows, a node's own `schema` adds its operator rows)
- source: string; an id from `source list`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)
- deviation_hz: integer or null
- sideband: string or null
- elastic: boolean
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"job_start","kind":"passthrough","source":"<id>","center_hz":144800000,"bandwidth_hz":12500,"deviation_hz":null,"sideband":null,"elastic":false,"replace":[]}`

CLI: `zeta job start --kind passthrough --source <id> --center-hz 144800000 --bandwidth-hz 12500`

`fm_demod` is narrowband FM voice; `wfm_demod` is wideband broadcast FM (mono, 50 us de-emphasis, 15 kHz audio; give it a band of about 200000 Hz). `am_demod` is AM voice (envelope detector, 5 kHz audio, slow AGC; give it a band of about 10000 Hz). `ssb_demod` is SSB voice (2800 Hz audio, slow AGC; `center_hz` is the suppressed-carrier (dial) frequency, not the middle of the voice band, and the band must cover both sides of it: give it about 6000 Hz). All four output 48 kHz mono audio: an audio frame's payload is `{"sample_rate_hz", "channels", "samples": N, "peak"}`, `channels` 1 here, the samples interleaved and `samples` the per-channel count (a frame without `channels` means 1). `fm_demod` shapes its audio as a voice receiver does (a 3.4 kHz low-pass and a 530 us de-emphasis after the discriminator: `audio_lowpass_hz` and `deemphasis_us` on its node, `null` for the flat audio a decoder wants), and every voice demodulator (`fm_demod`, `am_demod`, `ssb_demod`) takes `denoise: true` on its node to run RNNoise on its audio output (off by default; speech only; one 10 ms frame of delay the frames' spans account for; set with `graph put`, kept through a retune; a `transcribe` attached to such a job skips its own `denoise` stage). `fm_demod` and `am_demod` carry an automatic squelch: it opens 6 dB over the channel's tracked noise floor (never taken above -40 dBFS: with no fixed threshold `floor_db` reads at most -40) and closes after 300 ms under it. While closed the job publishes no audio (an attached transcriber idles; `--then-tail` tails the audio, so an idle channel's tail is empty) and the first block and each change publish a `squelch` event (`open`, `power_db`, `floor_db`) on the job's event stream. The demod node's params set it with `graph get` and `graph put`: `squelch_db` is an absolute threshold in dBFS, -120..=0 (`null`, the default, is the automatic one; a value outside the range fails the job with `squelch_db <v> out of range -120..=0`), and `squelch: false` turns it off. `aprs_decode`'s demodulator runs with the squelch off. `fm_demod` on a channel of a plan (`channel_plans`) carries an adjacent-channel guard: its squelch stays closed while a channel `guard_hz` away is more than `guard_db` (12) dB stronger, so a transmission on ch20 does not open ch59 or ch60. The `squelch` event carries `adjacent_db` and `guarded`; once guarded, the gate stays held until the neighbour falls 2 dB under `guard_db`. The job's `job_context` keeps the last `squelch` event under `squelch` (removed when a run has the squelch off), so a client that subscribes after the gate changed reads it there. `graph put` with `guard_hz: null` on the demod node turns it off. `pocsag_decode` decodes POCSAG pagers (512, 1200 and 2400 baud detected automatically) from a 12.5 kHz FM channel into one event per message (`address`, `function`, `kind`, `text`). `rds_decode` reads the RDS data of a broadcast FM station (give it the same 200000 Hz band as `wfm_demod`) into events carrying `pi`, `ps` (station name) and `rt` (radio text) as they complete; the first event takes a few seconds. `adsb_decode` decodes Mode S / ADS-B at 1090000000 Hz with `bandwidth_hz` 2400000 (anything under 2000000 is refused) by running dump1090 on the node's host; one event per decoded message (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`, `track_deg`), position and identity arriving in separate messages. When the host has no dump1090 the node's startup handler report names it (`zeta: handlers: dump1090: missing (apt install dump1090-fa; <url>) — modes: adsb_decode`), `modes_list` shows `adsb_decode` `unavailable` with that line as its `reason`, and `job_start` fails with `mode adsb_decode is unavailable: <reason>`. `acars_decode` decodes ACARS (VHF, AM, 2400 baud MSK) from a 10000 Hz channel on 131550000 Hz (worldwide primary), 131725000 / 131825000 (Europe), 130025000 / 130450000 / 131125000 (North America), 131450000 (Asia) into one event per CRC-clean block (`reg`, `flight`, `label`, `text`). `rs41_decode` decodes a Vaisala RS41 radiosonde anywhere in 400050000-406000000 Hz (`dfm_decode` and `m20_decode` a Graw DFM and a Meteomodem M20 alike) by running the rs1729 decoder on the node's host over NBFM audio: start it on the sonde's measured centre (its frequency varies by flight; never a raster) with `bandwidth_hz` 12500, one sonde per job, and each event carries `id`, `position` and the frame's GPS time as `t_ns`. `deviation_hz` is the FM peak deviation for `fm_demod` and `wfm_demod` (100-75000 Hz; `null` means the kind's default: 5000 Hz for `fm_demod`, right for standard voice FM, use 2500 for narrow 12.5 kHz channels; 75000 Hz for `wfm_demod`, right for broadcast FM). Every other kind takes `null`. `sideband` is `usb` or `lsb` for `ssb_demod` (`null` means `usb`; amateur convention is `lsb` below 10 MHz, `usb` above); every other kind takes `null`. On the CLI, omit `--deviation-hz` and `--sideband` for `null`. `scan` sweeps a band wider than one capture: the node steps the source across it in captures of at most 16 MHz, dwells 250 ms on each, and emits `spectrum` rows whose `center_hz` is that step's (frame `channel` `scan/<step index>`), wrapping until stopped. Alternate sweeps shift the step grid down by a quarter step (the odd sweep has at most one more step), so a step's centre and edges in one sweep fall a quarter of the way into a row of the next and every frequency is seen clean in one of two sweeps; `scan.alternate` in the job context gives `shift_hz` and `steps_shifted` (`null` for a one-step span). A scan on a local hardware receiver masks each row's centre notch (`notch_hz`, 50000 Hz) and its outer edges (`edge_mask`, 0.02 of the bins each side); every other source gets none. A row with masked bins carries `masked: [[lo, hi], ...]` (bin index ranges, `lo` inclusive, `hi` exclusive): those bins are no data, not low power; their `q` is still present. `scan.mask` reports the values in force (a step narrower than four notches gets no notch; a one-step scan does not alternate and gets no edge mask: nothing could fill it in), and `graph get`, edit the `fft` node's `notch_hz`/`edge_mask`, `graph put` changes them (keep `edge_mask` under 0.25, or the two grids' masks overlap). Of the scan's stored `fft` node only `notch_hz` and `edge_mask` are read: its `size`, `window` and `rows_per_s` are information only (the scan derives them per step), and a `graph put` whose graph has no `fft` node is re-expanded with the default mask on the next launch rather than refused. A scan time-shares the radio: while it runs, every other `job_start` and `source_ensure` on the source is a `conflict` naming it, decoders included, and `source_list` shows the step it is on as `current`. Stop the scan to decode. A scan's `job_context` holds `scan.steps` (how many captures one sweep takes), `scan.step_hz`, `scan.dwell_ms`, `scan.alternate` and `scan.mask`: report step counts from it, never from guesswork. `identify` is the thorough answer to "what is this?": started like a decoder (`job_start` with `kind: identify` on the box's centre and band), it holds a rigid lease on that band, measures it for 1000 ms (layer 1), scores every mode row against it and tries each ready row that fits, one at a time in score order, as an ordinary child job on the same band (`job_list` shows it, `source_list.holders` shows both while it runs) for the row's `identify.dwell_ms` (5000 ms for APRS, POCSAG and the sonde rows, 10000 for RDS and ACARS, 15000 for ADS-B). Each row it considers publishes one `candidate` event on its event stream: `mode`, `score`, `outcome` (`proved`, `no_frame`, `skipped_band` for a row wider than the band, `unavailable` for a row whose program is missing or whose job would not start), `evidence`, `summary` (`<mode>: <outcome>`) and `job` (the child's id, `null` when none ran). A proof ends the job `stopped` with `job_context.identify` = `{"verdict": {"mode", "label", "summary", "evidence", "job"}, "attempts": n}` and leaves that mode's job running under the id in `verdict.job`, already decoding; stop it with `job_stop` when done. A list exhausted with no proof ends it `stopped` with `verdict` `{"mode": null, "label": <layer-1 name>, "features": <the layer-1 record>}`: report that as unknown with those measured features. Stopping an `identify` job stops the child it is trying; a source that faults fails it with the fault as its reason. With `--then-tail`, an `identify` that ends with a verdict during the tail is a success: the candidates are printed and the proved job (`verdict.job`) is stopped, or left running with `--keep` (`{"stopped": false}`). `watch` keeps an eye on a band and decodes what appears in it: `job_start` with `kind: watch` on the band (400-406 MHz is `center_hz: 403025000`, `bandwidth_hz: 5950000`; put the centre off any channel of interest, the receiver's DC spike sits there). It holds a rigid lease on the band like `identify` and runs `fft → detect` over the capture: `detect` publishes a `signal` event (`state` `new` or `gone`, `center_hz`, `bandwidth_hz`, `snr_db`, `first_ns`, `last_ns`) for each run of bins that persists (at most 10 `signal` events a second, the rest delayed), and each new signal no other job's band overlaps gets an `identify` run on its measured centre, one at a time, best-scoring first, on the band the best-scoring fitting row expects (12500 Hz for the sondes). A proof leaves the decoder running: inside the watch's capture with `decode_on: capture` (the default; one radio does both), or with `decode_on: free` on a free source other than the watch's (`source_free`), the ids in `prefer` first, back in the capture when none takes it. `on_unknown: capture` (with `capture: {bursts, window_ms, pre_ms, post_ms, timeout_ms}`) captures an unknown signal's bursts once per run as a `capture` child, publishing `capturing` and `captured`; `job_context.watch.signals[].captures` lists them. A signal no row proves is identified again after 60000 ms while it persists, at most 3 times; a decoder whose signal is `gone` and that has sent no event for 120000 ms is stopped (a landed sonde). The watch's own events, `protocol: watch`, go on the same event stream: `state` `identifying` (`signal`, `job` the identify run), `decoding` (`mode`, `job`, `source`, `placed` `capture` or `free`; the `summary` says why it fell back to the capture), `unknown` (`features`, the layer-1 record: report it as unknown with them) and `lost`, each with `summary` and `t_ns`. `job_context.watch` holds `signals` (each with `tries`, `next_retry_ns`, `handled`), `decoding` (`mode`, `job`, `source`, `signal`, `since_ns`, `placed`) and `current` (the identify run in progress, else `null`). `detect` also honours a row's `masked` ranges (those bins take no part in a run). Tune it with `graph get`, edit the `detect` node's params, `graph put`: `threshold_db` (6, over the row's median bin), `energy_db` (30, the least sum over a run's bins of dB over that median), `persist_ms` (3000; a signal also needs 3 sighting rows before `new`), `width_hz` (`[4000, 30000]`), `dc_mask_hz` (25000), `edge_mask` (0.05), `spurs_hz` (`[]`), `gone_ms` (30000), and the policy `decode_on`, `prefer`, `sondehub`, `on_unknown` (`report`, the default, or `capture`) and `capture`. `sondehub` (`{callsign, lat, lon, alt_m, batch_ms, url}`, the last two optional: 5000 ms and SondeHub's telemetry endpoint) appends a `sink_sondehub` node to every decoder the watch keeps, fed by its events; without it nothing is uploaded. The `sondehub` block is checked like the detect params: a bad one is refused at `job_start` and `graph put`, naming `sondehub.<field>`, before anything starts or restarts. If the sink cannot be added to a kept decoder it runs without it (the `decoding` event's `summary` says so and its `decoding` entry carries `sink_error`); one that cannot be relaunched at all is `lost`. `sink_sondehub` can be put on any sonde job with `graph put` (an `event` edge from the decoder's event node): it sends the `rs41`, `dfm` and `m20` events that carry a `position` (with a numeric altitude), a `datetime` and a `frame` to SondeHub in a `PUT` every `batch_ms`, and `job_context.sondehub` holds `sent`, `failed` and `last_error` (a failed batch is counted and dropped, not retried). The identify runs and their trial jobs are the watch's (`owner` is its id in `job_list`) and leave no history; the decoders it keeps are ordinary jobs. `job_stop` on the watch stops its identify run and every decoder it keeps, and so does a watch that ends `failed`; a `graph put` on it or a node restart re-adopts them instead, stopping any whose signal does not reappear within `gone_ms`. A `job_start` or `graph put` with an `edge_mask` outside 0..0.5, a `width_hz` not ordered `[min, max]`, a bad `sondehub` or `capture` (naming `capture.<field>`) or a `prefer` naming a source not in `source_list` (`prefer[<i>]: unknown source <id>`) is refused naming the field, before anything starts or restarts. `transcribe` is not started on the radio: `job_start` refuses it; use `job_attach`. `capture` writes a signal's bursts to disk as SigMF windows (`burst_capture`: `bursts`, `window_ms`, `pre_ms`, `post_ms`, `timeout_ms`, `whole`; a signal already on when it starts, which makes no burst edge, is cut into back-to-back windows after 500 ms, `job_context.capture.whole_auto` true) and ends `stopped` with `job_context.capture`; one started by hand classifies its first window at its end into `job_context.capture.classified` (`labels`, `tried`, `allocations`). `kind` also takes the id of any ready mode row (`modes_list`), an operator's own rows included; an id that is neither a built-in kind nor a row's fails with an `unknown` error (`field: "kind"`, `known` the built-in kinds and every row id), a row that is not ready with `mode <id> is <state>: <reason>`. `elastic: true` (spectrum only) makes the job follow the source's capture instead of holding it: it never conflicts, is relaunched on every retune and is listed under `elastic` in `source_list`; a rigid request inside the capture elastic jobs follow is granted on it without a retune, one outside it retunes the source; the web page's own spectrum job is elastic. `replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder. An identical rigid start (same source, kind, band, `deviation_hz` and `sideband`) on a running job is refused as `duplicate` naming that job; stop it or use it.

### job_attach

Attach a job of an attachable `kind` (`transcribe`) to the running job `job`: it reads that job's audio output stream (the last one in its graph order when it has several; audio passed between two of its stages does not count) instead of the radio, holds no lease, copies the job's `source` and `tuning`, and records it as its `input`. It follows its input through a `job_retune` or a `graph_put` restart: it is relaunched under its own id on the input's new stream once the input runs again (`failed` with the reason when it cannot be). It ends `stopped` when the input job stops or fails for its own reason. On a node restart it resumes after its input, or is `failed` with `input job <id> not running` when the input does not run.

- kind: string (JobKind: passthrough|spectrum|fm_demod|wfm_demod|am_demod|ssb_demod|aprs_decode|pocsag_decode|rds_decode|acars_decode|adsb_decode|measure|scan|identify|watch|transcribe|mix|capture|dfm_decode|m20_decode|rs41_decode; the built-in kinds and the embedded mode rows, a node's own `schema` adds its operator rows)
- job: string; an id from `job list`
- language: string or null

JSON: `{"op":"job_attach","kind":"transcribe","job":"<id>","language":null}`

CLI: `zeta job attach --kind transcribe --job <id>`

Attach a processing chain to a running job's output instead of the radio: `transcribe` reads the job's audio (any demodulator, later any digital voice decoder), cleans it (denoise, voice gate) and emits one `transcript` event per utterance (`text`, `language`, `start_ns`, `end_ns`) plus `vad` events as speech starts and stops; it holds no lease, follows its input through a `job_retune` or a `graph_put` (relaunched under its own id) and ends when the input job stops. `language` is a BCP-47 code (`null` means `en`; the backend's languages are in `GET /stt/languages`). To transcribe a station: `job_start` a `wfm_demod` on it, then `job_attach` with `kind: transcribe` and that job's id. The attached job fails with a reason naming the speech backend when the node has none or it cannot serve the language. Every other kind (`identify`, `watch`, the demodulators and decoders) runs on the radio and is refused here as `<kind> is not attachable`.

### job_retune

Retune a running job on the radio to a new band under the same id: under the source's gate its lease is moved to `center_hz`/`bandwidth_hz` (inside the current capture it is granted without a tune, whether or not another rigid job's band overlaps it; outside it the source is tuned when no other rigid job or pin holds it, else it is a `conflict` naming the holders and the job is left as it was), its graph is expanded again for the new band at the capture's rate and the job relaunched, `tuning` updated. Applies to a `running` job the node expanded and can re-expand: the four demodulators, the decoders, `measure`, `passthrough` and the mode rows. Refused naming the kind for `scan`, `watch`, `identify`, `spectrum` (elastic or rigid), `mix` (it has no band), a `graph_start` job (it runs on no radio) and `transcribe` (attached: retune its input), and for a job whose graph an operator `graph_put` replaced (it launches as stored: put a new graph or start a new job). A job that is not running is `job <id> is <state>`; an unknown id is `unknown`. The result is the job's id, as `job_start` answers.

- job: string; an id from `job list`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"job_retune","job":"<id>","center_hz":144800000,"bandwidth_hz":12500,"replace":[]}`

CLI: `zeta job retune --job <id> --center-hz 144800000 --bandwidth-hz 12500`

`replace`: as on `job_start`, the holders that may be stopped to make room, stopped under the gate right before the request; a `conflict` offers `retry with replace: [<ids>]` and the page shows the same retry buttons. A demodulator keeps its `squelch`, `squelch_db`, `deviation_hz` and `sideband` through a retune; its adjacent-channel guard (`guard_hz`, `guard_db`, `channel_hz`, the channel node's `wide_hz`) is derived again from the channel plan at the new band, never carried, so a width preset changes what the filter passes and a move to a neighbouring channel takes that channel's geometry; a guard turned off with `graph_put` (`guard_hz: null`) comes back on a retune onto a plan channel. Every attached job (`job_attach`) reading the retuned job is relaunched under its own id once the input runs again (one that cannot relaunch is `failed` with the reason; the retune still succeeds). Centre the band on the signal's measured centre (`measure`, `classify`), never on a raster: see Fitting a demod to a signal under Field craft. A one-shot `zeta job retune` cannot reach a job `zeta serve` runs (`job <id> is not running in this node`): use `--node`. On the CLI, `--replace ID[,ID]`.

### mix_start

Start a mix: a job that sums the audio of every other job with an audio output on `sources` and plays it as one stream, so whatever is talking is heard. It takes no lease, tunes nothing and blocks no job (its record's source is `node`, its band 0); jobs started or stopped later join and leave by themselves within a second. It outputs `audio` and `event` streams; each change of who is talking publishes `{"protocol": "mix", "active": [{"job", "source", "center_hz"}], "t_ns", "summary"}`. Attached jobs, a `watch`'s or `identify`'s own jobs and other mixes are not mixed. With a mix of the same set of sources already running the result is its id; `null` matches only `null`, so a list naming every source is another mix. It runs until stopped, and a node restart resumes it after the jobs it mixes.

- sources: array or null; ids from `source list` (strings), `null` for every source

JSON: `{"op":"mix_start","sources":null}`

CLI: `zeta mix start`

Starts one `mix` job that plays the audio of every job on `sources` (`null`: every source) as one stream, so whatever is talking is heard. It holds no lease, tunes nothing and blocks no job; jobs started or stopped later join and leave by themselves. Its `event` stream publishes `active` (who is talking: `job`, `source`, `center_hz`) when it changes, with a hangover after a channel falls silent. It is idempotent on the same set of sources: a mix of that set already running answers its id (`null` matches only `null`). To mute a channel, `graph get` the mix, edit `muted` on the `mix` node (a list of `{source, center_hz}`), `graph put`, which restarts it under its id.

### station_set

Set the node's station: its position, region and antennas, persisted in `station.json`. A node-level record, set once; the ACMA provider's distance and reach, `sink_sondehub`'s defaults and the page's home read it. `antennas` is `null` for the defaults (a 0-30 MHz dipole and a 30 MHz-6 GHz discone, both 5 m up). Validated before anything is written: `lat` -90..=90, `lon` -180..=180, each antenna `min_hz` < `max_hz`, `azimuth_deg` 0..360, `elevation_deg` 0..=90, ids unique. The result is the station as stored.

- lat: number
- lon: number
- alt_m: number
- region: string; free-text region code such as `AU`
- antennas: array or null

JSON: `{"op":"station_set","lat":null,"lon":null,"alt_m":null,"region":"AU","antennas":null}`

CLI: `zeta station set --lat -27.38 --lon 153.12 --alt-m 4 [--region AU] [--antenna KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]]...`

A node-level record, set once: the position, region and antennas the node listens from. The ACMA provider's distance and reach, `sink_sondehub`'s defaults and the page's home read it. `antennas: null` takes the defaults (a 0-30 MHz dipole and a 30 MHz-6 GHz discone, both 5 m up); an antenna is `{id, kind, min_hz, max_hz, height_m, azimuth_deg, elevation_deg}` with `kind` one of `discone`, `dipole`, `skyloop`, `vertical`, `yagi`, `dish`, `other` and the two angles `null` for an omnidirectional one. A bad value is refused naming the field (`lat`, `antennas[<i>].<field>`), with nothing written. On the CLI, `--region` defaults to `AU` and each `--antenna KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]` adds one to the defaults (its id is the kind, numbered when taken); a declared antenna wins over a default on its own band. `station_get` answers the station or `null`; `station_clear` forgets it.

### station_get

The station, or `null` when none is set.

JSON: `{"op":"station_get"}`

CLI: `zeta station get`

### station_clear

Forget the station (`station.json` is removed).

JSON: `{"op":"station_clear"}`

CLI: `zeta station clear`

### capture_list

The captures the node's burst store holds, newest first, each a record (`id`, `source`, `center_hz`, `bandwidth_hz`, `sample_rate_hz`, `start_ns`, `end_ns`, `bytes`, `features`, `signal`, `job`, `watch`, `allocations`, `pinned`). `signal` narrows the list to the captures whose band overlaps that band; `null` lists all.

- signal: object or null

JSON: `{"op":"capture_list","signal":null}`

CLI: `zeta capture list [--center-hz C --bandwidth-hz B]`

The captures the node's burst store holds (written by `capture` jobs), newest first: each record has `id`, `source`, `center_hz`, `bandwidth_hz`, `sample_rate_hz`, `start_ns`, `end_ns`, `bytes`, `features` (the window's layer-1 record), `signal` (as a watch detected it, else `null`), `job`, `watch`, `allocations`, `pinned` and the two paths on the node's host. `signal` narrows the list to the captures whose band overlaps that band (`{center_hz, bandwidth_hz}`); `null` lists every capture. On the CLI, give `--center-hz` and `--bandwidth-hz` together to narrow. Through `ask` (the agent transport) only the newest 20 are returned, each without `allocations` and the two paths (`capture_show` has them), with `count`, how many the store holds.

### capture_show

One capture's record and its SigMF meta (`meta`, the `.sigmf-meta` as written). The record's `meta_path` and `data_path` are the node's own files; a remote caller fetches them at `GET /captures/<id>.sigmf-meta` and `.sigmf-data`.

- capture: string

JSON: `{"op":"capture_show","capture":"<id>"}`

CLI: `zeta capture show --capture <id>`

One capture's record and its SigMF `meta` as written. `meta_path` and `data_path` are files on the node's host, for a local caller; a remote caller fetches the same files from the node's HTTP port, with the bearer, at `GET /captures/<id>.sigmf-meta` (`application/json`) and `GET /captures/<id>.sigmf-data` (`application/octet-stream`, `cf32_le`); another name or an unknown id is 404. An unknown `capture` is an `unknown` error (`field: "capture"`, `known` the store's ids): take ids from `capture_list`, never invent them.

### capture_classify

Run the classifiers over a capture's whole window (the stored allocations included) as `classify` does over the ring: `{labels, tried, allocations}`.

- capture: string

JSON: `{"op":"capture_classify","capture":"<id>"}`

CLI: `zeta capture classify --capture <id>`

Run the classifiers over the capture's whole window as `classify` does over the ring (the record's stored `allocations` take part): `{labels, tried, allocations}`. A `no_frame` does not prove the protocol absent. A window shorter than a row's `identify.min_span_ms` is `skipped_span`.

### capture_delete

Delete a capture and its files. A pinned capture is refused. Deleting is the operator's decision: through `ask` it needs `allow.delete_captures`.

- capture: string

JSON: `{"op":"capture_delete","capture":"<id>"}`

CLI: `zeta capture delete --capture <id>`

Delete a capture and its files. A pinned capture is refused (`capture <id> is pinned`). Deleting is the operator's decision: through `ask` the run needs `allow.delete_captures` (`zeta ask --allow-delete`), else the call is refused as `not_allowed`: report that, do not retry. Never delete a capture the request did not name.

### plan_start

Start `kind` on every channel of the channel plan `plan` (`channel_plans`), or on the channels numbered in `channels`, each at its channel's `center_hz` with the plan's `bandwidth_hz`, and attach `attach` to each started job. Under the source's gate, in this order: everything is checked first (an unknown `plan` or channel is `unknown`, a channel listed twice, a kind that does not run per channel — `spectrum`, `scan`, `watch`, `identify`, `mix`, an attachable kind — and a non-attachable `attach` are `other`; nothing has happened yet); a channel where an identical rigid job already runs is `existing` and skipped; with `attach`, its stage must be ready, else the op is refused with the stage's reason; the `replace` jobs are stopped; then one lease decision for the band bounding the channels to start (lowest channel edge to highest), checked as a rigid `job_start` of that band would be: inside the capture no tune, outside it a tune when only elastic jobs hold the source, else a `conflict` naming the holders and nothing starts; a band wider than the source is `out_of_range` on `bandwidth_hz`. Each channel is then started in channel-number order inside that capture: one that fails is listed under `refused` with its error and the rest go on. With `attach`, each started job (and each `existing` one without one) gets one attached; a failure is that channel's `attach_error`, the job kept. The result: `capture`, `started` (`channel`, `center_hz`, `job`, `attached`, `attach_error`), `existing` (`channel`, `center_hz`, `job`, `attached`), `refused` (`channel`, `center_hz`, `error`) and `warnings` (strings to repeat to the operator; empty when there is nothing to say). The jobs are ordinary jobs; running the same request twice starts only what is missing.

- plan: string; an id from `channel_plans`
- channels: array or null; channel numbers of the plan (integers), `null` for all
- kind: string (JobKind: passthrough|spectrum|fm_demod|wfm_demod|am_demod|ssb_demod|aprs_decode|pocsag_decode|rds_decode|acars_decode|adsb_decode|measure|scan|identify|watch|transcribe|mix|capture|dfm_decode|m20_decode|rs41_decode; the built-in kinds and the embedded mode rows, a node's own `schema` adds its operator rows)
- source: string; an id from `source list`
- deviation_hz: integer or null
- sideband: string or null
- attach: string or null
- language: string or null
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"plan_start","plan":"uhf_cb_au","channels":null,"kind":"passthrough","source":"<id>","deviation_hz":null,"sideband":null,"attach":"transcribe","language":null,"replace":[]}`

CLI: `zeta plan start --plan uhf_cb_au --kind fm_demod --source <id> [--channels 1,2,5-8] [--attach transcribe] [--language de] [--replace ID,ID]`

Starts `kind` on the channels of a plan (`channels`, `null` for all of them) at each channel's `center_hz` with the plan's `bandwidth_hz`, under one lease decision for the band that bounds them: inside the current capture nothing retunes, otherwise the source is tuned when only elastic jobs hold it, and a scan or a rigid holder elsewhere is a `conflict` naming the holders with nothing started. Check `source_list` first. `kind` must run on the radio (not `transcribe`, `spectrum`, `scan`, `watch` or `identify`). `deviation_hz` and `sideband` are passed to every start as on `job_start` (`null` is the kind's default; give 2500 for `fm_demod` on 12.5 kHz channels). `attach` is `null` or `transcribe`, attached to every channel started or already running, and needs a ready stage (a speech backend for `transcribe`), else the op is refused with the stage's reason: start without `attach`. `language` must be `null` without `attach`. `replace`: as on `job_start`. The result lists `started` (channel, `center_hz`, `job`, `attached`, `attach_error`), `existing` (channels an identical running job already holds, skipped), `refused` (channels whose own start failed, with the error; the others went on) and `warnings`: repeat the `warnings` to the operator. Run it again to complete a partial set: running channels are `existing`, so nothing doubles.

### job_list

JSON: `{"op":"job_list"}`

CLI: `zeta job list`

Stopped and failed jobs are kept as history, newest 20 only (older ones are forgotten with their files); a running job is never dropped. Through `ask` (the agent transport) each record is returned without its `graph`; `graph_get` fetches one.

### job_stop

- job: string; an id from `job list`

JSON: `{"op":"job_stop","job":"<id>"}`

CLI: `zeta job stop --job <id>`

Stopping a job is the operator's decision: never `job_stop` a job the request did not name, except one you started in this run (`job_start`, `plan_start`, `job_attach`, `graph_start`, `bits_start`, or the `verdict.job` of your own `identify` once you have read it): stop your own `measure` and `identify` leftovers when done. Through `ask` any other stop needs the run to allow stops, else it is refused as `not_allowed`: report that, do not retry.

### jobs_stop

Stop every running job that matches all the filters given, in one operation: `source`, `kind` and `plan` (`null` for any; all three `null` is every job). A job's filter on `plan` is its centre lying on one of that plan's channels; a job on no radio (`graph_start`, `bits_start`, a mix) never matches a `plan`. Elastic jobs (the page's spectrum) and jobs another job owns (a `watch`'s or an `identify`'s trial jobs) are never named: they follow the capture or their owner. Jobs on a radio are stopped first, then those attached to them; an attached job that ended with its input counts as stopped. The result lists `stopped` (ids) and `failed` (`job`, `error`; the others go on). No match is an empty `stopped`, not an error.

- source: string or null; an id from `source list`
- kind: string or null
- plan: string or null; an id from `channel_plans`

JSON: `{"op":"jobs_stop","source":null,"kind":null,"plan":null}`

CLI: `zeta jobs stop [--source ID] [--kind KIND] [--plan ID]`

One call however many jobs match; the page's elastic spectrum keeps running. Stopping jobs is the operator's decision: use it only when the request asks for all of them, or names the source, kind or plan. Through `ask` the run must allow stops, else the call is refused as `not_allowed`: report that, do not retry. Report how many stopped and any `failed`. A mix stops only with no filter or `kind: mix`.

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

List the streams: id, kind and provenance. One is always the node's own `ops` event stream (provenance source `node` and no `job`; a mix's streams have source `node` too, with their `job`): one event per applied operation with its outcome and caller (`transport`: `local`, `http`, `ui` or `agent`), logged after the operation completes.

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

Fetch the current graph with `graph get`, edit it, and put it back; stage kinds and their params are listed under Stage kinds below and by `schema`. A put that changes only a demodulator's `squelch`/`squelch_db`, its guard's `guard_hz`/`guard_db`/`channel_hz`, the channel node's `wide_hz` (or `deviation_hz`/`sideband`) keeps the job re-expandable: a later capture change (another rate after a restart) re-expands it with those values carried. Any other change launches the graph as stored from then on.

### graph_start

Start a job from a graph whose first node has no inputs (`capture_in`, `stream_in`, `mix`): the record's kind is `graph`, its source `node`, its band the first `capture_in`'s first capture's (0/0 without one, so the page can place it), and it takes no lease and tunes nothing. A first node that takes input, and a node with an unfed input (no edge into it), are refused (`other`, naming the node); a `capture_in` whose capture is not in the store is `unknown` naming `captures[<i>]`. It launches the graph as stored (as after a `graph_put`); it is persisted and resumed like any job, after the jobs on a radio, and a `capture_in` job whose capture was expired since fails `capture <id> not found`. It has no band to retune. The result is the job's id, as `job_start` answers.

- graph: object (Graph: id, nodes[Node: id, kind, params], edges[Edge: from, to, kind])

JSON: `{"op":"graph_start","graph":{"id":"<id>","nodes":[{"id":"<node-id>","kind":"passthrough","params":{"kind":"iq"}}],"edges":[]}}`

CLI: `zeta graph start --file graph.json`

### bits_start

Work a capture's bits: takes every capture of the named capture's `set`, in time order, and starts `capture_in → resample → <demod> → clock_recover → slicer → bit_view` from the named capture's feature record, each `null` taken from the features (`modulation`, one of `fsk`, `psk` and `ook`, from the layer-1 label: `fsk` and `fm` give `fsk`, `psk` and `ook` themselves; `symbol_rate_baud` and `order` from the record, `order` 2 when it has none; fsk takes 2 or 4, psk 2, 4 or 8, ook 2). The capture is resampled to about 8 samples per symbol for fsk and ook, and for psk to the smallest multiple of 4 times the baud that also covers a carrier offset of 2000 Hz (the coarse estimate is unambiguous only within the rate over twice the order), moved to a rate the resampler serves exactly; 4-fsk takes the `early_late` clock; fsk's demodulator is given the record's measured `tone_spacing_hz` (it tracks the tones itself when the record has none); `capture_in` loops, paced, with a 500 ms pause between passes, and the job runs until stopped. Refused with `other`: a field neither the request nor the features give (`no symbol rate measured; give symbol_rate_baud`, `no modulation measured (label <x>); give modulation`), a rate the capture cannot hold (`symbol_rate_baud <r> needs <n> Hz; the capture holds <rate> Hz`, 4 samples per symbol at least; a rate of 0 is `symbol_rate_baud: at least 1`), and a set whose captures differ in rate or centre (`capture <id>: <rate> Hz differs from <first>'s <rate> Hz`); an unknown `capture` is `unknown`. The record's kind is `bits`, its source `node`, its band the capture's, `job_context.bits` the plan (`captures`, `modulation`, `symbol_rate_baud`, `order`, `tone_spacing_hz`, `rate_hz`); the slicer's `bits` events are on `job_context.slicer.stream` and `bit_view`'s latest hypotheses in `job_context.bit_view`. Change a running chain with `graph_get`, edit, `graph_put`. The result is the job's id, as `job_start` answers; one-shot `--then-tail` tails the `bits` events. The agent's own `bits_start` jobs count as jobs it started.

- capture: string
- modulation: string or null
- symbol_rate_baud: integer or null
- order: integer or null

JSON: `{"op":"bits_start","capture":"<id>","modulation":null,"symbol_rate_baud":null,"order":null}`

CLI: `zeta bits start --capture <id> [--modulation fsk|psk|ook] [--symbol-rate-baud N] [--order N] [--then-tail MAX_FRAMES,TIMEOUT_MS] [--keep]`

### knowledge_lookup

Band-plan allocations in `region` and known signatures overlapping the band, from every registered knowledge provider.

- region: string; free-text region code such as `AU`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"knowledge_lookup","region":"AU","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta knowledge lookup --region AU --center-hz 144800000 --bandwidth-hz 12500`

Two providers answer: the built-in seed signatures, and `catalogue`, the 594-signal iNTERCEPT catalogue (Apache-2.0; one signature per frequency range of each signal overlapping the band, `bandwidth_hz` its widest bandwidth, `modulation` the first it lists). A signature's `decoder` is the id of the mode (`modes_list`) that proves it, the `kind` to start on the band to decode it; `null` means no mode row proves it yet. `classify` runs only the modes with a quick attempt (APRS, POCSAG, ACARS, RDS) as decode-to-identify. The built-in two have no allocations. With `--knowledge-acma` a third provider, `acma`, answers: the register's plan services and assignments overlapping the band, each with licensee, site, distance and bearing from the station, power, role, the decoded emission designator and `reach` (whether a signal from that site can plausibly arrive here). It needs the station (`station set`): without one it gives no distances. An unreachable service gives no allocations, so an empty `allocations` list means no provider data, not nothing allocated.

### channel_plans

The channel plans the node knows, each with its channels expanded and sorted by frequency.

JSON: `{"op":"channel_plans"}`

CLI: `zeta plans [--plan ID]`

`channel_plans` lists the channel plans the node knows (`uhf_cb_au`: Australian UHF CB, 80 channels, two rasters 12.5 kHz apart). Name a channel by its number to the user ("ch20") and start jobs at its `center_hz` with the plan's `bandwidth_hz`.

### modes_list

The decoder modes the node knows, one row each (`modes/*.json` built in, then the operator's `--modes-dir`), sorted by id, narrowed to `family` and `state` when given (`null` for any).

- family: string or null
- state: string or null

JSON: `{"op":"modes_list","family":null,"state":null}`

CLI: `zeta modes list [--family F] [--state S]`

The decoder modes the node knows, one row each: `id` (the `kind` a decoder job starts with), `name`, `family`, `band` (`bandwidth_hz` the decoder expects, well-known `centers_hz`, `min_hz` the narrowest band it accepts or `null`, `range_hz` the span `[lo, hi]` it is found anywhere in or `null`), `modulation` (the layer-1 labels it presents as), `decoder_kind` (`graph`, run in-process, or `program`, an external decoder), `file` (`modes/<id>.json` for a built-in row, the path for an operator row), `program`, `checked` (UNIX seconds at which `zeta modes check` last passed the row on this host; `null` if never or for a graph row), `subject` (the event field naming what events are about, its table columns and how the page folds it; `null` for none), and `state` ready|broken|unavailable with `reason` (`null` when ready). A broken row's `file` is the file to fix and its `reason` names the field (`<field>: <error>`); an unavailable one names the program it lacks. `family` (and the `family` filter) is one of `tv`, `broadcast`, `aviation`, `maritime`, `satellite`, `two_way`, `cellular_rail`, `pagers`, `iot`, `weather_science`, `amateur_digital`, `amateur_voice`, `hf_utility`, `general`. On the CLI, `--family` and `--state` narrow the list. `zeta modes check [ID] [--strict]` (local only, not with `--node`) runs each row's test on a node over a synthesised capture and prints `<id>: ok`, `<id>: fail: <reason>` or `<id>: skipped: <reason>` (no test, or the program missing: `--strict` makes that a fail); it exits 0 when no row failed.

### modes_show

One mode's row as loaded, with its `state` and `reason`.

- id: string; an id from `modes list`

JSON: `{"op":"modes_show","id":"<id>"}`

CLI: `zeta modes show <id>`

One mode's full row as loaded (`row`: band, decoder graph or program block (an audio program row adds its `demod` and `input.container`), event schema and `summary` template (`{field:.N}` and `{field:+.N}` format numbers), `identify` rule, catalogue ids, `position` (the event fields of a fix, added to each event as `position: {lat, lon, alt_m}`; `alt_unit` `ft` converts a feet field), `subject` (`key`, `name`, `waterfall` `summary` or `fold`, `columns` of `field`, `label` and `unit`, `heading`, `label`) and `time` (the event field whose ISO 8601 UTC time becomes `t_ns`), each `null` for none) with its `state`, `reason`, `file`, `program` and `checked`. An unknown id is an `unknown` error (`field: "id"`) listing the known ids in `known`; take ids from `modes list`, never invent them.

### classify

Run every registered classifier over the selection's band and span of the source ring (no upstream features).

- selection: object (Selection: source, span{start_ns,end_ns: integer (ns since epoch)}, center_hz, bandwidth_hz)

JSON: `{"op":"classify","selection":{"source":"<id>","span":{"start_ns":1760000000000000000,"end_ns":1760000001000000000},"center_hz":144800000,"bandwidth_hz":12500}}`

CLI: `zeta classify --source <id> --start-ns 1760000000000000000 --end-ns 1760000001000000000 --center-hz 144800000 --bandwidth-hz 12500`

Layer 1 labels the measured band (`wfm`, `fm`, `am`, `ssb`, `fsk`, `carrier`, `noise` or `unknown`; an `ssb` label's evidence gives the dial frequency). Layer 2 first sweeps the decoder modes (`modes_list`) as decode-to-identify: each ready or unavailable row is scored against the layer-1 measurement (0.4 for the measured centre within `band.bandwidth_hz` of one of its `band.centers_hz` or inside its `band.range_hz`, or always when it has neither; 0.3 for a measured bandwidth within 0.5-1.5 times its `band.bandwidth_hz`; 0.3 for the layer-1 label in its `modulation`; plus its best catalogue score, below) and the best 5 are considered in score order (ties by `identify.priority`, highest first, then `family`, then `id`). The result is `{labels, tried}`: `tried` lists those 5 as `{mode, score, outcome}`, `outcome` one of `proved` (its quick decode passed its CRC/BCH check), `no_frame` (tried, nothing decoded), `skipped_span` (the span is shorter than the row's `identify.min_span_ms`, 1000 ms for the built-in rows), `skipped_quick` (the row has no quick attempt: a program row such as `adsb_decode` is tried only by identify, never by `classify`; use `adsb_decode`), `skipped_band` (the selection is narrower than the row's `band.min_hz`, else its `band.bandwidth_hz`: `rds_decode` needs 150000 Hz, `adsb_decode` 2000000; on a whole block the measured occupied band counts, read as 150000 Hz once it reaches 100000) or `unavailable` (its program is missing). A proof adds a layer-2 label named by the row's `label` (`APRS`, `POCSAG`, `ACARS`, `RDS`) at confidence 1.0, evidence `decode <mode>: ...`; a protocol is labelled only then. A `no_frame` does not prove the protocol absent: the span may have held no frame. Layer 2 also adds up to three signal names from the catalogue (evidence `catalogue <id>: ...`), scored 0.4 for the measured centre lying in one of the signal's frequency ranges (required), 0.3 for a matching bandwidth (required for signals wider than 50 kHz) and 0.3 for a matching layer-1 modulation. Layer 3 is an empty slot. A span older than the source's ring fails `ring` with `detail.cause` `too_old` and `detail.earliest_ns`, the ring's first retained sample (`source_list` gives its `ring.seconds`): re-issue on a span at or after it, never guess.

### schema

JSON: `{"op":"schema"}`

CLI: `zeta schema`

## Stage kinds

A graph node's `kind` names one of these; its `params` must match the listed fields (an optional one may be left out for its default).

### acars_decode

- inputs: iq
- outputs: event

### adsb_decode

- inputs: iq
- outputs: event

### am_demod

- inputs: iq
- outputs: audio, event
- params.audio_rate_hz: integer
- params.denoise: boolean (optional, default false)
- params.squelch: boolean (optional, default true)
- params.squelch_db: integer or null (optional, default null)

### aprs_decode

- inputs: audio
- outputs: event

### bit_view

- inputs: event
- outputs: event
- params.window_bits: integer (optional, default 65536)

### burst_capture

- inputs: iq
- outputs: event
- params.allocations: any (optional, default null)
- params.bandwidth_hz: integer or null (optional, default null)
- params.bursts: integer (optional, default 32)
- params.post_ms: integer (optional, default 20)
- params.pre_ms: integer (optional, default 20)
- params.signal: any (optional, default null)
- params.timeout_ms: integer (optional, default 120000)
- params.watch: any (optional, default null)
- params.whole: boolean (optional, default false)
- params.window_ms: integer (optional, default 2000)

### capture_in

- inputs: 
- outputs: iq, event
- params.captures: array
- params.loop: boolean (optional, default false)
- params.loop_pause_ms: integer (optional, default 500)
- params.realtime: boolean (optional, default true)

### channel

- inputs: iq
- outputs: iq
- params.center_hz: integer
- params.bandwidth_hz: integer
- params.out_rate_hz: integer
- params.wide_hz: integer or null (optional, default null)

### clock_recover

- inputs: audio
- outputs: audio, event
- params.symbol_rate_baud: integer
- params.algorithm: any (optional, default null)
- params.loop_bw: number (optional, default 0.01)
- params.max_ppm: integer (optional, default 2000)

### correlate

- inputs: event
- outputs: event
- params.pattern: string
- params.bits: integer
- params.max_errors: integer (optional, default 0)

### denoise

- inputs: audio
- outputs: audio

### detect

- inputs: event
- outputs: event
- params.capture: any (optional, default null)
- params.dc_mask_hz: integer (optional, default 25000)
- params.decode_on: string (DecodeOn: capture|free) (optional, default "capture")
- params.edge_mask: number (optional, default 0.05)
- params.energy_db: number (optional, default 30.0)
- params.gone_ms: integer (optional, default 30000)
- params.on_unknown: string (OnUnknown: report|capture) (optional, default "report")
- params.persist_ms: integer (optional, default 3000)
- params.prefer: array (optional, default [])
- params.sondehub: any (optional, default null)
- params.spurs_hz: array (optional, default [])
- params.threshold_db: number (optional, default 6.0)
- params.width_hz: array (optional, default [4000,30000])

### external_decode

- inputs: iq, audio
- outputs: event
- params.mode: string

### fft

- inputs: iq
- outputs: event
- params.size: integer
- params.window: string (Window: hann|rect)
- params.edge_mask: number (optional, default 0.0)
- params.notch_hz: integer (optional, default 0)
- params.rows_per_s: integer (optional, default 20)

### fsk_demod

- inputs: iq
- outputs: audio
- params.symbol_rate_baud: integer
- params.order: integer (optional, default 2)
- params.tone_spacing_hz: integer or null (optional, default null)

### measure

- inputs: iq
- outputs: event
- params.bandwidth_hz: integer or null

### mix

- inputs: 
- outputs: audio, event
- params.sources: array or null
- params.muted: array (optional, default [])

### nbfm_demod

- inputs: iq
- outputs: audio, event
- params.deviation_hz: integer
- params.audio_rate_hz: integer
- params.audio_lowpass_hz: integer or null (optional, default null)
- params.channel_hz: integer or null (optional, default null)
- params.deemphasis_us: integer or null (optional, default null)
- params.denoise: boolean (optional, default false)
- params.guard_db: integer or null (optional, default null)
- params.guard_hz: integer or null (optional, default null)
- params.squelch: boolean (optional, default true)
- params.squelch_db: integer or null (optional, default null)

### ook_demod

- inputs: iq
- outputs: audio
- params.symbol_rate_baud: integer

### passthrough

- inputs: iq, audio, video, text, event, sink
- outputs: iq, audio, video, text, event, sink
- params.kind: string (StreamKind: iq|audio|video|text|event|sink)

### pocsag_decode

- inputs: iq
- outputs: event

### psk_demod

- inputs: iq
- outputs: audio
- params.symbol_rate_baud: integer
- params.loop_bw: number (optional, default 0.02)
- params.order: integer (optional, default 2)
- params.rolloff: number (optional, default 0.35)

### rds_decode

- inputs: iq
- outputs: event

### resample

- inputs: iq
- outputs: iq
- params.out_rate_hz: integer

### sink_sondehub

- inputs: event
- outputs: 
- params.callsign: string
- params.alt_m: number or null (optional, default null)
- params.batch_ms: integer (optional, default 5000)
- params.lat: number or null (optional, default null)
- params.lon: number or null (optional, default null)
- params.url: string (optional, default "https://api.v2.sondehub.org/sondes/telemetry")

### slicer

- inputs: audio
- outputs: event
- params.chunk_bits: integer (optional, default 4096)
- params.constellation: any (optional, default null)
- params.differential: boolean (optional, default false)
- params.levels: integer (optional, default 2)
- params.mapping: any (optional, default "natural")

### ssb_demod

- inputs: iq
- outputs: audio
- params.sideband: string (Sideband: usb|lsb)
- params.audio_rate_hz: integer
- params.audio_bandwidth_hz: integer
- params.denoise: boolean (optional, default false)

### stream_in

- inputs: 
- outputs: audio
- params.job: string
- params.kind: string (StreamKind: iq|audio|video|text|event|sink)
- params.stream: string

### transcribe

- inputs: audio
- outputs: event
- params.gap_ms: integer (optional, default 300)
- params.language: string (optional, default "en")
- params.max_utterance_s: integer (optional, default 15)
- params.min_utterance_ms: integer (optional, default 700)

### vad

- inputs: audio
- outputs: audio, event
- params.hangover_ms: integer (optional, default 300)
- params.min_speech_ms: integer (optional, default 200)
- params.threshold_db: number (optional, default 6.0)

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
- Licensed bands: with the ACMA register attached (`--knowledge-acma`) and a station set (`station_set`), call `knowledge_lookup` on the band first, before demodulating anything. Report the nearest `tx` assignment's `licensee`, `site`, `distance_km`, `power_w` and decoded emission, and the answer's `as_of` (the register is as fresh as its last sync); say so when `as_of` is `null` or old. An assignment whose `reach` says the site cannot plausibly arrive here is context, not an explanation. A register match is a hypothesis about who transmits, never a proof of what a signal is: only a CRC-clean decode proves a protocol.

### Decoding APRS and reading measurements

To decode APRS from a source, start the job and tail its events in one call: `zeta job start --kind aprs_decode --source S --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000`. It prints `[job_result, frames_result, {"stopped": true}]` (the job is stopped after the tail; add `--keep` to leave it running); each frame's `payload` is one decoded packet (`src`, `dst`, `info`, `fcs_ok`). An empty frame list means no packet arrived within the timeout, not that none exist. Exit 2 with `[job_result, job_record]` means the job is not running: read `state.reason` (e.g. a stage that failed on the source's blocks). A `measure` job emits one event per second of IQ: `center_hz` and `bandwidth_hz` are the occupied band (spectrum bins at least 6 dB above the noise floor, the 15th-percentile bin of the band), `snr_db` is the peak above that floor, `burst` is true when the loudest 10 ms is more than 3 times the median 10 ms power, `symbol_rate_baud` is the keying rate when a symbol-rate line stands out and `null` otherwise, and `label.name` is one of `noise` (SNR under 6 dB), `carrier` (steady envelope, no modulation), `fm` (steady envelope with deviation), `fsk` (two tones 500-3000 Hz apart, on RF or in FM audio), `psk`, `ook` or `unknown`. Report `unknown` as unknown, with the measured fields. A `measure` event with `partial: true` covers a window cut short by a gap, lag or retune and spans only its samples; `partial: false` is a full second. `classify` and `measure` analyse only the selection/job band (a `measure` event's `center_hz` is absolute RF, within that band); `spectrum` covers the whole source band. A `spectrum` event is one row of power-averaged spectrum (20 per second of signal time) with `size` bins lowest frequency first, `bin_hz`, `center_hz` and the levels quantised as `q` (0-255 per bin): the dB of bin k is `floor_db + q[k] * step_db`; `floor_db` sits 5 dB under the row's 2nd-percentile bin, so a `q` near 10 is the noise floor and 255 means at least 127.5 dB above `floor_db`. A row may carry `masked: [[lo, hi], ...]`, bin index ranges (`lo` inclusive, `hi` exclusive) the receiver could not see honestly (a scan step's DC notch, its band edges): read them as no data, never as a quiet band; the `q` under them is present only so an older reader sees a whole row. A scan's steps alternate between two grids a quarter step apart, so a step's notch and its edges in one sweep fall a quarter of the way into a row of the next, clear of its mask: judge a frequency from two consecutive sweeps. On a local hardware receiver a `measure` job ignores the bins within 25 kHz of the capture centre (`job context` shows `measure.dc_mask_hz`, 0 when none), unless the box is centred on it (a lone job's capture: then the box is measured as today), so a box that sits beside the DC spike in a wider capture measures the signal.

A `measure` event also carries the burst features, always: `duty` (the share of the window the signal is on), `bursts`, `burst_ms` and `period_ms` (`null` when there is no burst train), and, when a keying line or tone clusters stand out, `order` (the FSK or PSK order: 2, 4, 8) and `tone_spacing_hz`. When the window holds a burst train (at least two on-runs, or one of 3 ms or more, with the signal off between them: the quiet chunks within 6 dB of the in-band noise floor, so clicks over a continuous signal do not count) the band, SNR, label, `symbol_rate_baud` and `order` are measured over the on-time only, not averaged into the silence. So a low `duty` means the features were measured over the bursts: report the burst timing (`duty`, `burst_ms`, `period_ms`) with them and say the measurement is over the bursts, never that the band is quiet or `noise` on the strength of a whole-window average. `symbol_rate_baud` comes from a cyclostationary line (the |x|^2 spectrum, or the envelope edges for on-off keying); a line is a hypothesis about the keying rate, and a short burst may give none (`null`).

### Decoding pagers, aircraft and FM data

Each of these is a mode row: `zeta modes list` gives its id (the `kind` to start), the `band` it expects with well-known `centers_hz`, and whether it is `ready`. `zeta modes show ID` prints the whole row. Read the band from the row, not from memory; the built-in rows say:

- POCSAG (`pocsag_decode`): 148-149 MHz in AU (Telstra at 148 637 500 Hz among them), 466 075 000 Hz in much of Europe, 929-932 MHz in the US. 12 500 Hz band.
- ACARS (`acars_decode`): 131 550 000 Hz (worldwide primary), 131 725 000 and 131 825 000 Hz (Europe), 130 025 000, 130 450 000 and 131 125 000 Hz (North America), 131 450 000 Hz (Asia). 10 000 Hz each.
- RDS (`rds_decode`): any FM broadcast station, 200 000 Hz band. The first event takes a few seconds.
- ADS-B (`adsb_decode`): 1 090 000 000 Hz with a 2 400 000 Hz band; under 2 000 000 Hz is refused. It needs dump1090 on the node's host: when it is missing, the startup handler report names it with its package and `modes_list` shows `adsb_decode` `unavailable` with the reason.

"Set up a background decoder for all ACARS and ADS-B activity":

1. `source_ensure` the radio at 130 925 000 Hz with 2 400 000 Hz. That band covers every ACARS channel above.
2. Start one `acars_decode` per channel inside it. They share the capture.
3. Start `adsb_decode` on a second source. A radio serves one band at a time, so on one radio it must wait until the ACARS jobs are stopped.

`knowledge_lookup` says which decoder proves a band's signature (`decoder` names the mode id, the job kind; `null` when no mode proves it yet). `classify` tries the best-scoring modes with a quick attempt (APRS, POCSAG, ACARS, RDS) as decode-to-identify and lists each under `tried` with its outcome; it never runs a program row such as ADS-B (`skipped_quick`: use `adsb_decode`). A CRC-clean decode is the strongest layer 2 evidence there is. A signature match without one is a guess: say so.

### Decoding modes

A decoder is a row, not code. `zeta modes list [--family F] [--state S]` shows every row with its `state`:

- `ready`: the node can start it. `job_start --kind <id>` takes the row's id.
- `unavailable`: the row is fine but its program is not on this host. `reason` names the program.
- `broken`: the row did not parse or validate. `file` is the file to fix and `reason` is `<field>: <error>`. A broken row never stops the node: it is reported at startup as `zeta: mode <id> (<file>): <field>: <error>` and starts nothing.

Never invent an id: `job_start` with a kind that is neither a built-in nor a row's id fails with an `unknown` error (`field: "kind"`, `known` the built-in kinds and every row id), and a row that is not ready fails `mode <id> is <state>: <reason>`.

**Handlers.** A program row (today `adsb_decode`, running dump1090) needs an external program. At startup the node prints one line per program: `zeta: handlers: <program>: found <path> (<version>)`, or `zeta: handlers: <program>: missing (apt install <pkg>; <url>) — modes: <ids>`. A missing handler is not an error and is never fetched or built by zeta. To install it, run the package command from the line (`apt install dump1090-fa` for dump1090), then restart the node: the row turns `ready`. A program outside `PATH` is named with `ZETA_HANDLER_<PROGRAM>` (`ZETA_HANDLER_DUMP1090`; `ZETA_DUMP1090` still works). Programs run with a cleared environment, as plugins do.

**Audio-input rows.** A program row can take demodulated audio instead of IQ. Its `decoder.demod` (`{kind: nbfm, deviation_hz, audio_rate_hz}`) names the demodulator and `input.container` (`raw` or `wav`) how the audio reaches the program; the node expands it to `channel → nbfm_demod (squelch off) → external_decode`. The sonde rows are the example: `rs41_decode`, `dfm_decode` and `m20_decode` run the rs1729 decoders (`rs41mod`, `dfm09mod`, `m20mod`; package `sonde-decoders`, <https://github.com/rs1729/RS>) over a 12 500 Hz channel anywhere in 400.05-406 MHz (`band.range_hz`, no `centers_hz`: a sonde's frequency varies by flight, so start the job on the measured centre). Their rows map the decoder's fields to `position` (`lat`, `lon`, `alt_m`) and name the GPS-time field in `time`, so events carry `position` and `t_ns`, and `summary` reads `{id} {lat?:.4},{lon?:.4} {alt?:.0} m {vel_v?:+.1} m/s` (a `?` field the decoder left out is dropped with its separator). One sonde per job. `zeta modes check --strict rs41_decode` passes against the real `rs41mod`; `dfm_decode` and `m20_decode` have no synthesised test yet, so they print `skipped: no test` (and `--strict` fails a row without one).

**Checking a row.** `zeta modes check [ID] [--strict]` runs each row's test on a local node over a synthesised capture (APRS, POCSAG, RDS, ACARS, mode S identity, NBFM tone, RS41 (`rs41`: `serial`, `lat`, `lon`, `alt_m`, `vel_v`, `frames`) and the generic `fsk`, `psk` and `ook` families, whose `bits` are hex or `pattern:<hex>`; impairments `snr_db`, `offset_hz`, `ppm` and `echo` `{delay_us, gain_db}`) and prints `<id>: ok`, `<id>: fail: <reason>` or `<id>: skipped: <reason>`. Skipped means no test, or the program is missing; `--strict` makes that a fail. It exits 0 when no row failed. A pass is recorded (`checked` in `modes list`) for program rows. It is local only: it is refused with `--node` and while `zeta serve` owns the state directory. Run it after writing a row and before trusting it.

**Writing a row.** Put one `*.json` file in the operator directory (`--modes-dir PATH`, default `$XDG_CONFIG_HOME/zetasdr/modes`, else `~/.config/zetasdr/modes`; the node reads it at startup). A row whose `id` matches a built-in replaces it; a new `id` adds a mode. Every field is required unless the schema marks it optional, and unknown fields are rejected. The simplest row is a copy of a built-in with a changed centre. Take `zeta modes show pocsag_decode`, keep the whole row, give it a new `id` and `name`, and put the centre you measured in `centers_hz` (148 812 500 Hz below is only a placeholder):

```json
{
  "id": "pocsag_local",
  "name": "POCSAG, local pager channel",
  "family": "pagers",
  "band": { "bandwidth_hz": 12500, "centers_hz": [148812500] },
  ...every other field of pocsag_decode unchanged...
}
```

Then `zeta modes list --state broken` must not list it, and `zeta modes check pocsag_local` must print `ok`. Centre the row on the measured signal, not on a raster. A graph row like this one reuses a built-in decoder at another centre; a program row declares its program, input format and output parser (`json_lines`, `sbs`, `nmea`, `kiss`, `text`, `multimon`); read `adsb_decode`'s row for the shape.

**What the events look like.** Every decoded event carries `protocol`, `t_ns` and `summary`, the row's one-line rendering (`{address}: {text?}` for POCSAG). Report `summary`; the other fields are the decoder's own.

**Subjects.** A row may name what its events are about: `subject` is `{key, name, waterfall, columns, heading, label, track_points, track_min_m}`. `key` is the event field naming the subject (`icao` for an aircraft, `id` for a sonde, `src` for an APRS station), so the page keeps one table row per key rather than one ribbon per event; `name` is the plural noun ("aircraft"); `waterfall` is `summary` (one line for the job) or `fold` (one line per subject); `columns` are the event fields shown, each with a `label` and a `unit` (`ft`, `m`, `kt`, `km_h`, `m_s`, `deg` or `null`) that the page shows as the operator reads it; `heading` and `label` name the event fields holding the heading in degrees and the name it is called by, or `null`; `track_points` (100 when left out, at least 4) and `track_min_m` (0 when left out, not negative) shape its track on the map: a fix joins when it is at least `track_min_m` metres from the last kept point, altitude counted where both have one, or when `heading` has turned more than 10 degrees from that point's, the marker and the line's end always at the latest fix, and at `track_points` the older half loses every other point, its first kept. A bad one is `broken` naming `subject.track_points` or `subject.track_min_m`. Every named field must be in the event schema, else the row is `broken` naming it (`subject.key`, `subject.columns[<i>].field`, `subject.heading`, `subject.label`), and a word outside `summary|fold`, the unit words or `m|ft` is `broken` naming `subject.waterfall`, `subject.columns[<i>].unit` or `position.alt_unit`. A mode that reports altitude in feet says so in its `position` with `alt_unit: "ft"` (`m` when left out): the event keeps its field, and `position.alt_m` is converted. A row without `subject` keeps one ribbon per event; an operator row may add one, as it may add a `position`.

**Quick answer or thorough.** `classify` is the quick answer: it scores every row against the layer-1 measurement and tries the best 5 with a short decode, listing each under `tried` with `proved`, `no_frame`, `skipped_band`, `skipped_span`, `skipped_quick` or `unavailable`. It never runs a program row. A `no_frame` means nothing decoded in that span, not that the protocol is absent. When `classify` leaves the answer open, or the signal may be ADS-B, use `identify`: `job_start --kind identify` on the box, or one-shot `zeta job start --kind identify ... --then-tail 3,10000`. The tail is capped at 10 000 ms (a larger timeout is `out_of_range`), so a one-shot `--then-tail` only sees a verdict that lands inside that window, while the dwells run in sequence (5 s for APRS, POCSAG and the sondes, 10 s for RDS and ACARS, 15 s for ADS-B). For anything longer, start `identify` against `zeta serve` (`--node`) and read `job_context` (`identify.verdict`), or `stream tail` the `candidate` events. It holds the radio, measures 1 s, then runs each fitting ready row as a child job in score order for the row's dwell. Read its `candidate` events in order: each has `mode`, `score`, `outcome`, `evidence`, `summary` and the child's `job`. A `proved` candidate ends the job with the verdict in `job_context.identify` and leaves that decoder running under `verdict.job`; stop it with `job_stop` when done (`--then-tail` already stops it unless `--keep`). An exhausted list ends with `verdict.mode: null` and the layer-1 features: report that as unknown, with those features. Do not guess a protocol from a `no_frame` list.

### Watching a band for sondes

Radiosondes are rare: Brisbane Airport launches about twice a day (the synoptic 00Z and 12Z releases, roughly 10:00 and 22:00 local, a flight of two to three hours), and a sonde on 400-406 MHz is there for that flight only. Nobody sits watching for one; a `watch` job does. It holds a band, spots a new signal in it, runs `identify` on the signal, and keeps the decoder that proves.

One radio: pin or start the watch on the band and let the decoder share the capture.

1. `job start --kind watch --source S --center-hz 403025000 --bandwidth-hz 5950000` (400-406 MHz; the centre sits off any channel of interest because the receiver's DC spike is there). It holds a rigid lease on the band, so other jobs on that source conflict until it stops.
2. `stream list`, then `stream tail` the watch's event stream. A `signal` event (`state` `new` or `gone`, `center_hz`, `bandwidth_hz`, `snr_db`) says something appeared; the watch's own events follow it: `identifying`, then `decoding` (`mode`, `job`, `placed`), or `unknown` with the layer-1 `features`. Report an `unknown` as unknown with those features; the watch tries the signal again after a minute, three times at most.
3. `job context --job WATCH` holds `watch.signals`, `watch.decoding` (the kept decoder's `mode`, `job`, `source`, `since_ns`) and `watch.current` (the identify run in progress). The decoder is an ordinary job in `job list`; its events carry `id`, `position` and `summary`. The watch's own identify runs are owned by it (`owner` in `job list`) and leave no history.
4. `job stop` on the watch stops its identify run and every decoder it keeps. A decoder whose signal is `gone` and silent for 120 s is stopped by the watch (a landed sonde).

The detector counts a bin occupied when it stands `threshold_db` (6) over the row's median bin, not its floor, and a run as a signal only when its energy, the sum over its bins of dB over that median, reaches `energy_db` (30): a synthesised RS41 at 6 dB channel SNR clears both, and on a noisy receiver raise `energy_db` (with `graph put` on the `detect` node) before `threshold_db`.

`--then-tail` on a watch tails its event stream: frame 1 is the `signal new`, frame 2 the watch's own event.

The identify run is tuned on the detector's centre, which is good to about one fft bin (about 1.5 kHz at 6 MHz), and `rs41mod` decodes only within about 1 kHz of the sonde's centre (measured on synthesised frames: ok at ±1000 Hz, nothing at ±1500 Hz). `identify` therefore re-centres each trial on the layer-1 measured centre (the verdict and the `unknown` record carry `recentred`), but only when the shifted band still fits inside the capture and the label is not `noise`. When a sonde shows as `signal new` and still ends `unknown`, run a `measure` job on the signal's band and start `identify` (or `rs41_decode`) yourself on the measured `center_hz`.

Two radios: with a second source, for example the field host's B210 attached as `--source-node b210=b210@http://field-host:7374,max_bandwidth_hz=...`, the watch keeps the first radio on the band and puts the decoder on the other one. `zeta source free --center-hz C --bandwidth-hz B` lists which sources could take a band now; it is a suggestion, not a reservation. Then `graph get` the watch, set `decode_on` to `free` (and `prefer` to the source ids you want first) on its `detect` node, and `graph put`; the decoder lands on a free source, and falls back to the watch's capture, saying so in the `decoding` event's `summary`, when none takes it. `prefer` takes ids from `source list` only: an unknown one is refused (`prefer[<i>]: unknown source <id>`). A limitation: the watch skips a new signal that another job already decodes only when that job holds a band on the watch's own source; a decoder you started yourself on the second radio is not seen, so the watch may identify the same sonde again and start a second decoder on it.

The page shows a watch as an outline on the waterfall with its signals marked ("Watch this band" in the box menu starts one). Its owned identify jobs are hidden. Events with a `position` appear on the map (`view set map on|off`): a marker and track per subject for a row that names one (thinned by the row's `track_points` and `track_min_m`, the marker turned to its heading and labelled with its name and altitude), per job otherwise, whichever source it runs on (the decoder on the second radio included), and with `view set home LAT,LON` (`none` clears it; while it is `none` the page uses the station's position, read once at page load, when `station_set` gave one) the jobs pane shows distance and bearing from home for a job with one marker. A row with `subject` columns also shows on the Subjects tab of the pane strip beside the radio tabs (one pane at a time under the waterfall, chosen by its tab or `view set pane subjects|none`; `view set subjects on|off` is the same choice, `auto` only shows the tab while such a job runs), one table row per subject in metres and km/h, climb rate in m/s; a subject silent for `subject_dim_s` (60) dims and for `subject_drop_s` (300) goes, and a click on a table row selects its marker and the reverse. This state lives on the page, so a refresh refills from live traffic. The map uses vendored Leaflet and fetches OpenStreetMap tiles only when the browser is online. The box menu's "Capture bursts" starts a `capture` job on the box (fitted to the signal first, as a demodulator is), and the Explore tab of the pane strip (`view set pane explore`, shown once a capture exists and selected the first time one appears) lists the captures newest first with What is this (`capture_classify`), Download (both SigMF files) and Delete (a second click within 3 s).

SondeHub is a feed out of the node, so nothing uploads unless the operator gave a callsign. Put `sondehub: {callsign}` (with `lat`, `lon`, `alt_m` only to override the station) on the watch's `detect` node (`batch_ms` and `url` are optional) and every decoder it keeps gains a `sink_sondehub` node (with no station set and no position given, the decoder is kept without the sink: its `decoding` entry carries `sink_error` and the summary says `no station set`; a hand-placed sink in that state idles, `job_context.sondehub.idle` true with `last_error`, and never fails its job); or `graph put` a `sink_sondehub` node on any sonde job yourself. `job_context.sondehub` counts `sent` and `failed` and keeps `last_error`; events SondeHub would reject (no position, no `datetime`, no `frame`, no numeric altitude) are skipped, not counted, so `failed` means transport trouble. `lat`, `lon` and `alt_m` may be left out: the node's station (`station_get`) supplies them, and the sink fails `sink.params.lat: no station set` when there is none. Do not invent a callsign or a position.

### Remote HF receivers (KiwiSDR)

A KiwiSDR source covers 0-30 MHz and delivers 12 kHz of IQ per connection, so `bandwidth_hz` is at most 12 000: work one HF channel at a time with `measure` jobs or `classify` selections inside the band you tuned (e.g. 7 100 000 Hz with 10 000 Hz for 40 m), not wide `spectrum` sweeps. A Kiwi has a limited number of user slots; when they are all taken the source faults with `too_busy` and the job fails with that reason: report it, do not retry in a loop. The connection opens on the first tune and holds a slot until the node exits. Reception is site-dependent: the receiver's location, antenna, local noise and the time of day decide what is audible, so an empty band on one Kiwi says nothing about the band elsewhere.

### Sources on another node (federation)

A `--source-node` source is a receiver attached to another ZetaSDR node, typically a Raspberry Pi at the antenna. That node ships narrowband IQ by default (250 kHz at most), which a LAN or WiFi link carries comfortably: work one channel at a time with `measure`, `aprs_decode` or `classify` inside it. Widen it only when the job needs it, with `max_bandwidth_hz=N` on the flag, and expect a slow link to drop blocks rather than slow down. A `bandwidth_hz` over the cap is `out_of_range` with `suggested` set to the cap. Each tune runs a `passthrough` job on the remote node, so that node's `source list` and `job list` show the source as held: another caller there sees the lease conflict instead of a silent retune. If the remote node goes away, the local source faults naming its URL and the job fails with that reason: report it, do not retry in a loop.

### External stages

A plugin registered with `--plugin KIND=PATH` is a stage kind like any other: it is listed in `schema` under `stages` with its `params_schema`, and you use it by adding a node of that `kind` to a job's graph with `graph get`, edit, `graph put`. No job kind starts it on its own. A plugin that crashes or misbehaves fails its job with a reason naming the plugin.

A plugin speaks wire protocol 1 or 2; its `hello` names the version and the node answers `run` with the same one. Protocol 2 adds the `video` stream: a frame carries one image, `{"width", "height", "format", "frame": N}` in its header (`format` `rgb8`, `gray8` or `png`, `N` the body's byte count) and the raw bytes as its body, `width x height x 3` for `rgb8`, `width x height` for `gray8`, any non-zero length for `png`. A body that does not fit its format is refused. A protocol-1 plugin that declares a `video` port is refused at startup; a `stream tail` of a video stream shows each frame as `{"width", "height", "format", "bytes": N}`. `sink` is carried by neither protocol.

### Bit streams

A layer-4 decoder (a signal with no known decoder, worked by hand) reports the bits it sliced as `event` frames of this shape: `{"protocol": "bits", "rate_baud": N, "bits": "<hex>", "count": N, "t_ns": N}`. `bits` is the sliced bits packed most significant bit first and written in hex, `count` how many bits it holds (the last byte is padded with zeros), `rate_baud` the symbol rate it was sliced at, `t_ns` the time of the first bit. An event is at most 64 KiB. No stage emits it yet; it is the shape a layer-4 stage will.

### Transcribing a channel

Start the demodulator, then attach the transcription to it; the attached job reads the
demodulator's audio, holds no radio lease and ends when that job stops:

1. `job start --kind wfm_demod --source S --center-hz 100500000 --bandwidth-hz 200000` (`fm_demod`, `am_demod` or `ssb_demod` for voice channels). Note the job id.
2. `job attach --kind transcribe --job THAT_ID`. Its chain is `stream_in → denoise → vad → transcribe` (without the `denoise` when the demodulator already denoises its own audio, below).
3. `stream list` shows two event streams on the attached job (`vad`'s and `transcribe`'s); `stream tail` them. A `transcript` event is one utterance (`text`, `language`, `start_ns`, `end_ns`); `vad` events mark where speech starts and stops. An empty tail means no utterance ended in the window, not that nothing was said.

Transcription is per demodulator: a `mix` (see "Listening to many channels") is not transcribed unless the operator asks for it.

The node needs a speech backend (`--stt-socket` or `--stt-node`); without one the attached job fails naming it, so report that and do not retry. Language: the attached job transcribes `en` unless it was attached with `language`; a language the backend does not list (see `GET /stt/languages`) fails the job naming the languages it has. Do not guess a language from the band.

Music and noise are gated, not separated: `vad` passes any loud audio, so a music station or a broadcast with a bed under the voice yields poor or empty text. Say so rather than reporting the text as the speech. Separation is a slot: an external audio-to-audio plugin (`--plugin separate=PATH`, a Demucs wrapper for instance) replaces the `denoise` node with `graph get` of the attached job, edit, `graph put`. The job restarts on `graph put`. `docs/speech.md` has the daemon install and the plugin stub.

`queued_ms` on a transcript is how long it waited for the speech daemon (at most 4 calls at once); `dropped` with `reason: busy` means the daemon had no slot for 30 s, `reason: queue` that the job's own queue of 4 was full. Utterances under `min_utterance_ms` (700) are not sent at all; the job's context counts them under `transcribe.short`.

An `fm_demod` shapes its audio as a voice receiver does: a 3.4 kHz low-pass (`audio_lowpass_hz`) and a 6 dB per octave de-emphasis (`deemphasis_us`, 530) after the discriminator take out the hiss above the voice that a weak signal carries; the audio-input decoder rows keep the flat discriminator they decode from (`null` for both). Every voice demodulator (`fm_demod`, `am_demod`, `ssb_demod`) also takes `denoise: true` on its node (`graph get`, edit, `graph put`; the page's outline menu offers "Denoise on/off"): RNNoise runs on its audio output, so the page, a `--then-tail` and any attached transcriber all get the cleaned audio, and a `transcribe` attached to such a job runs without a `denoise` stage of its own. It is a speech model: off by default, and never on a decoder row (a tone or a data burst comes out mangled). It adds one 10 ms frame of delay, which the audio frames' spans account for, so transcripts and ribbons keep their time. These params are carried like the squelch: a `graph put` that changes only them keeps the job re-expandable, and a `job retune` keeps them.

A `fm_demod` or `am_demod` has an automatic squelch: an idle channel publishes no audio, so a quiet transcript is normal. `squelch` events say why a row is quiet (idle, or open). It opens 6 dB over the tracked noise floor (`squelch_db: null`, the default) and closes after 300 ms under it. To fix or disable it, `graph get`, set `squelch_db` (an absolute threshold in dBFS, -120..=0, not dB above the floor) or `squelch: false` on the demodulator's node, `graph put`; such a put keeps the job re-expandable at later capture changes. A `--then-tail` of an idle squelched channel is empty: that is the squelch, not a fault.

### Reading transcripts

Every transcript event of an attached job is also written to the node's `transcripts.log`. `zeta transcripts tail --lines N --job ID` reads the last lines (default 50, one JSON line each: `job`, `source`, `center_hz`, `bandwidth_hz`, `start_ns`, `end_ns`, `text`, `language`, `dropped`, `error`), so you can answer "what was said on 476.4 MHz" after the fact without a live `stream tail`. It reads the state directory, so it does not work with `--node`. A line with `error` set is a failed utterance, not silence.

Digital voice decoders (DMR, D-STAR, M17) will be transcribed the same way: they produce audio, so `job attach --kind transcribe` to them.

### Taking the radio from another job

A source holds one tuning, and a second job that needs a different one gets a `conflict` error. Read it before acting: it names the holders (job ids, kinds and what each is doing) and lists options, the last being `retry with replace: [<ids>]`. Decide whether the holders matter. If they do not, or the user asked for the new job, retry the same request with `--replace ID[,ID]` (`job start`, `source ensure`; `replace` is an array in the op). The named jobs are stopped and the new job started in one step under the source's gate, so nothing else can slip in between. Replace is not transactional: if a holder you did not name still conflicts, the jobs you named stay stopped and the new conflict names the remaining holders. Every id must hold that source as a rigid lease (`unknown_job` or `job <id> does not hold source <src>` otherwise); naming an elastic job is that error and nothing is stopped, the elastic job keeps running. Never replace a holder you have not read, and say in the report what you stopped. Stopping or replacing a job is the operator's explicit decision; hand-made jobs survive any request that did not name them.

The web page's own `spectrum` job is elastic (`job start --elastic`, `spectrum` only): it never conflicts, never blocks a retune and follows each retune of its source, so it is not a holder to wait for or replace. `source list` shows such jobs under `elastic`, not `holders`. A scan on the source stops it, and the page restarts its view afterwards. After a restart of the node the capture is restored (the band bounding a source's rigid jobs, when the radio can capture it) and the jobs resume in it, a scan first, then widest first. `zeta ops tail --lines N` (default 50) shows the last applied ops with the transport that sent them (`local`, `http`, `ui`, `agent`): use it to see who changed the radio.

### Holding a band

To keep many channels running on one radio, pin the capture instead of letting each job retune it: `source pin --source S --center-hz C --bandwidth-hz B` (add `--replace ID` for the jobs in the way). Centre the pin between the channels you want, not on one of them, so no channel sits on the DC spike. Example: UHF CB (80 channels) is held by `--center-hz 476918750 --bandwidth-hz 1000000`; the lease may round up (1 024 000 Hz) and every channel fits. The pin retunes the radio to its band, even one inside a wider capture the page's spectrum holds (the spectrum follows); only when a decoder already holds the source inside the band does the pin join the current capture instead. Jobs started inside the pin run on it without a retune; the pin and the gain (`source gain --source S --db N`, or `--auto`) are saved and come back after a restart, and one-shot commands, read-only ones included, restore the pins and the gain of the sources they attach.

A start outside the pin is a `conflict` offering `unpin source S (source_unpin)` (and `stop one of the holders` only when jobs hold the source). A pin is not a holder and is not in `--replace`. Unpinning frees the radio for other work and does not stop the jobs that were inside it; they conflict with the next retune. Read the conflict, and unpin only when the user wants the pinned channels given up.

### Channel plans

`channel_plans` (CLI `zeta plans`) returns the plans the node knows with every channel's `number` and `center_hz`. Name a channel to the user by its number ("ch20", not 476.9 MHz) and start jobs at its `center_hz` with the plan's `bandwidth_hz`. The AU UHF CB plan interleaves two rasters: ch1-40 sit 25 kHz apart and ch41-80 fall halfway between them, so every channel has neighbours 12.5 kHz either side. A strong ch20 transmission leaks into ch59 and ch60 and used to open both squelches and echo the same words three times. An `fm_demod` on a plan channel therefore carries a guard: its squelch stays closed while a neighbour `guard_hz` away is more than `guard_db` stronger, and the `squelch` event says so with `guarded: true` and `adjacent_db`. When a quiet channel shows `squelch` and no transcripts, read `guarded` before concluding it is broken. The guard needs the squelch on (it is inert with `squelch: false`) and `graph put` with `guard_hz: null` turns it off. `transcribe` ignores utterances under `min_utterance_ms` (700) and a busy speech daemon drops with `reason` `queue` or `busy`; `queued_ms` on a transcript is the time it waited.

Plan ids come from `channel_plans`: read it before naming a plan. A band with no plan takes one `job_start` per measured station; never invent a plan. Broadcast FM is the example: it has no channel plan, so measure the band and start a `wfm_demod` on each station found.

To run demodulators on active channels only, not on all 80, use `examples/python/cb_watch.py` (`--dry-run` shows which channels the capture holds): it keeps one elastic `spectrum` job, starts `fm_demod` plus `transcribe` on a channel that stands 10 dB over the floor and stops them after 60 s of quiet. For every channel at once, `plan_start`.

### Stopping many jobs

"Stop all jobs" is one `jobs_stop` with `source`, `kind` and `plan` all `null`, not one `job_stop` per job: there are not enough turns for that. Name a `source`, `kind` or `plan` when the request does. Report how many stopped and any `failed`. `not_allowed` means the run may not stop jobs: tell the operator, do not retry.

### Starting a whole plan

"Transcribe all the UHF CB channels" is one operation, not 80 starts:

1. `source_list`: note the `holders` other than `elastic` (the page's spectrum job is not one) on the source you will use. A holder outside the plan's band will make the start a `conflict`.
2. `plan_start` with the plan, `channels: null` (or the numbers asked for), the `kind`, the `source`, and `attach: "transcribe"` when transcripts are wanted. For `fm_demod` on a 12.5 kHz plan give `deviation_hz: 2500`.
3. Read all four lists of the result and report them: `started` (with each `attached` and any `attach_error`), `existing` (channels already running an identical job, left alone), `refused` (channels whose own start failed, each with its error) and `warnings` (repeat them to the operator, the speech backend's capacity among them).
4. A `conflict` names the holders and starts nothing. Report it and stop: the operator decides whether those jobs go. Do not retry with `replace` or `job_stop` them unless the request named them.
5. A partial set is completed by running `plan_start` again: running channels come back as `existing`.

A result of `not_allowed` means the run may not do that: the agent is not allowed to stop jobs, or to put any in `replace`, unless the operator unlocked it for the run. Do not retry and do not work around it with other calls. Tell the operator to say `allow stops on`, or press the retry button, then ask again.

### Listening to many channels

"Let me hear everything" is one `mix_start`, not a player per channel. `sources: null` mixes every source of the node, a list only those; one mix spans any set of tuners. It takes no lease, tunes nothing and stops nothing, so start it whenever; running it twice on the same set answers the same job.

1. `mix_start`, then play the job's audio or `stream tail` its streams (`stream list` shows an `audio` and an `event` stream with provenance source `node` and the mix's `job`; the `ops` stream is the one with no `job`).
2. Its `event` stream publishes `active` (`job`, `source`, `center_hz` of every channel being heard) whenever that changes; a channel stays active for 500 ms after its last audio. `job_context.mix` holds `inputs` (how many jobs it mixes), `active`, `last` (the last channel heard), `muted`, `dropped_ms` and `underruns` (each per job), and `skipped` (jobs whose audio is not 48 kHz mono, each named once).
3. `dropped_ms` counts audio the mix threw away because a channel's queue ran over 200 ms; `underruns` counts the times a channel ran dry after it had started feeding. Both are per job, not per channel. Either means the mix could not keep time with that channel: report it with the job, do not hide it.
4. A mute is a channel, `{source, center_hz}`, not a job: `graph get` the mix, add it to `muted` on the `mix` node, `graph put` (this restarts the mix under its id). It matches any job on that source centred within 1 kHz, so it outlives the job: a channel muted and restarted stays muted, a fit that moves the centre a little keeps it, and a retune elsewhere is heard again. Mutes live in the mix, so stopping the mix (`stop all` included) forgets them.
5. `wfm_demod` and `ssb_demod` have no squelch: they always produce audio, so a mix holding one is open all the time until that channel is muted. Say so rather than calling the mix broken.

On the page, many idle plan channels fold into one `<n> idle <kind> on <plan>` group (more than 8 idle), but only on evidence the page has: a kind with no squelch, a watched job (at most 4) whose squelch is known closed for 60 s, or an unwatched job on a source covered by the mix the page is playing that the mix has not heard for 60 s. Without "Listen to all" most CB demodulators therefore stay unfolded: an unfolded idle channel is not broken. `view set side both|transcript|jobs|none` picks which side panes show (`jobs` gives the job list the whole column) and `view set theme console|plain` the look; in `console` job tags sit on a rail left of the frequency axis, in `plain` on the outlines.

Transcription happens per demodulator. Starting a mix attaches no transcript; `job_attach` a `transcribe` to each channel you want transcribed, as under "Transcribing a channel". Attaching a `transcribe` to the mix's output is the operator's option, alongside the per-demodulator ones: it runs overlapping voices together and names no channel (its log shows `node` and 0 Hz): attach it there only when asked.

### Fitting a demod to a signal

A box drawn by hand over a 12.5 kHz channel is about 100 kHz wide: 10 dB of extra noise in the audio and a centre a little off the signal. Fit the band to the signal, never to a raster (channels do not sit on a grid; a plan's channel numbers are names, not truth):

1. Measure the box: `job start --kind measure --source S --center-hz C --bandwidth-hz B --then-tail 1,3000`. Read `label.name`, `center_hz` (the occupied band's centre, absolute RF) and `bandwidth_hz` (the occupied width) from the event; an `ssb` label's `evidence` carries the dial frequency (`dial ≈ N Hz`). Started without `--then-tail`, the `measure` keeps running: `job_stop` it when you have read it.
2. A `noise` label (or `unknown` under 6 dB SNR) means nothing to fit: start at the box as drawn and say so.
3. Otherwise start the demodulator on the measured centre with the occupied width times 1.25, floored at 5000 Hz for `fm_demod` and `am_demod`, 150000 Hz for `wfm_demod`, and never wider than the box; an `ssb_demod` keeps its 6000 Hz band and only its dial moves (to the evidence's dial, else 1500 Hz below the occupied centre for USB, above for LSB): its channel passes half the band either side of the dial, and USB voice reaches 2.7 kHz above it.
4. A job already running is moved the same way with `job retune --job ID --center-hz C --bandwidth-hz B`: same id, its attached `transcribe` relaunched with it, its squelch settings kept, its channel-plan guard derived again at the new band. Inside the current capture nothing retunes the radio; outside it the radio moves when no other rigid job or pin holds it, else the `conflict` names the holders and `--replace` is the answer when they may stop.

The receiver's DC spike sits at the capture centre. On a local device `measure` does not measure the bins within 25 kHz of it when the box contains the centre but is not centred on it (a box the radio was tuned for is centred on the DC and is measured as today, spike included). So a box that contains the capture centre cannot fit a signal within 25 kHz of that centre: under the UHF CB pin at 476 918 750 Hz that is ch20, ch21, ch60 and ch61. Fit such a channel from a box that does not contain the centre, or with the outline menu's "Fit to signal" on a job whose band excludes it (a 12.5 kHz band on ch20 holds no DC and is measured unmasked). Pin the capture between the channels you listen to and box away from its centre.

The web page does this on its own: a demodulator started from a box is fitted first (the transcript says `fitted <kind> to <centre> MHz, <width> kHz (<label>)`, or `no signal found in <lo>-<hi> MHz; started <kind> at the box as drawn`, with `(the capture centre <mhz> MHz is in the box: the 50 kHz around it are not measured)` before the `;` when the box holds a local radio's capture centre), a running job's outline is dragged by its edges or its body and retuned on release, and a demodulator's outline menu offers Fit to signal and the 2.5, 5, 6.25, 12.5 and 25 kHz widths about the current centre (every job's offers Stop).

### Reading a source's health

`source list` gives each Soapy source a `stats` record (`null` for a recording, a KiwiSDR, a `--source-node` and a `--source-vrt` source; `worker` shows the child that owns the device (`pid`, `restarts`, `since_ns`; `null` when the source runs in the node)): `measured_rate_hz`, the samples per second the device actually delivered (an integer, `null` until two blocks have arrived), and `overflows`, `timeouts` and `restarts`. Compare `measured_rate_hz` with the pinned rate: within 1 % is healthy. `restarts` counts clock-guard stream restarts, each logged as `zeta: soapy <id>: samples ran <n> ms ahead of the clock; stream restarted`; more than 5 in 60 s fault the source with `soapy: samples arrive faster than <rate> Hz`. `overflows` and `timeouts` are zeta's counts of the driver's read results, not the device's own. `bank.dropped` in the same record counts input blocks the shared channel bank dropped under load; a number that climbs means the node cannot keep up, so drop jobs or narrow the pin.

`GET /health` carries `stages_running`, the live stage tasks: every graph stage plus one IQ forwarder per job on the radio, none for an attached job, and one loop for a scan whatever its graph. A number that keeps climbing after jobs are stopped is a leak: report it with the job history, do not restart quietly.

A `measure` job's `job context` carries `measure: {discarded, partial, last}`: windows discarded, windows emitted short, and the last cause (`gap`, `lag` or `retune`). A `measure` event with `partial: true` spans only the samples it held (at least 250 ms); read its fields with that in mind.

### A radio that comes back

A Soapy device runs in a `zeta soapy-worker` child that the node spawns, so a driver crash or a pulled cable faults the source and never ends the node. Expect this on a cable pull: within about a second `source list` shows `fault` as `no samples for 1000 ms (device unplugged?)` (the worker's exit 5), every job on the source fails with `source <id>: <fault>`, and `job_start` is refused with `source <id>: <fault>` (`source_ensure` and `source_pin` still answer `io: <fault>`). A driver crash reads `soapy worker for <id> died: signal 11 (SIGSEGV)` (or `exit code N`); a device that is not there reads `device not found: <args>`, and an open that fails `soapy open failed: <last stderr line>`. Nothing needs restarting: the node respawns the worker (1 s doubling to 5 s, reset after 60 s alive; every 5 s while the device is absent), logs `zeta: soapy worker for <id>: <fault>; waiting for the device` once and, when the device is back, `zeta: soapy worker for <id>: back after <n> attempts, <s> s`. Then it restores the gain and the pin and relaunches every job that failed with that fault, under its own id (a scan first, then widest first; an attached `transcribe` after its input; the `watch` re-adopting its decoders), logging `zeta: source <id>: recovered: pin <c>/<bw> Hz, gain <g> dB, <n> jobs relaunched`. A healed fault stays in `source list` until recovery has seen the jobs end, normally up to one poll (200 ms) after the hello; during that hold a `job_start` is accepted, and its job may fail with the fault and be relaunched under its id. Jobs you stopped, jobs that failed for their own reason and jobs that failed in an earlier episode stay as they are. The node awaits the worker's hello for up to 30 s (`soapy worker for <id>: no hello in 30 s`); on the field host the B210 says hello in 2-4 s.

`worker` in `source list` is `{pid, restarts, since_ns}` (`null` for a source that runs in the node): `restarts` counts respawns since the node started, `since_ns` is when the live worker said hello, and a `restarts` that climbs while the device is healthy means the driver keeps crashing: read the worker's own stderr lines, prefixed `zeta-soapy-worker[<id>]: `. A source that stays faulted while `restarts` rises is waiting for a device that is not on the bus. A device absent at startup attaches faulted (`device not found: <args>`) when an earlier start saw it, else the flag is refused (`device not found: <args> (never seen on this host; plug it in once)`, exit 1). `--soapy-in-process` opens the device in the node as before, for diagnosing the worker only: a crash then ends the node.

Running a second node that owns the radio and attaching it with `--source-node` is federation (a radio on another host), not crash protection; one node with `--source-soapy hackrf=driver=hackrf,gain=32` is the form for a local HackRF.

### Exploring an unknown signal

A signal nobody has named goes up a ladder; stop at the first rung that answers, and report what each rung found.

1. Direct. Layer 1 measures the band (`measure`, `classify`) and labels it; layer 2 scores the catalogue, the seed signatures and every mode row. `classify` tries the quick rows; `identify` runs each fitting row for its dwell, in score order, and publishes a `candidate` event as it reaches each row. A proof (a CRC-clean decode) ends the ladder and leaves the decoder running under `verdict.job`.
2. The register and the decoders it suggests. With the register attached, `knowledge_lookup`, `classify` and `identify` list the licensed assignments overlapping the band, nearest first, and a row whose width fits the licensed channel scores 0.2 higher, so the decoder that fits is tried before one that does not. The allocations ride along in the result whatever happens next. The `reach` of the answer (`km`, `class`, `antenna`, `note`) says how far a site can plausibly be heard on the station's antenna: skywave is not cut, tropo and UHF are; give it when the register is empty or the nearest site is out of reach.
3. Features and the register, as an `unknown`. With no proof the answer is `unknown`, never a guess: `verdict.mode` is `null` and the verdict carries the layer-1 features and `allocations`; the watch's `unknown` summary pairs the measurement with the register as `register says <EMISSION> (<bw> kHz <modulation> <info>) <licensee>, <site>, <d> km`. Report both together, for instance "unknown at 404.025 MHz: burst fsk, 16 kHz, 4800 Bd, 1 % duty every 1.0 s; the register says 16K0F1D, Unitywater, Example Suburb". Say which numbers were measured over the bursts (a low `duty`). A match between the measurement and the register's emission is a coincidence of features, not a decode: say "consistent with", never "is".
4. Captures. The explore policy of a watch, `on_unknown: capture` on its `detect` node (default `report`), writes an unknown signal's bursts to disk once per run (the count resets when the watch restarts) as a `capture` child, and publishes `capturing` and `captured`; `job_context.watch.signals[].captures` lists the ids. By hand, `job_start` a `capture` job on the signal's measured band, or use the box menu's "Capture bursts". `capture_list` shows the store newest first, `capture_show` one record, `capture_classify` classifies the band a capture recorded, so a signal that was gone before anyone looked can still be asked "what is this?", and `capture_delete` removes one (an agent run needs `allow.delete_captures`). The store keeps 256 MB and 7 days by default (`--bursts-keep-mb`, `--bursts-keep-days`); pinned captures are never expired. The page's Explore tab (`view set pane explore`) lists them with What is this, Download and Delete.

When `classify` answers `too_old` the ring has moved on: the error carries the ring's earliest start in its detail; re-issue on the latest span it still holds, and say so.

The playbook ends at classification and, when the operator accepts one, the signature written back so the next encounter is a layer 2 match. It stops before decoding the content of a third-party transmission: measuring, classifying, capturing a few bursts to identify the signal and saying who the register says is licensed is in scope; recovering and reading the messages of someone else's link is not, and interception law varies by country, so that is the operator's decision and legal responsibility. State this legal line once when a request goes that way, and do not repeat it. A known public protocol the node has a decoder row for (APRS, POCSAG, ADS-B, ACARS, radiosondes) is the ordinary use of the radio and is not held back by it. Bits and frame hypotheses are ops now (below); signature write-back arrives in a later plan.

### Working layer 4

When a signal has no known decoder and a capture of it exists, work its bits with the ops, in this order:

1. `capture_list` (narrow with `signal` when the band is known); take the capture's id from it, never invent one. No capture: start a `capture` job on the signal's measured band first.
2. `bits_start` with `capture`, `modulation`, `symbol_rate_baud` and `order`, each `null` to take it from the capture's feature record and its layer-1 label (`fsk` and `fm` give `fsk`; `psk` and `ook` themselves). It takes every capture of the capture's `set` in time order and builds `capture_in → resample → <demod> → clock_recover → slicer → bit_view`. fsk takes order 2 or 4, psk 2, 4 or 8, ook 2. It refuses with `other`: `no symbol rate measured; give symbol_rate_baud`, `no modulation measured (label <x>); give modulation`, `symbol_rate_baud <r> needs <4r> Hz; the capture holds <rate> Hz`, `symbol_rate_baud: at least 1` for a rate of 0, and a set of mixed rates or centres (`capture <id>: <rate> Hz differs from <first>'s <rate> Hz`). Short FSK bursts, real or synthesised, often have no measured rate: give it. On a refusal, report it and the features; do not guess a rate.
3. The chain's rate: about 8 samples per symbol for fsk and ook; for psk the smallest multiple of 4 times the baud that also covers a carrier offset of 2000 Hz (`psk_demod`'s coarse estimate is unambiguous only within the rate over twice the order). A 4-FSK chain takes `clock_recover` `algorithm: early_late`; `bits_start` sets it, a chain built by hand must too, or it mistimes.
4. The replay loops (`loop: true`, `realtime: true`, a 500 ms pause between passes) and the job runs until stopped. A one-pass chain (`loop: false`) drains to its end, its last chunk delivered, and ends `stopped`. An unpaced loop (`loop: true`, `realtime: false`) needs `loop_pause_ms` of at least 100.
5. Read, with `job_context`: `bit_view` (the hypotheses over the whole set: line code, frame period, sync-word candidates, CRC candidates, each with its counts; its window spans the set's captures, each a segment no word or frame crosses, the counts summed over them, and a looping replay's next pass starts it afresh, so a pass is never counted twice), `clock` (`rate_est_baud`, `locked`: a clock that is not locked, or an estimate far from the rate, means the rate or the modulation is wrong) and `slicer` (`symbols`, `events`, `stream`). Never tail the whole bits stream: `stream_tail` of `job_context.slicer.stream` with `max_frames` at most 4. A CRC candidate with `period_bits` 0 means the frames run from each sync to the next.
6. Change a running chain with `graph_get`, edit, `graph_put`: a `slicer` with `differential: true` or another `mapping`, or a `correlate` node (`{pattern, bits, max_errors}`) between the slicer and `bit_view`. `correlate` forwards every event and adds `sync` hits, so the hypotheses keep coming. A node's output feeds one edge: there is no fan-out.
7. PSK's phase ambiguity is not resolved: a BPSK stream may come out inverted and a QPSK stream rotated. `slicer` `differential: true` is the answer; try it before calling a PSK stream noise.
8. Report the hypotheses with their counts (`sync` hits, matching frames out of tried, CRC passes), and say which are guesses. When nothing checks, stop at "unknown" with the measured features: do not claim a protocol, a framing or a payload from a hypothesis without a CRC-clean check.
9. A chain of your own: `graph_start` runs a graph that has no radio (a `capture_in`, `stream_in` or `mix` at its root) on the node source (`zeta graph start --file`). `graph` and `bits` are reserved kind names, so `job_start` refuses them (an operator mode row with either id cannot be started).
10. You may stop your own `bits_start` and `graph_start` jobs when done (`job_stop`); never one you did not start. `jobs_stop` with a plan filter skips them (their centre is the capture's, not a channel they hold).
11. Write the result back as a signature so the next encounter is a layer 2 match: not yet an op (plan 19.3); until then report it.

### The rule

When nothing matches, say "unknown" and report the measured features (centre, bandwidth, modulation, symbol rate, burst timing). Do not guess.
