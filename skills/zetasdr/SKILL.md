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
- Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it. A `why` of `insufficient bandwidth: ...` names the source's measured ceiling, not a decoder gap: narrow the request or move to a wider radio rather than wait for a mode, and report it to the operator, who can pin or re-measure the cap. Never delete or edit the node's state files.
- Unknown signals are reported as unknown, never guessed.
- Ids are never invented: `source` values come from `source list`, `job` values from `job list`.
- Radios: when the request names a radio (a source id as a whole word), use it. Otherwise use the default radio the request's context names (`## This request` in the system prompt); with none, follow the `## This request` paragraph (under `focus` with no radio in focus, list the free radios and ask; under `focus-fallback`, the first free); without that section (an MCP client), the first free id `source_free` gives. The policy decides when the default radio cannot take the band (it does not cover it, a job or a pin holds it, it is faulted): under `focus`, the default, use only the default radio; start nothing, say why, name the free radios and ask which to use; never start on another unnamed radio (the gate answers `policy`). Under `focus-fallback`, use the first free radio in the preference order and say which and why. Under `free-first`, use the first free radio. Under `ask`, ask before starting. Never stop or replace a job to free a radio.
- Jobs belong to the operator: never `job_stop` a job or put one in `replace` unless the request names it, the run allows it or, for `job_stop`, you started it in this run. A `conflict` is reported with its holders; the operator decides.
- For a kind on every channel of a plan use `plan_start`, never one `job_start` per channel.
- Plan ids come from `channel_plans`: read it before naming a plan. A band with no plan takes one `job_start` per measured station; never invent a plan.
- To stop every job, or every job of a source, kind or plan, use `jobs_stop`, never one `job_stop` per job.
- For a named set use `set_start`, never its members by hand; set ids come from `set_list`.

## Running the node

Every command takes these global flags:

- `--state-dir PATH`: where jobs and captures persist (default `$XDG_STATE_HOME/zetasdr`, else `~/.local/state/zetasdr`). Pass the same directory on every invocation.
- `--source-file ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]`: attach a SigMF recording as source `ID`. With the range it plays as a radio tunable over `N`-`M` Hz (its `tune_min_hz`/`tune_max_hz` in `source list`): a band inside the range moves the capture's centre there, the samples staying the recording's (a test and demo aid); without it only the recording's own band is accepted.
- `--source-kiwi ID=HOST:PORT[/PASSWORD]`: attach a KiwiSDR receiver (0-30 MHz, at most 12 kHz wide) as source `ID`; the port is required and the password is never printed. Without `/PASSWORD` the password comes from `ZETA_KIWI_PASSWORD_<ID>` (`ID` upper-cased, other characters as `_`), which keeps it out of `ps`; the flag wins when both are given.
- `--source-position ID=LAT,LON[,ALT_M]`: where source `ID` is (degrees, metres; repeatable); a source that reports its own position (a KiwiSDR with GPS) overrides it. The station (`station_set`) stays the node's.
- `--source-node ID=SOURCE@URL[,max_bandwidth_hz=N]`: use source `SOURCE` of another ZetaSDR node, `URL` being its `http://HOST:PORT` (a `zeta serve --listen` node), as source `ID`. The node's secret comes from `ZETA_NODE_SECRET_<ID>` (`ID` mapped as for Kiwi passwords) and is never printed; without it the command is a usage error (exit 1). A remote source ships narrowband by default: `bandwidth_hz` is capped at 250 000 Hz (or the remote source's own maximum when smaller) unless `max_bandwidth_hz=N` (1-20 000 000) widens it explicitly. Each tune holds a `passthrough` job on the other node, so its `source list` and `job list` show who is using the source; the job is stopped when the tuning changes or this process exits normally (a killed process leaves it running; stop it there with `job stop`).
- `--source-vrt ID=udp://ADDR:PORT`: receive VITA 49.2 (VRT) packets, one per UDP datagram, on `ADDR:PORT` as source `ID`; `ADDR` must be a LAN address (`0.0.0.0` listens on every interface). The sender sets the frequency and rate: a job's band must lie inside the sender's last context packet (`source list` shows it as `current`; a job started before any context packet waits up to 1.5 s for one), otherwise the job fails with `unsupported` saying where the sender is; retune the sender, not the job. Packets that do not decode are skipped.
- `--source-soapy ID=ARGS`: attach a SoapySDR device as source `ID`, `ARGS` being SoapySDR device arguments such as `driver=hackrf,serial=...`. The device runs in a `zeta soapy-worker` child the node spawns and respawns: a driver crash or an unplugged device faults the source (`source list` shows `fault`, jobs on it fail with `source <id>: <fault>`) and never ends the node. The node then respawns the worker and waits for the device; when it is back it restores the pin and the gain and relaunches every job that failed with that fault under its own id, the `watch` and its decoders and an attached `transcribe` included, logging `zeta: source <id>: recovered: ...`. `worker` in `source list` shows the child's pid, how many times it was respawned and `transport` (`path` `shm`, a shared-memory ring the node creates per spawn, or `pipe`; `format`, `cf32` or `sc16`: a device whose samples are 16-bit puts them on the ring unconverted, half the bytes and twice the time in the same ring, and the node converts them; a worker of another protocol or ring version is refused with a fault naming both) with `transport_dropped`, `attachment` (`usb:<bus>-<path>@<speed>`, the link's negotiated speed in Mb/s, `null` where it cannot be told) and `link_measure` (the figures of the latest hello and the speed they were measured at). At open the worker probes the rates the device offers and reports the first clean one as `max_bandwidth_hz: {measured: N}`: a request wider than it is refused `out_of_range` with `why: insufficient bandwidth: ...`. A measured cap is cached in `sources/<id>.json` and passed to every respawn; a probe with no clean rate measures nothing and keeps the driver's cap. A `max_rate_hz=N` item in `ARGS` (zeta's own, like `gain=`) asserts a cap and skips the probe; it is read from `ARGS` at every start, never cached. It is the remedy when the journal shows `rate cap not measured: no rung clean` or, at every open, `rate probe: device lost at <rate>`. A device whose link carries more than one sample depth (a B2xx: `sc16`, `sc12` and the lossy `sc8`; `capabilities.depths`, each `{bits, lossless, max_bandwidth_hz}`) has each probed at open, and the top-level `max_bandwidth_hz` is the figure for the depth in force (`depth` in `source list`): the fastest lossless one unless `bits=N` in `ARGS` asks for another (the only way to a lossy one; a `scan` takes a lossy depth for its run with `bits`). Three more items in `ARGS` are zeta's own: `settle_ms=N` (0-10000) is how long a tune takes to settle after the device returns from it, in place of the radio's (1 ms on the B2xx family, 10 ms for a radio zeta has not measured), `usable=F` (0.5-1) the part of a capture the front end passes flat, in place of 0.9, and `bits=N`. While a tune blocks the reader the device overflows, and that is counted as `stats.tune_gaps`, not as `overflows`: `overflows` stays the figure for loss while streaming. A start that has no cached figures measures first, and says so: the journal reads `measuring what this radio's link can sustain (up to about <N> s; the host will be busy; max_rate_hz= skips it)` and, when the worker says hello, `measured in <N> s` (a B210's three depths take about 70-110 s, within the 150 s the node waits for the hello). A node's very first start shows only those two lines, since it serves after its sources' first hello; a respawn (a replug, a link-speed change, `source_measure`) also shows `worker.starting` (`what`, `detail`, `step`, `of`, `eta_ms`) in `source list`, a notice on the source's tab and the feed's `measuring` and `measured` events, and its line opens the parentheses with the reason (`link speed changed (<old> to <new>)` or `as asked`). `max_rate_hz=N` asserts the cap and skips the probe. The measured figures are cached in `sources/<id>.json` with the link's speed (`measured_at_speed`): a source measures again by itself only when its negotiated speed differs from the one its figures were measured at (a USB 3 radio on a USB 2 port, or the reverse); another port at the same speed keeps the cache. To measure again otherwise (the system stutters after devices were added around the radio, a new cable, a faster host), `source_measure`; an agent reports `insufficient bandwidth` to the operator, asks before stopping a job to measure, and never deletes or edits state files. A device absent at startup is attached faulted when the host has seen it before, else the flag is refused (exit 1). `zeta soapy-worker --id ID --args ARGS` is what the node spawns; it is not run by hand. The worker inherits the node's environment (the driver reads `UHD_IMAGES_DIR`, `SOAPY_SDR_PLUGIN_PATH`) minus the secrets: every `ZETA_*` variable whose name contains `SECRET`, `PASSWORD` or `KEY` (`ZETA_SECRET`, `ZETA_NODE_SECRET_<ID>`, `ZETA_KIWI_PASSWORD_<ID>`, `ZETA_STT_NODE_SECRET`, `ZETA_LLM_KEY` among them). A `gain=DB` item in `ARGS` is zeta's own: automatic gain is turned off and the overall receive gain set to `DB` dB when the device opens (logged with the device's range as `zeta: gain for <id>: ...`), and a `DB` outside that range refuses to attach the source (exit 1); without it the driver's default gain is used, which on a USRP is its minimum. A gain set live with `source_set_gain` is persisted and wins over `gain=` on the next start. `ppm=N` corrects the device's frequency error: the hardware is tuned `N` parts per million higher than every frequency zeta reports, so a device that reads high by `N` ppm (a carrier on 146.975 MHz seen at 146.9754) reads true. `zeta soapy find [--filter ARGS]` lists the devices attached to this machine as JSON. Only in a zeta built with `RUSTFLAGS="--cfg soapy"` (needs libsoapysdr-dev); otherwise both exit 1 saying so.
- `--soapy-in-process`: open `--source-soapy` devices inside the node instead of a worker child; for diagnosis only, as a driver crash then ends the node.
- `--ring-budget-mb N` (env `ZETA_RING_BUDGET_MB`): memory budget for one source's IQ history (default a quarter of RAM, at most 4 GiB). The ring keeps 10 s at narrow rates and shrinks to as little as 1 s at wide ones (a ring holds 8 bytes per sample, so 16 Msps gets 8 s of a 1 GiB budget); `iq read` and `classify` spans older than that fail `too_old`. The size chosen is logged to stderr as `zeta: ring for <source>: <s> s at <rate> Hz (<MB> MB)`.
- `--bursts-keep-mb N` (env `ZETA_BURSTS_KEEP_MB`) and `--bursts-keep-days N` (env `ZETA_BURSTS_KEEP_DAYS`): how much the burst capture store keeps (defaults 256 MB and 7 days; zero is a usage error). Expiry removes the oldest unpinned captures until the unpinned rest fit the megabytes, then every unpinned capture written more than the days ago; it runs when `zeta serve` opens (a one-shot expires at open only when given one of these flags while no serve owns the state directory, so a plain one-shot never drops what serve's policy keeps) and after each write, and a capture just written is spared by its own write. A pinned capture is never expired. An unreadable `bursts/index.json` is moved aside as `index.json.broken-<ulid>` and the index rebuilt from the `.sigmf-meta` files (pins are lost), logged as `zeta: bursts: index unreadable (<e>); moved aside`. Both belong to the serving node: with `--node` they are a usage error (exit 1).
- `--knowledge-acma URL[,reach_km=N]` (env `ZETA_KNOWLEDGE_ACMA`): attach the ACMA licence register service (an MCP server over HTTP): the service's `http://HOST:PORT` (a trailing `/mcp` is accepted) as the `acma` knowledge provider of `knowledge_lookup` and `classify`. `reach_km=N` (a positive number) replaces the reach rule's kilometres. The service is probed once at startup (one-shot commands pay at most 5 s when the host is unreachable); an unreachable one is logged as `zeta: knowledge acma: <url> unreachable: <e>` and the provider still attaches, giving no allocations until it answers. Needs `station set` for distances. It belongs to the serving node: with `--node` it is a usage error (exit 1).
- `--plugin KIND=PATH`: register the program at absolute `PATH` as stage kind `KIND` (lowercase, not a built-in kind). It is probed at startup; a failed probe exits 1 naming the kind and the reason (a `hello` of a protocol other than 1 or 2 is refused). A plugin runs with a cleared environment: only `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM` and `PYTHONPATH` pass through, and each `ZETA_PLUGIN_ENV_<NAME>` variable reaches it as `<NAME>`; nothing else (no `ZETA_SECRET`) does.
- `--modes-dir PATH`: the operator's decoder mode rows, one `*.json` file each, read after the built-in ones (default `$XDG_CONFIG_HOME/zetasdr/modes`, else `~/.config/zetasdr/modes`; a missing directory is no error). A row with a built-in mode's `id` replaces it. A row that does not parse or validate never stops the node: it is listed `broken` by `modes list` and reported at startup as `zeta: mode <id> (<file>): <field>: <error>`, and a broken file that meant to replace a built-in row leaves that row in force.
- `--sets-dir PATH`: the operator's named sets, one `*.json` file each, read after the built-in ones (`airband`, `airband_vhf`, `airband_vhf_and_sos`, `pagers_au`; default `$XDG_CONFIG_HOME/zetasdr/sets`, else `~/.config/zetasdr/sets`; a missing directory is no error). A set with a built-in set's `id` replaces it. A set names roles (`radios`), never sources, and never carries `source` or `replace` on a member. A set that does not parse or validate never stops the node: it is listed `broken` by `zeta sets` (`set_list`) and reported at startup as `zeta: set <id> (<file>): <field>: <error>`, and a broken file that meant to replace a built-in set leaves that set in force.
- `--themes-dir PATH`: the operator's themes, one folder each (default `$XDG_CONFIG_HOME/zetasdr/themes`, else `~/.config/zetasdr/themes`; a missing directory is no error), read after the built-in ones. A theme with a built-in theme's `id` replaces it. A folder that does not validate never stops the node: it is listed `broken` by `themes list` and reported at startup as `zeta: theme <id> (<path>): <field>: <error>`, and a broken folder that meant to replace a built-in theme leaves that theme in force.

Sources and plugins are not persisted: repeat the flags, with the same ids and kinds, on every invocation, `serve` included. A source id may be used once across all the source flags. `source list` shows each source's capability record: `remote` is true for a KiwiSDR, a `--source-node` and a `--source-vrt` source, `push` false for SoapySDR devices, and `fault` names a source that has failed (a full Kiwi, an unplugged device); a job on a faulted source fails with that reason.

There are two ways to run:

- One-shot, `zeta <group> <verb> ...`: opens the node, applies one operation, prints the JSON result and exits (0 on success, 2 with a JSON error). It never resumes jobs: a job started one-shot is persisted as `running` and runs once `zeta serve` starts. `job start --then-tail MAX_FRAMES,TIMEOUT_MS` keeps that one-shot node up after the start, tails the job's audio output when it has one (a demodulator), else its first event stream, then stops the job and prints `[job_result, frames_result, {"stopped": true}]`; add `--keep` to leave it running (`{"stopped": false}`). If the job is not running (e.g. failed) it prints `[job_result, job_record]`, the record's `state` carrying `status` and `reason`, and exits 2; `stream tail` on its own only sees streams of jobs running in the same process, and `job attach` needs its input job running in the same node, so attach against `zeta serve` (`--node`). A one-shot `iq read` or `classify` reads the ring that the command itself fills from the source, waiting up to 5 s for it to cover the span. One-shot commands, read-only ones included, restore the pins and the gain of the sources they attach (so even a one-shot `source list` tunes a pinned radio). One-shot commands, like `zeta serve`, start and probe every `--plugin` program at startup, so with plugins configured even a one-shot `source list` costs one process start per plugin.
- `zeta serve`: resumes persisted jobs and keeps them running until Ctrl-C.
- Against a running node, `zeta --node URL --secret SECRET <group> <verb> ...` (or the `ZETA_NODE` and `ZETA_SECRET` variables): the operation is sent to that node over HTTP and applied there, so jobs started this way keep running; `--state-dir`, the `--source-*` flags, `--plugin`, `--modes-dir`, `--themes-dir`, `--sets-dir`, `--knowledge-acma` and `--bursts-keep-*` belong to the serving node and are rejected (a usage error, exit 1); `--stt-socket`, `--stt-node` and `--soapy-in-process` belong to it too but are ignored, and so are `ZETA_KNOWLEDGE_ACMA`, `ZETA_BURSTS_KEEP_MB`, `ZETA_BURSTS_KEEP_DAYS` and `ZETA_RING_BUDGET_MB` from the environment. `zeta soapy find` always runs locally. `--then-tail` is not supported with `--node` yet (a usage error, exit 1): start the job, then `stream list` and `stream tail` against the node. Start that node with `zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) and a secret. `zeta-mcp --node URL` offers the same operations as MCP tools, and `zeta ask "REQUEST"` lets a local model call them.
- `zeta serve --listen PORT --llm-url URL --llm-model NAME` (the other `--llm-*` flags of `zeta ask` apply too) enables `POST /ask` with `{"request": "TEXT", "max_turns": 8, "allow": {"stop_jobs": false, "replace": false, "delete_captures": false, "accept_signatures": false}}` (`max_turns` 1-32, default 8; `allow` optional, all false by default: without them the agent's `job_stop` and non-empty `replace` calls, its `capture_delete` and its `signature_accept` and `signature_reject` are refused as `not_allowed`; `page_tools` optional, false by default: the web page sends `true`, which adds the page-only `view_set` tool, answered by the agent itself and applied by the page, never by the node): the node runs the agent against itself, so every operation it applies appears on the `ops` stream, and answers the transcript (`request`, `answer`, `calls`, `turns`) as `zeta ask` prints it, plus `allow` as the run had it and `turn_log`, one entry per model completion with its reasoning, content and call count. One run at a time (409 while one is in progress); 503 when serve has no `--llm-url`; a failed run is 502 with the error and the transcript so far; 504 after 300 s.
- `zeta serve --listen PORT --stt-socket PATH` (env `ZETA_STT_SOCKET`; default `$XDG_RUNTIME_DIR/voiced.sock` when that file exists) enables `POST /stt?lang=CODE` (`lang` defaults to `en`): the body is a 16 kHz mono `audio/wav` (at most 4 MiB), the node hands it to the speech daemon at that Unix socket and answers `{"text": "TRANSCRIPT"}`. The daemon is `local-voice` or any daemon speaking its protocol: one line per connection, `file:<absolute wav path>` answered `ok:<text>` or `error:<message>`, and `languages` answered `ok:en,de,...` (a daemon answering `error:` there is taken to know English only). Instead of a daemon, `--stt-node URL` (env `ZETA_STT_NODE`, secret in `ZETA_STT_NODE_SECRET`, never printed) forwards each request to that node's `POST /stt`; the two flags are exclusive, for every command (exit 1). A forwarded request carries `X-Zeta-Stt-Hop: 1`, and a node that would forward it again answers 508 (`stt forwarding loop`). `GET /stt/languages` answers `{"backend": NAME, "languages": [CODES]}` (503 without a backend). 415 when the body is not WAV, 413 when too large, 422 `{"error": "unsupported", "language", "supported"}` for a language the backend lacks, 502 when the daemon reports an error, 503 when serve has no backend or cannot reach it, 503 `{"error": "busy", "message", "wait_ms"}` when every one of the daemon's 4 slots stayed taken for 30 s (a node forwarding with `--stt-node` reports that as a `busy` drop, not an error), 504 when the daemon does not answer within 30 s of its turn.

`zeta stream save --stream ID --path FILE [--timeout-ms N] --node URL` writes the next video frame to arrive on a stream (a `video_decode` job's `video` stream) as a full-width PNG. It needs a running node; an audio or IQ stream is refused with a pointer to `zeta iq read`.

A video stream on the stream websocket is always PNG in binary mode: `GET /stream/ID?format=binary&video=smooth|sharp|thumb`. `smooth` (the default) is 360 px wide with every line of the field (360 x 288 for PAL: a field is already anamorphic, so its height is not scaled), one frame per field; `sharp` is full width at most 5 a second; `thumb` is 120 px wide with a third of the lines (120 x 96) at most 2 a second. Only the newest frame is sent, and `lagged.missed` counts the frames skipped.

While `zeta serve` runs it owns the state directory (it holds `serve.lock` there). Mutating one-shot commands (`job start`, `job attach`, `job retune`, `job stop`, `jobs stop`, `plan start`, `set start`, `set stop`, `mix start`, `bits start`, `graph put`, `graph start`, `station set`, `station clear`, `capture delete`, `signature propose`, `signature accept`, `signature reject`, `source ensure`, `source pin`, `source unpin`, `source measure`, `source gain`, and `origin lookup` with `--job`) are then refused with an `other` error naming the server's pid; read-only commands still work. Use `--node` to change a node that `zeta serve` owns.

Every operation the node applies is appended to `ops.log` in the state directory (rotated at 8 MiB) with its caller's transport (read-only operations are logged without their result); `zeta ops tail --lines N` prints the last N (local only). Every `transcript` event of a `transcribe` job is appended to `transcripts.log` in the state directory (rotated at 8 MiB) with the job, its input, the source and the channel's centre; `zeta transcripts tail --lines N [--job ID]` prints the last N (local only). Every event of every job on a source (what the source's feed carries: every decoded frame, detection, transcript, a `watch`'s and an `identify`'s own events, and an `origin_lookup`'s `origin` answer, whether or not a page or a `stream tail` is open; an `fft`'s rows stay out) is appended to `events.log` in the state directory (rotated at 32 MiB) as `t_ns`, `source`, `job`, `channel`, `center_hz`, `bandwidth_hz`, `start_ns`, `end_ns` and the event itself under `event`; `zeta events tail --lines N [--job ID] [--source ID] [--protocol NAME]` prints the last N (local only), so "what did ACARS hear overnight" is `zeta events tail --protocol acars --lines 500` after the fact, and the sites lit for a job are `zeta events tail --protocol origin --job ID`.

## Operations

### source_list

List the sources: id, capabilities, `current` band, `holders` (the rigid lease holders' job ids), `elastic` (the jobs following the capture, see `job_start`), `pin` (the band `source_pin` holds the source at, `null` for none), `fault`, the source's unrecoverable failure (`null` when healthy; a healed fault stays until recovery has seen the jobs end, normally up to one poll after the worker's hello, and a `job_start` in that hold is accepted, its job possibly failing and being relaunched under its id), `bank`, the channel bank shared by the capture's channels (`bin_hz`, `consumers`, `dropped`, the input blocks it dropped because it could not keep up; `null` for none), `gain`, the overall receive gain (`db`, `null` while automatic; `min_db`, `max_db`, `automatic`, `automatic_available`; `null` for a source without adjustable gain), and `stats`, the stream's counters since it opened (`measured_rate_hz`, the rate delivered over the last 10 s, `null` before any; `overflows`, `timeouts`, `restarts`, streams reopened because their samples ran ahead of the clock; `tune_gaps`, overflows the reader's own blocked tune caused, which are not in `overflows`; `null` for a source that keeps none), and `depth`, the sample depth in force on a link that carries more than one (`bits`, `lossless`, `max_bandwidth_hz`, one of `capabilities.depths`; `null` for a source with one), and `worker`, the child process the source runs in (`pid`, `restarts`, `since_ns`; `transport`, how its blocks reach the node: `path` `shm` or `pipe`, `format`, `ring_bytes`, `null` while none runs or from an older worker; and `transport_dropped`, what it could not hand over: ring pieces (2 MiB) the node had not released, or whole blocks its own pump could not take (pipe queue full, broadcast lagging); `attachment`, where its device is attached (`usb:<bus>-<path>@<speed>`, `null` where it cannot tell); and `link_measure`, the figures the latest hello measured (`max_bandwidth_hz`, `depths`) with the link speed they were measured at (`at_speed`), cached for the next start (see `source_measure`); and `starting`, what a worker not yet live is doing (`what` `measuring`, `detail`, `step` of `of`, `eta_ms`: the most time left), `null` otherwise; `null` for a source that runs in the node), and `ring`, the IQ history (`seconds`, `null` before the first block; a `classify` or `iq_read` span older than it fails `too_old`), and `position` `{lat, lon, alt_m, from}` (`from` `status` when the source reported it, `configured` from `--source-position`) where the source knows where it is, `null` otherwise, and `sweeps`, the scans the source is sweeping for by its own means (a source whose `capabilities.sweep` says it can: `min_hz`, `max_hz`, `bin_hz_min`, `rows_per_s_max`, `calibrated`, the levels in dBm; `null` for a source that cannot), which hold no band and are never among `holders`. Jobs on a faulted source fail with `source ID: FAULT` as their reason; a successful tune (`source_ensure`, `job_start`) clears it. A channelised job's `job_context` holds `channel.path`: `bank` (with `bin_hz`) or `direct` (with `reason`, why not the bank).

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

### source_measure

Drop the source's cached rate figures and measure its link again: the worker is ended and respawned to probe each sample depth's clean rate, as at a first start (up to a minute or more, the host busy meanwhile; `source_list` shows `worker.starting` while it lasts and faults the source until the new worker's hello, which heals it). The new figures govern tunes at once (`depth`), show in `worker.link_measure` and are cached for the next start; `capabilities` is set once a run: the first hello of the run that differs from it replaces it, and later ones leave it. A source measures again by itself only when its link speed changed; this is the operator's choice after devices were added around the radio. A source with no worker is refused; one held by a rigid or time-shared job or a pin is a `conflict` naming them (elastic jobs never hold it: they fail with the fault and are relaunched at the heal). The result is `ok` once the respawn is under way.

- source: string; an id from `source list`
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"source_measure","source":"<id>","replace":[]}`

CLI: `zeta source measure --source <id>`

Link diagnostics (the page's "Run link diagnostics" on a source with a `worker`): drop the source's cached rate figures and measure what its link sustains again, each sample depth probed as at a first start, about a minute with the host busy meanwhile. The figures belong to the link's speed (`worker.attachment`, `usb:<bus>-<path>@<speed>`): a radio whose USB link came back at another speed measures again by itself, so this is for a system that stutters after devices were added around the radio. It is the operator's call: run it only when asked, never with `replace` unless the operator agreed to stop those jobs. While it runs the source is faulted `measuring the link again` and `worker.starting` reports the probe's steps; the new worker's hello heals it and relaunches what failed. The new figures govern tunes at once (`depth`), show in `worker.link_measure` (`max_bandwidth_hz`, `depths`, `at_speed`) and are cached for the next start; `capabilities` is set once a run: the first hello of the run that differs from it replaces it, and later ones leave it, so read `depth` and `worker.link_measure` for the figures in force. Report each depth's rate against the one before. A source without a worker (a recording, a KiwiSDR, another node) answers `source <id> has no worker`; a source a decoder, a scan or a pin holds is a `conflict` naming them (the page's elastic spectrum never holds it).

### source_free

The sources that could take the band now: `free` lists, in id order, every source whose `tune_min_hz..=tune_max_hz` holds both edges of the band and whose `max_bandwidth_hz` its width, with no rigid holder, no fault, and no pin unless the pin's band holds the whole band (a pin admits jobs inside it; elastic jobs never make a source busy); `busy` lists every other source that covers the band with why: `holders`, `elastic`, `pin` and `fault` as `source_list` gives them. A source that cannot cover the band is in neither list. Takes no lease.

- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)

JSON: `{"op":"source_free","center_hz":144800000,"bandwidth_hz":12500}`

CLI: `zeta source free --center-hz 144800000 --bandwidth-hz 12500`

Ask which radio could take a band before starting a job on it, then start the job on one of the `free` ids. A free source is a suggestion, not a reservation: nothing is leased, and the next `job_start` may still conflict. Its `free` list is in id order, which is no preference: the request's default radio and preference order (`## This request`) come first. A source on another node (`--source-node`) can look free here while that node runs its own jobs on it; that surfaces as a `conflict` on `job_start`.

### source_set_gain

Set the source's overall receive gain live; the result is the gain after the change (`source_list` shows it as `gain`). Persisted in the state directory and re-applied when the node opens.

- source: string; an id from `source list`
- gain_db: number or null (dB)

JSON: `{"op":"source_set_gain","source":"<id>","gain_db":null}`

CLI: `zeta source gain --source <id> --db 40`

Set the source's overall receive gain live (`gain_db` in dB within `source list`'s `gain.min_db..=max_db`), or `null` for the driver's automatic gain where `gain.automatic_available`. Persisted: it is re-applied when the node restarts and wins over `gain=` in the device args. A source without adjustable gain (a recording, a KiwiSDR, another node) answers `source has no adjustable gain`. On the CLI, `--auto` passes `null`.

### job_start

- kind: string (JobKind: passthrough|spectrum|fm_demod|wfm_demod|am_demod|ssb_demod|aprs_decode|pocsag_decode|rds_decode|acars_decode|adsb_decode|measure|scan|identify|watch|transcribe|mix|capture|ais_decode|dfm_decode|flex_decode|hfdl_decode|m17_decode|m20_decode|rs41_decode|vdl2_decode|video_decode; the built-in kinds and the embedded mode rows, a node's own `schema` adds its operator rows)
- source: string; an id from `source list`
- center_hz: integer (Hz)
- bandwidth_hz: integer (Hz)
- deviation_hz: integer or null (Hz)
- sideband: string or null
- dwell_ms: integer or null; `scan` only, in ms (1-5000)
- average: integer or null; `scan` only, sweeps averaged (1-64)
- bits: integer or null; `scan` only, bits per sample: one of `capabilities.depths[].bits` in `source list`
- method: string or null; `scan` only, `stepped` or `source`; `null` lets the node choose
- bin_hz: integer or null (Hz); `scan` only, the bin width in Hz asked of a source that sweeps for itself (1-1000000)
- elastic: boolean
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"job_start","kind":"passthrough","source":"<id>","center_hz":144800000,"bandwidth_hz":12500,"deviation_hz":null,"sideband":null,"dwell_ms":null,"average":null,"bits":null,"method":null,"bin_hz":null,"elastic":false,"replace":[]}`

CLI: `zeta job start --kind passthrough --source <id> --center-hz 144800000 --bandwidth-hz 12500`

`fm_demod` is narrowband FM voice; `wfm_demod` is wideband broadcast FM (mono, 50 us de-emphasis, 15 kHz audio; give it a band of about 200000 Hz). `am_demod` is AM voice (envelope detector, 5 kHz audio, slow AGC; give it a band of about 10000 Hz). `ssb_demod` is SSB voice (2800 Hz audio, slow AGC; `center_hz` is the suppressed-carrier (dial) frequency, not the middle of the voice band, and the band must cover both sides of it: give it about 6000 Hz). All four output 48 kHz mono audio: an audio frame's payload is `{"sample_rate_hz", "channels", "samples": N, "peak"}`, `channels` 1 here, the samples interleaved and `samples` the per-channel count (a frame without `channels` means 1). `fm_demod` shapes its audio as a voice receiver does (a 3.4 kHz low-pass and a 530 us de-emphasis after the discriminator: `audio_lowpass_hz` and `deemphasis_us` on its node, `null` for the flat audio a decoder wants), and every voice demodulator (`fm_demod`, `am_demod`, `ssb_demod`) takes `denoise: true` on its node to run RNNoise on its audio output (off by default; speech only; one 10 ms frame of delay the frames' spans account for; set with `graph put`, kept through a retune; a `transcribe` attached to such a job skips its own `denoise` stage). `fm_demod` and `am_demod` carry an automatic squelch: it opens 6 dB over the channel's tracked noise floor (never taken above -40 dBFS: with no fixed threshold `floor_db` reads at most -40) and closes after 300 ms under it. While closed the job publishes no audio (an attached transcriber idles; `--then-tail` tails the audio, so an idle channel's tail is empty) and the first block and each change publish a `squelch` event (`open`, `power_db`, `floor_db`) on the job's event stream. The demod node's params set it with `graph get` and `graph put`: `squelch_db` is an absolute threshold in dBFS, -120..=0 (`null`, the default, is the automatic one; a value outside the range fails the job with `squelch_db <v> out of range -120..=0`), and `squelch: false` turns it off. `aprs_decode`'s demodulator runs with the squelch off. `fm_demod` on a channel of a plan (`channel_plans`) carries an adjacent-channel guard: its squelch stays closed while a channel `guard_hz` away is more than `guard_db` (12) dB stronger, so a transmission on ch20 does not open ch59 or ch60. The `squelch` event carries `adjacent_db` and `guarded`; once guarded, the gate stays held until the neighbour falls 2 dB under `guard_db`. The job's `job_context` keeps the last `squelch` event under `squelch` (removed when a run has the squelch off), so a client that subscribes after the gate changed reads it there. `graph put` with `guard_hz: null` on the demod node turns it off. `pocsag_decode` decodes POCSAG pagers (512, 1200 and 2400 baud detected automatically) from a 12.5 kHz FM channel into one event per message (`address`, `function`, `kind`, `text`). `flex_decode` runs multimon-ng on the node's host over a 25 000 Hz channel at the measured centre, one event per page like POCSAG; its events carry `access: 'restricted'`. `rds_decode` reads the RDS data of a broadcast FM station (give it the same 200000 Hz band as `wfm_demod`) into events carrying `pi`, `ps` (station name) and `rt` (radio text) as they complete; the first event takes a few seconds. `adsb_decode` decodes Mode S / ADS-B at 1090000000 Hz with `bandwidth_hz` 2400000 (anything under 2000000 is refused) by running dump1090 on the node's host; one event per decoded message (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`, `track_deg`), position and identity arriving in separate messages. When the host has no dump1090 the node's startup handler report names it (`zeta: handlers: dump1090: missing (apt install dump1090-fa; <url>) — modes: adsb_decode`), `modes_list` shows `adsb_decode` `unavailable` with that line as its `reason`, and `job_start` fails with `mode adsb_decode is unavailable: <reason>`. `acars_decode` decodes ACARS (VHF, AM, 2400 baud MSK) from a 10000 Hz channel on 131550000 Hz (worldwide primary), 131725000 / 131825000 (Europe), 130025000 / 130450000 / 131125000 (North America), 131450000 (Asia) into one event per CRC-clean block (`reg`, `flight`, `label`, `text`, `downlink`: true for a downlink); a downlink whose text is a position report (labels H1, 80, 16, 3L, QQ) also carries `lat`, `lon`, sometimes `alt_ft`, and `position`. `vdl2_decode` decodes VDL Mode 2 (VHF data link, D8PSK) from a 25000 Hz channel on 136975000 Hz worldwide (136725000 / 136775000 / 136825000 / 136875000 in Europe) by running dumpvdl2 on the node's host: one event per AVLC frame keyed on the ICAO address `icao` (`addr_type` tells an aircraft from a ground station), with the ACARS fields (`reg`, `flight`, `label`, `text`) and a `position` on ADS-C reports; pin the radio first beside the ACARS jobs (`source_pin`, then `plan_start`, then the job, or it is a `conflict` naming them). `hfdl_decode` decodes HFDL (HF data link) on an HF source such as a KiwiSDR by running dumphfdl on the node's host: `center_hz` is the listed dial frequency from the operator's frequency list (dumphfdl's `etc/systable.conf` upstream), not a raster, `bandwidth_hz` 6000, the signal 1440 Hz above it; a ground-station squitter within 40 s proves the channel. `ais_decode` hears one AIS channel (A or B) and emits one event per message, vessels as subjects by MMSI; `plan_start ais_vhf` runs both. `rs41_decode` decodes a Vaisala RS41 radiosonde anywhere in 400050000-406000000 Hz (`dfm_decode` and `m20_decode` a Graw DFM and a Meteomodem M20 alike) by running the rs1729 decoder on the node's host over NBFM audio: start it on the sonde's measured centre (its frequency varies by flight; never a raster) with `bandwidth_hz` 12500, one sonde per job, and each event carries `id`, `position` and the frame's GPS time as `t_ns`. `deviation_hz` is the FM peak deviation for `fm_demod` and `wfm_demod` (100-75000 Hz; `null` means the kind's default: 5000 Hz for `fm_demod`, right for standard voice FM, use 2500 for narrow 12.5 kHz channels; 75000 Hz for `wfm_demod`, right for broadcast FM). Every other kind takes `null`. `sideband` is `usb` or `lsb` for `ssb_demod` (`null` means `usb`; amateur convention is `lsb` below 10 MHz, `usb` above); every other kind takes `null`. On the CLI, omit `--deviation-hz` and `--sideband` for `null`. `scan` sweeps a band wider than one capture: the node steps the source across it in captures of its whole `max_bandwidth_hz`, each step advancing by the capture's usable part (`usable_fraction` in the source's capabilities: 0.9 on a SoapySDR device, `usable=F` in its args to change it; a `--source-node` takes its remote's; 1.0 on the other kinds), takes `dwell_ms` of samples on each (20 ms unless `job_start` asks otherwise, 1-5000; at least four FFT frames and never more than the source's ring holds; `scan.dwell_ms` in its context is the dwell in force, `scan.dwell_ms_asked` the one asked when they differ), and emits `spectrum` rows whose `center_hz` is that step's (frame `channel` `scan/<step index>`), wrapping until stopped. With `average` N (1-64, default 1) each row is the mean power of its step's last N sweeps of the same grid (an even step with the even sweeps of its centre; N is lowered to what an eighth of the node's ring budget holds for every step's rows, and then `scan.average` is the one in force and `scan.average_asked` the one asked), and every row carries `sweeps`, how many it averages so far (1 without averaging; the averages start over when the scan restarts or its grid changes). The look is the operator's choice: many quick looks averaged (20 ms × 16) lower the noise ripple as one 320 ms dwell does while each step is still revisited every sweep; a long look (250 ms × 1) sees a faint steady signal in one dwell; the default (20 ms × 1) is the fastest sweep. With `bits` (one of the source's `capabilities.depths[].bits`; `null`, the default, is the source's own `depth`) the scan holds the source's link at that sample depth for its run, steps at that depth's `max_bandwidth_hz`, and puts the source's own depth back before anything else is granted the source; `scan.bits` and every row's `bits` say the depth in force (on a source with depths). A lossy depth (8 bits on a B210) sweeps faster, but a weak signal beside a strong one in the same capture is lost in its quantisation noise: it is for a coarse survey of occupancy, never for faint signals, and never the default. A `bits` the source does not carry is refused naming its depths. Alternate sweeps shift the step grid down by a quarter of the stride, `step_hz` (the odd sweep has at most one more step), so a step's centre and edges in one sweep fall a quarter of the way into a row of the next and every frequency is seen clean in one of two sweeps, whatever edge mask is under `(1 - u) / 2 + u / 8` for the usable fraction `u` (0.1625 at 0.9, 0.125 at 1.0); `scan.alternate` in the job context gives `shift_hz` and `steps_shifted` (`null` for a one-step span). A scan on a local hardware receiver masks each row's centre notch (`notch_hz`, 50000 Hz), and every scan masks each row's outer edges that the usable fraction leaves unused (`edge_mask`, `(1 - usable_fraction) / 2` of the bins each side: 0.05 on a SoapySDR device, none where the whole capture is usable), never less whatever `edge_mask` its stored node says. A row with masked bins carries `masked: [[lo, hi], ...]` (bin index ranges, `lo` inclusive, `hi` exclusive): those bins are no data, not low power; their `q` is still present. `scan.mask` reports the values in force (a step narrower than four notches gets no notch; a span no wider than one capture's usable part is one step, asking for the span divided by the usable fraction so the unused edges it masks lie outside the span; it does not alternate and masks no more than those edges, since nothing could fill a wider mask in), and `graph get`, edit the `fft` node's `notch_hz`/`edge_mask`, `graph put` changes them (keep `edge_mask` under that bound, 0.1625 on a SoapySDR device, or the two grids' masks overlap). Of the scan's stored `fft` node only `notch_hz`, `edge_mask`, `dwell_ms`, `average` and `bits` are read (`graph put` changes the look as it changes the mask; the scan restarts): its `size`, `window` and `rows_per_s` are information only (the scan derives them per step), and a `graph put` whose graph has no `fft` node is re-expanded with the default mask on the next launch rather than refused. A scan time-shares the radio: while it runs, every other `job_start` and `source_ensure` on the source is a `conflict` naming it, decoders included, and `source_list` shows the step it is on as `current`. Stop the scan to decode. A scan's `job_context` holds `scan.steps` (how many captures one sweep takes), `scan.step_hz` (the stride), `scan.rate_hz` (each capture's width; both rewritten at a sweep's start when the source's capture, usable fraction or depth changed, a rate cap measured after the scan started, or a scan's `bits` taking another depth's width), `scan.dwell_ms`, `scan.average`, `scan.bits`, `scan.alternate` and `scan.mask`: report step counts from it, never from guesswork. `scan.timing` is the cost of the pace, rewritten about once a second: `sweep_ms` (the last whole sweep), `step_ms` (the median step), `tune_ms` (`median` and `max`), `slow_tunes` (tunes over ten times the median: the AD9361's calibrating tunes on a B210), `skipped` (steps whose samples did not arrive; both counted since the last write), `fft_wait_ms` (time the loop waited on the FFT), `hz_per_s` (the span over the sweep) and `at_ns`. Report how long a sweep takes from `scan.timing.sweep_ms`, never from step counts or the dwell: the radio sets the pace (a step is its tune and the dwell's samples, the FFT running beside the next tune, and the step is the source's whole capture, not a fixed width), and a scan started before the source's rate cap was measured sweeps at the asserted width until the cap arrives. A source delivers only settled samples after a tune (the first block of a new centre is taken after the radio settled), so a step's first row is as good as its later ones; a `--source-node` source takes its far node's, and a KiwiSDR drops the frames still in flight at the old frequency. `identify` is the thorough answer to "what is this?": started like a decoder (`job_start` with `kind: identify` on the box's centre and band), it holds a rigid lease on that band, measures it for 1000 ms (layer 1), scores every mode row against it and tries each ready row that fits, one at a time in score order, as an ordinary child job on the same band (`job_list` shows it, `source_list.holders` shows both while it runs) for the row's `identify.dwell_ms` (5000 ms for APRS, POCSAG and the sonde rows, 10000 for RDS and ACARS, 15000 for ADS-B and VDL2, 40000 for HFDL). Each row it considers publishes one `candidate` event on its event stream: `mode`, `score`, `outcome` (`proved`, `no_frame`, `skipped_band` for a row wider than the band, `unavailable` for a row whose program is missing or whose job would not start), `evidence`, `summary` (`<mode>: <outcome>`) and `job` (the child's id, `null` when none ran). A proof ends the job `stopped` with `job_context.identify` = `{"verdict": {"mode", "label", "summary", "evidence", "job"}, "attempts": n}` and leaves that mode's job running under the id in `verdict.job`, already decoding; stop it with `job_stop` when done. A list exhausted with no proof ends it `stopped` with `verdict` `{"mode": null, "label": <layer-1 name>, "features": <the layer-1 record>}`: report that as unknown with those measured features. Either way the verdict is also the stream's last event, one `verdict` (`mode`, `label`, `center_hz` the measured centre, `job`, `allocations`, `summary`: `<label> at <MHz>: <the decoder's summary>`, or `unknown at <MHz>: <label> <features>; register says ...` as a watch's `unknown` reads). Stopping an `identify` job stops the child it is trying; a source that faults fails it with the fault as its reason. With `--then-tail`, an `identify` that ends with a verdict during the tail is a success: the candidates are printed and the proved job (`verdict.job`) is stopped, or left running with `--keep` (`{"stopped": false}`). `watch` keeps an eye on a band and decodes what appears in it (with `modes` on its `detect` node it decodes every channel of the span; `modes`, `max_decoders`, `hold_ms`, `cooldown_ms`, `confirm_ms`, `seed_known` and a `lost` event's `why`, one of `gone and silent`, `not seen again`, `unproven`, `relaunch failed`, are described in "Decoding every channel of a band" in the field craft): `job_start` with `kind: watch` on the band (400-406 MHz is `center_hz: 403025000`, `bandwidth_hz: 5950000`; put the centre off any channel of interest, the receiver's DC spike sits there). It holds a rigid lease on the band like `identify` and runs `fft → detect` over the capture: `detect` publishes a `signal` event (`state` `new` or `gone`, `center_hz`, `bandwidth_hz`, `snr_db`, `first_ns`, `last_ns`) for each run of bins that persists (at most 10 `signal` events a second, the rest delayed), and each new signal no other job's band overlaps gets an `identify` run on its measured centre, one at a time, best-scoring first, on the band the best-scoring fitting row expects (12500 Hz for the sondes). A proof leaves the decoder running: inside the watch's capture with `decode_on: capture` (the default; one radio does both), or with `decode_on: free` on a free source other than the watch's (`source_free`), the ids in `prefer` first, back in the capture when none takes it. `on_unknown: capture` (with `capture: {bursts, window_ms, pre_ms, post_ms, timeout_ms}`) captures an unknown signal's bursts once per run as a `capture` child, publishing `capturing` and `captured`; `job_context.watch.signals[].captures` lists them. A signal no row proves is identified again after 60000 ms while it persists, at most 3 times; a decoder whose signal is `gone` and that has sent no event for 120000 ms is stopped (a landed sonde). The watch's own events, `protocol: watch`, go on the same event stream: `state` `identifying` (`signal`, `job` the identify run), `decoding` (`mode`, `job`, `source`, `placed` `capture` or `free`; the `summary` says why it fell back to the capture), `unknown` (`features`, the layer-1 record: report it as unknown with them) and `lost`, each with `summary` and `t_ns`. `job_context.watch` holds `signals` (each with `tries`, `next_retry_ns`, `handled`), `decoding` (`mode`, `job`, `source`, `signal`, `since_ns`, `placed`) and `current` (the identify run in progress, else `null`). `detect` also honours a row's `masked` ranges (those bins take no part in a run) and keeps to the watch's own band: in a wider capture (a pin, a shared one) the bins outside it take no part either, so a narrow watch announces only signals inside its box. Tune it with `graph get`, edit the `detect` node's params, `graph put`: `threshold_db` (6, over the row's median bin), `energy_db` (30, the least sum over a run's bins of dB over that median), `persist_ms` (3000; a signal also needs 3 sighting rows before `new`), `width_hz` (`[4000, 30000]`), `dc_mask_hz` (25000), `edge_mask` (0.05), `spurs_hz` (`[]`), `gone_ms` (30000), and the policy `decode_on`, `prefer`, `sondehub`, `on_unknown` (`report`, the default, or `capture`) and `capture`. `sondehub` (`{callsign, lat, lon, alt_m, batch_ms, url}`, the last two optional: 5000 ms and SondeHub's telemetry endpoint) appends a `sink_sondehub` node to every decoder the watch keeps, fed by its events; without it nothing is uploaded. The `sondehub` block is checked like the detect params: a bad one is refused at `job_start` and `graph put`, naming `sondehub.<field>`, before anything starts or restarts. If the sink cannot be added to a kept decoder it runs without it (the `decoding` event's `summary` says so and its `decoding` entry carries `sink_error`); one that cannot be relaunched at all is `lost`. `sink_sondehub` can be put on any sonde job with `graph put` (an `event` edge from the decoder's event node): it sends the `rs41`, `dfm` and `m20` events that carry a `position` (with a numeric altitude), a `datetime` and a `frame` to SondeHub in a `PUT` every `batch_ms`, and `job_context.sondehub` holds `sent`, `failed` and `last_error` (a failed batch is counted and dropped, not retried). The identify runs and their trial jobs are the watch's (`owner` is its id in `job_list`) and leave no history; the decoders it keeps are ordinary jobs. `job_stop` on the watch stops its identify run and every decoder it keeps, and so does a watch that ends `failed`; a `graph put` on it or a node restart re-adopts them instead, stopping any whose signal does not reappear within `gone_ms`. A `job_start` or `graph put` with an `edge_mask` outside 0..0.5, a `width_hz` not ordered `[min, max]`, a bad `sondehub` or `capture` (naming `capture.<field>`) or a `prefer` naming a source not in `source_list` (`prefer[<i>]: unknown source <id>`) is refused naming the field, before anything starts or restarts. `transcribe` is not started on the radio: `job_start` refuses it; use `job_attach`. `capture` writes a signal's bursts to disk as SigMF windows (`burst_capture`: `bursts`, `window_ms`, `pre_ms`, `post_ms`, `timeout_ms`, `whole`; a signal already on when it starts, which makes no burst edge, is cut into back-to-back windows after 500 ms, `job_context.capture.whole_auto` true) and ends `stopped` with `job_context.capture`; one started by hand classifies its first window at its end into `job_context.capture.classified` (`labels`, `tried`, `allocations`). When the job ends with a features-only signature proposed from its captures (confidence 0.3), `job_context.capture.proposal` holds the proposal's id (`signature_show` gives its name, confidence and evidence): it waits for the operator (`signature_list`, `signature_accept`). `kind` also takes the id of any ready mode row (`modes_list`), an operator's own rows included; an id that is neither a built-in kind nor a row's fails with an `unknown` error (`field: "kind"`, `known` the built-in kinds and every row id), a row that is not ready with `mode <id> is <state>: <reason>`. `elastic: true` (spectrum only) makes the job follow the source's capture instead of holding it: it never conflicts, is relaunched on every retune and is listed under `elastic` in `source_list`; a rigid request inside the capture elastic jobs follow is granted on it without a retune, one outside it retunes the source; the web page's own spectrum job is elastic. `replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder. An identical rigid start (same source, kind, band, `deviation_hz` and `sideband`) on a running job is refused as `duplicate` naming that job; stop it or use it. `m17_decode` decodes M17 digital voice (4FSK 4800 baud) on any 2 m or 70 cm channel at the measured centre, 12 500 Hz band; its audio is ordinary job audio (play it, or `job_attach` transcribe), its events name the stream's callsigns; it needs `c2dec` (package codec2) on the node's host.

### job_attach

Attach a job of an attachable `kind` (`transcribe`) to the running job `job`, a job whose kind emits audio: the voice demodulators and any mode row whose `outputs` lists `audio`. It reads that job's audio output stream (the last one in its graph order when it has several; audio passed between two of its stages does not count) instead of the radio, holds no lease, copies the job's `source` and `tuning`, and records it as its `input`. It follows its input through a `job_retune` or a `graph_put` restart: it is relaunched under its own id on the input's new stream once the input runs again (`failed` with the reason when it cannot be). It ends `stopped` when the input job stops or fails for its own reason. On a node restart it resumes after its input, or is `failed` with `input job <id> not running` when the input does not run.

- kind: string (JobKind: passthrough|spectrum|fm_demod|wfm_demod|am_demod|ssb_demod|aprs_decode|pocsag_decode|rds_decode|acars_decode|adsb_decode|measure|scan|identify|watch|transcribe|mix|capture|ais_decode|dfm_decode|flex_decode|hfdl_decode|m17_decode|m20_decode|rs41_decode|vdl2_decode|video_decode; the built-in kinds and the embedded mode rows, a node's own `schema` adds its operator rows)
- job: string; an id from `job list`
- language: string or null

JSON: `{"op":"job_attach","kind":"transcribe","job":"<id>","language":null}`

CLI: `zeta job attach --kind transcribe --job <id>`

Attach a processing chain to a running job's output instead of the radio: `transcribe` reads the audio of a job whose kind emits audio: the voice demodulators and any mode row whose `outputs` lists `audio` (`m17_decode`). It cleans it (denoise, voice gate) and emits one `transcript` event per utterance (`text`, `language`, `start_ns`, `end_ns`) plus `vad` events as speech starts and stops; it holds no lease, follows its input through a `job_retune` or a `graph_put` (relaunched under its own id) and ends when the input job stops. `language` is a BCP-47 code (`null` means `en`; the backend's languages are in `GET /stt/languages`). To transcribe a station: `job_start` a `wfm_demod` on it, then `job_attach` with `kind: transcribe` and that job's id. The attached job fails with a reason naming the speech backend when the node has none or it cannot serve the language. Every other kind (`identify`, `watch`, the demodulators and decoders) runs on the radio and is refused here as `<kind> is not attachable`. A stage output marked `role: composite` (a video demodulator's composite) is never a job's audio: `job_attach`, the mix and `--then-tail` skip it (a `stream tail` on it still reads it).

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

Start a mix: a job that sums the audio of every other job with an audio output on `sources` and plays it as one stream, so whatever is talking is heard. It takes no lease, tunes nothing and blocks no job (its record's source is `node`, its band 0); jobs started or stopped later join and leave by themselves within a second. It outputs `audio` and `event` streams; each change of who is talking publishes `{"protocol": "mix", "active": [{"job", "source", "center_hz"}], "t_ns", "summary"}`. Attached jobs, a `watch`'s or `identify`'s own jobs, other mixes and jobs centred on a plan channel that carries no voice (UHF CB ch22 and ch23) are not mixed. With a mix of the same set of sources already running the result is its id; `null` matches only `null`, so a list naming every source is another mix. It runs until stopped, and a node restart resumes it after the jobs it mixes.

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

Run the classifiers over the capture's whole window as `classify` does over the ring (the record's stored `allocations` take part): `{labels, tried, allocations, origins}` (`origins` is `origin_lookup`'s answer for the capture's band and source, scored on the measured layer-1 label; a hand `capture` job's `job_context.capture.classified` carries the same). A `no_frame` does not prove the protocol absent. A window shorter than a row's `identify.min_span_ms` is `skipped_span`.

### capture_delete

Delete a capture and its files. A pinned capture is refused. Deleting is the operator's decision: through `ask` it needs `allow.delete_captures`.

- capture: string

JSON: `{"op":"capture_delete","capture":"<id>"}`

CLI: `zeta capture delete --capture <id>`

Delete a capture and its files. A pinned capture is refused (`capture <id> is pinned`). Deleting is the operator's decision: through `ask` the run needs `allow.delete_captures` (`zeta ask --allow-delete`; on the page, `allow deletes on`), else the call is refused as `not_allowed`: report that, do not retry. Never delete a capture the request did not name.

### signature_list

The learned signatures (`state_dir/signatures`), newest first, narrowed to `state` (`proposed`, `accepted`, `rejected`, `superseded`; `null` for all). Each record has `id`, `name`, `state`, the waveform (`min_hz`, `max_hz`, `bandwidth_hz`, `modulation`, `order`, `symbol_rate_baud`, `tone_spacing_hz`, `burst` `{duty, burst_ms, period_ms}`), `framing` (`null`, or the `frame_decode` params that checked), `decoder`, `decoder_file`, `decoder_note`, `confidence` (0.3 for features only, 0.6 with a framing, 0.9 with a CRC that checks on at least 5 frames), `evidence`, `provenance` (`source`, `station`, `captures`, `jobs`, `allocation`, `proposed_ns`, `proposed_by`, `accepted_ns`, `accepted_by`, `superseded_by`, `superseded_ns`), `matches` and `last_matched_ns`.

- state: string or null

JSON: `{"op":"signature_list","state":null}`

CLI: `zeta signature list [--state S]`

A learned signature is what the node, or the operator by hand (`signature_propose`), proposes about a signal no mode row knows; it waits as `proposed` until the operator accepts or rejects it. `state` `null` lists every record. Take ids from here, never invent them.

### signature_show

One learned signature's record. An unknown `id` is `unknown` (`field: "id"`, `known` the store's ids).

- id: string; an id from `signature list`

JSON: `{"op":"signature_show","id":"<id>"}`

CLI: `zeta signature show --id <id>`

One record by `id`. An unknown `id` is an `unknown` error (`field: "id"`, `known` the store's ids): take ids from `signature_list`.

### signature_propose

Propose a signature by hand from a capture: its feature record and layer-1 label (a `noise` capture is refused), every capture of its set, the nearest `tx` assignment it kept and the station. `name` `null` gives the node's `<modulation> <width> kHz at <MHz>`; `framing` is the `frame_decode` params that checked (refused when the stage refuses them), `null` for none. The record is `proposed`; a proposal is harmless: it waits for the operator.

- capture: string
- name: string or null
- framing: object or null

JSON: `{"op":"signature_propose","capture":"<id>","name":null,"framing":null}`

CLI: `zeta signature propose --capture <id> [--name NAME] [--framing-file FILE]`

On the CLI, `--framing-file FILE` holds the `framing` JSON, the params of the `frame_decode` stage (under Stage kinds), e.g. `{"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"}`; a framing the stage refuses is `other` with `framing: <reason>` naming the field. An unknown `capture` is `unknown`; a capture without a feature record (`capture <id> has no feature record`) or labelled `noise` (`capture <id> is noise: nothing to propose`) proposes nothing. The confidence is 0.3 without a framing and 0.6 with one. Report the proposal's `id`, `name` and `confidence`.

### signature_accept

Accept a proposed signature; `name` `null` keeps the proposed one. Its provenance captures are pinned. A record not `proposed` is refused (`signature <id> is <state>, not proposed`). Accepting or rejecting is the operator's decision: through `ask` the run needs `allow.accept_signatures` (`zeta ask --allow-signatures`; on the page, `allow signatures on`), else `not_allowed`: report the proposal, do not retry.

- id: string; an id from `signature list`
- name: string or null

JSON: `{"op":"signature_accept","id":"<id>","name":null}`

CLI: `zeta signature accept --id <id> [--name NAME]`

Through `ask` without `allow.accept_signatures` the call is refused as `not_allowed`: report the proposal (its `id`, `name`, `confidence` and `evidence`), do not retry. Never accept a proposal the request did not name. The captures it was learned from are pinned: never expired nor deleted. A record not `proposed` is `other` (`signature <id> is accepted, not proposed`). Accepting a framed signature (a `framing` with a `crc`, and a `symbol_rate_baud`) also writes its decoder row, `learned_<id>` (the id lowercased), to `learned_<id>.json` in the operator's modes directory (`--modes-dir`, else `$XDG_CONFIG_HOME/zetasdr/modes` or `~/.config/zetasdr/modes`) and reloads the rows: a frame row of the signature's band, demodulator and framing with the `opaque` parser, proved by one CRC-clean frame, listed by `modes_list` at once. The record's `decoder` names the row and `decoder_file` its file: start it as `kind` the record's `decoder` (use that field; never build the id). A file already there is never overwritten (`decoder_note`: `row learned_<id> already present: kept as is`), so the operator may edit the row freely (a parser, a subject); with no modes directory (`no modes directory: row not written`) or a failed write (`row not written: <error>`) the signature is still accepted and `decoder` stays `null`. A features-only signature writes no row. When a built-in or operator row later proves its band (a `proved` in `classify` or `capture_classify`, an `identify` verdict, or a mode job's first proof), an accepted signature whose band holds the proved centre, the proved band 0.5 to 2 times its bandwidth, is `superseded` (`superseded_by` the row's id; its captures stay pinned) and no longer labels matches; a `learned_*` row never supersedes.

### signature_reject

Reject a proposed signature (the record stays, `rejected`). A record not `proposed` is refused (`signature <id> is <state>, not proposed`). Accepting or rejecting is the operator's decision: through `ask` the run needs `allow.accept_signatures` (`zeta ask --allow-signatures`; on the page, `allow signatures on`), else `not_allowed`: report the proposal, do not retry.

- id: string; an id from `signature list`

JSON: `{"op":"signature_reject","id":"<id>"}`

CLI: `zeta signature reject --id <id>`

Through `ask` without `allow.accept_signatures` the call is refused as `not_allowed`: report the proposal, do not retry. Never reject a proposal the request did not name. The record stays, `rejected`; its captures are not pinned.

### plan_start

Start `kind` on every channel of the channel plan `plan` (`channel_plans`), or on the channels numbered in `channels`, each at its channel's `center_hz` with the plan's `bandwidth_hz`, and attach `attach` to each started job. Under the source's gate, in this order: everything is checked first (an unknown `plan` or channel is `unknown`, a channel listed twice, a kind that does not run per channel — `spectrum`, `scan`, `watch`, `identify`, `mix`, an attachable kind — and a non-attachable `attach` are `other`; nothing has happened yet); a channel where an identical rigid job already runs is `existing` and skipped; with `attach`, its stage must be ready, else the op is refused with the stage's reason; the `replace` jobs are stopped; then one lease decision for the band bounding the channels to start (lowest channel edge to highest), checked as a rigid `job_start` of that band would be: inside the capture no tune, outside it a tune when only elastic jobs hold the source, else a `conflict` naming the holders and nothing starts; a band wider than the source is `out_of_range` on `bandwidth_hz`. Each channel is then started in channel-number order inside that capture: one that fails is listed under `refused` with its error and the rest go on. With `attach`, each started job (and each `existing` one without one) gets one attached, except on a channel that carries no voice (UHF CB ch22 and ch23, every ACARS channel), which a warning names; a failure is that channel's `attach_error`, the job kept. The result: `capture`, `started` (`channel`, `center_hz`, `job`, `attached`, `attach_error`), `existing` (`channel`, `center_hz`, `job`, `attached`), `refused` (`channel`, `center_hz`, `error`) and `warnings` (strings to repeat to the operator; empty when there is nothing to say). The jobs are ordinary jobs; running the same request twice starts only what is missing.

- plan: string; an id from `channel_plans`
- channels: array or null; channel numbers of the plan (integers), `null` for all
- kind: string (JobKind: passthrough|spectrum|fm_demod|wfm_demod|am_demod|ssb_demod|aprs_decode|pocsag_decode|rds_decode|acars_decode|adsb_decode|measure|scan|identify|watch|transcribe|mix|capture|ais_decode|dfm_decode|flex_decode|hfdl_decode|m17_decode|m20_decode|rs41_decode|vdl2_decode|video_decode; the built-in kinds and the embedded mode rows, a node's own `schema` adds its operator rows)
- source: string; an id from `source list`
- deviation_hz: integer or null (Hz)
- sideband: string or null
- attach: string or null
- language: string or null
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"plan_start","plan":"uhf_cb_au","channels":null,"kind":"passthrough","source":"<id>","deviation_hz":null,"sideband":null,"attach":"transcribe","language":null,"replace":[]}`

CLI: `zeta plan start --plan uhf_cb_au --kind fm_demod --source <id> [--channels 1,2,5-8] [--attach transcribe] [--language de] [--replace ID,ID]`

Starts `kind` on the channels of a plan (`channels`, `null` for all of them) at each channel's `center_hz` with the plan's `bandwidth_hz`, under one lease decision for the band that bounds them: inside the current capture nothing retunes, otherwise the source is tuned when only elastic jobs hold it, and a scan or a rigid holder elsewhere is a `conflict` naming the holders with nothing started. Check `source_list` first. `kind` must run on the radio (not `transcribe`, `spectrum`, `scan`, `watch` or `identify`). `deviation_hz` and `sideband` are passed to every start as on `job_start` (`null` is the kind's default; give 2500 for `fm_demod` on 12.5 kHz channels). `attach` is `null` or `transcribe`, attached to every channel started or already running except the data channels (UHF CB ch22 and ch23, every ACARS and AIS channel: their demodulators start, untranscribed, and a warning names them), and needs a ready stage (a speech backend for `transcribe`), else the op is refused with the stage's reason: start without `attach`. `language` must be `null` without `attach`. `replace`: as on `job_start`. The result lists `started` (channel, `center_hz`, `job`, `attached`, `attach_error`), `existing` (channels an identical running job already holds, skipped), `refused` (channels whose own start failed, with the error; the others went on) and `warnings`: repeat the `warnings` to the operator. Run it again to complete a partial set: running channels are `existing`, so nothing doubles.

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

List the streams: id, kind and provenance. One is always the node's own `ops` event stream (provenance source `node` and no `job`; a mix's streams have source `node` too, with their `job`): one event per applied operation with its outcome and caller (`transport`: `local`, `http`, `ui` or `agent`), logged after the operation completes. One more per source is its feed (kind `event`, provenance that source and no `job`): every event frame of every job on the source, each with its own `job`, `channel` and span, spectrum rows excepted. `stream_tail` a feed to see what a radio is doing in one call.

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

Work a capture's bits: takes every capture of the named capture's `set`, in time order, and starts `capture_in → resample → <demod> → clock_recover → slicer → bit_view` from the named capture's feature record, each `null` taken from the features (`modulation`, one of `fsk`, `psk` and `ook`, from the layer-1 label: `fsk` and `fm` give `fsk`, `psk` and `ook` themselves; `symbol_rate_baud` and `order` from the record, `order` 2 when it has none; fsk takes 2 or 4, psk 2, 4 or 8, ook 2). The capture is resampled to about 8 samples per symbol for fsk and ook, and for psk to the smallest multiple of 4 times the baud that also covers a carrier offset of 2000 Hz (the coarse estimate is unambiguous only within the rate over twice the order), moved to a rate the resampler serves exactly. A signal off the capture's centre whose recorded band reaches past the resampler's passband (its offset plus half its `bandwidth_hz` over 0.45 times that rate) is shifted to its recorded `center_hz` first, unless layer 1 labels the capture `noise`: `capture_in → channel [→ resample] → <demod> → ...`, the `channel` stage at the measured centre with the measured bandwidth × 1.25 (at least 4 × the baud, at most what the capture holds about that centre: its rate less twice the shift) and an output rate the capture rate over a whole divisor (the resampler follows unless that is already the chain rate). 4-fsk and ook take the `early_late` clock; fsk's demodulator is given the record's measured `tone_spacing_hz` (it tracks the tones itself when the record has none); `capture_in` loops, paced, with a 500 ms pause between passes, and the job runs until stopped. Refused with `other`: a field neither the request nor the features give (`no symbol rate measured; give symbol_rate_baud`, `no modulation measured (label <x>); give modulation`), a rate the capture cannot hold (`symbol_rate_baud <r> needs <n> Hz; the capture holds <rate> Hz`, 4 samples per symbol at least; a rate of 0 is `symbol_rate_baud: at least 1`), a measured centre the capture holds under 4 × the baud about (`the measured centre <c> Hz leaves <room> Hz of the capture <lo>..=<hi> Hz about it, under the <floor> Hz the chain needs (4 x <baud> Bd)`), and a set whose captures differ in rate or centre (`capture <id>: <rate> Hz differs from <first>'s <rate> Hz`); an unknown `capture` is `unknown`. The record's kind is `bits`, its source `node`, its band the capture's, `job_context.bits` the plan (`captures`, `modulation`, `symbol_rate_baud`, `order`, `tone_spacing_hz`, `rate_hz`, `center_hz` the centre the chain runs on and `shift_hz` its offset from the capture's, `bandwidth_hz`, `padded_bandwidth_hz` the channel band uncut and `clamped` whether it was cut to the capture, and `channel_rate_hz`, `null` (and `padded_bandwidth_hz` `null`) when the chain does not shift); the slicer's `bits` events are on `job_context.slicer.stream` and `bit_view`'s latest hypotheses in `job_context.bit_view`. Change a running chain with `graph_get`, edit, `graph_put`. The result is the job's id, as `job_start` answers; one-shot `--then-tail` tails the `bits` events. The agent's own `bits_start` jobs count as jobs it started.

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

Three providers answer: the built-in seed signatures; `catalogue`, the 594-signal iNTERCEPT catalogue (Apache-2.0; one signature per frequency range of each signal overlapping the band, `bandwidth_hz` its widest bandwidth, `modulation` the first it lists); and `learned`, the operator's accepted signatures (`signature_accept`) whose band overlaps, with `tier: learned`. Every signature also carries `order`, `tone_spacing_hz`, `burst` (`duty`, `burst_ms`, `period_ms`), `sync` (the framing's sync word, hex), `encoding`, `crc`, `confidence` and `tier`, each `null` for the seed and the catalogue (and for what a learned record lacks: a features-only one has no `sync`, `encoding`, `crc` or `decoder`). A learned signature is the operator's own description of a signal heard before, not a decode: `classify` and `identify` label a later match `learned <name>` (layer 2, confidence the record's times its score), which is consistent with that signal, never proof of it. A signature's `decoder` is the id of the mode (`modes_list`) that proves it, the `kind` to start on the band to decode it; `null` means no mode row proves it yet. `classify` runs only the modes with a quick attempt (APRS, POCSAG, ACARS, RDS, AIS, and frame rows with `identify.quick: frame`) as decode-to-identify. The built-in three have no allocations. With `--knowledge-acma` a fourth provider, `acma`, answers: the register's plan services and assignments overlapping the band, each with licensee, site, distance and bearing from the station, power, role, the decoded emission designator and `reach` (whether a signal from that site can plausibly arrive here). It needs the station (`station set`): without one it gives no distances. An unreachable service gives no allocations, so an empty `allocations` list means no provider data, not nothing allocated.

### origin_lookup

The licensed transmitters a signal may come from, scored against the band and modulation, seen from the source's vantage. Give `job`, or `source`, `center_hz` and `bandwidth_hz`, never both. By job: the band is the job's (a `bits` job's is its capture's, the capture's source the vantage), `modulation` `null` is read from the job (a decoder row's labels, a demodulator's kind, a `bits` chain's demodulator); the answer is published as one `origin` event on the job's event stream and kept as `job_context.origins`. By band: returned only, `modulation` `null` is unknown. A `mix`, `spectrum` or `scan` job has no band.

- job: string or null; an id from `job list`
- source: string or null; an id from `source list`
- center_hz: integer or null (Hz)
- bandwidth_hz: integer or null (Hz)
- modulation: array or null

JSON: `{"op":"origin_lookup","job":"<id>","source":null,"center_hz":null,"bandwidth_hz":null,"modulation":null}`

CLI: `zeta origin lookup --job ID | --source S --center-hz N --bandwidth-hz N [--modulation fm,fsk]`

Which licensed transmitters a signal may come from. Two forms, never both: `job` alone, or `source`, `center_hz` and `bandwidth_hz` together (anything else is refused `origin_lookup: give job, or source, center_hz and bandwidth_hz`). By job: the band is the job's (an attached job's is its input's; a `bits` job's is its capture's, from the capture's source), and `modulation: null` is read from the job: a decoder row's `modulation` labels (`modes_list`), a demodulator's kind (`fm_demod` reads `fm`, `am_demod` `am`, `ssb_demod` `ssb`, `wfm_demod` `wfm`), a `bits` chain's demodulator (`fsk`, `psk`, `ook`), else none. The answer is also published as one `origin` event on the job's event stream (so on its source's feed and in `events.log`) and kept as `job_context.origins` (the last answer only); a `bits` job's `origin` event stays on the job's own stream, not on a radio's feed or in `events.log`. A `mix`, `spectrum` or `scan` job has no band and is refused naming the kind. By band: a band of no width is refused (`origin_lookup: a band of no width`), the answer is returned only, nothing is published or kept, and `modulation: null` reads as unknown; the op never measures, so run `classify` on the band to score against a measurement. The answer: `origins`, at most 50 lit sites best first, each `{allocation, score, agree, terms, confirmed, heard}`: `allocation` the register row as `knowledge_lookup` gives it; `score` 0..1, the sum of `terms` (`width` 0.4 when the row's width is 0.5-1.5 times the band's, 0.2 within 0.25-4, 0.1 for a row with no width, each scaled by how far inside the band the row's centre sits: in full on the band's centre, falling to 0 for a row that only overlaps the band's edge, so the on-channel site scores above its neighbours; `modulation` 0.4 when the designator fits a label, 0 when it disagrees, 0.2 unknown; `reach` 0.2 at the station falling linearly to 0.1 at the reach limit, so the nearer of two otherwise equal sites scores higher, 0 beyond, 0.1 with no distance), 1 once `confirmed` by a decoded identity; `agree` `fits`, `disagrees` or `unknown` (no designator, or no label). `from` is where the query was made from: `{lat, lon, kind: station}` for a local radio or a recording, and for a remote source (a KiwiSDR) with a `position` (`source_list`) `{lat, lon, kind: source, source}`: the rows are cut to the reach around that receiver and the distances and bearings are from it, not the station. `from` is `null` with `from_note` saying why (`no station set`, or `remote source without a position`: no reach cut, the rows nearest in frequency across the region). `providers` names the providers asked, `register_rows` the rows the register returned (`null` with no register attached), `receivers` and `plans` the rows seen and not scored. The `origin` event carries the answer's fields with `job` and a `summary`: `<n> sites on <MHz> MHz: <licensee>, <site> <d> km <agree> <score>; ...`, or `no licensed transmitter on <MHz> MHz`, `register gave no rows`, `no register provider`. A job whose mode row declares `subject.identity` (`aprs_decode`: `src` against the `callsign`, the SSID stripped) confirms a lit site itself: when one of its events carries a callsign equal to a kept origin's (trimmed, case ignored), that origin gets `confirmed` `{field, value}` and score 1, `job_context.origins` is updated and the `origin` event is published again with `summary` `confirmed <value>: <licensee>, <site>`; a callsign matching no origin changes nothing (many stations share a channel), and only origins already kept are matched, so look the job up first. `heard` is `null` or `{last_ns, grade}` and is set only for a job whose mode row declares `subject.station` (`vdl2_decode`: a ground station's own frames, named by `icao`, and any other frame's `dst` names the station it is sent to; `acars_decode`: an uplink is a station's own frame, anonymous `*`). `grade` is `from` when any station's own frame was heard on the channel, else `to` (an aircraft heard here is talking to a station of that address). A heard site means the register's transmitter is on air on this channel, or an aircraft heard here is talking to a station of that address; the site is consistent; which co-sited transmitter it is stays unknown; it is still not `confirmed`, and the score is never changed by it. `from` is stronger evidence than `to`: the station itself was received, not only an aircraft addressing it. The answer and the `origin` event also carry `stations`, the job's tally `{key: {from, to, first_ns, last_ns}}` (frames the station sent, frames addressed to it; `last_ns` is stream time, the wall time on a live radio), and the `summary` then reads like `2 ground stations on <MHz> MHz: <key> heard <n> frames, <key> addressed by <n> frames; <sites>`; the anonymous station `*` (a row whose uplinks carry no station identity, ACARS) reads `uplinks heard <n> frames`, and a count of one reads `1 frame`. The tally is flushed at the run's end and seeded at launch, so a restart continues the counts. The same tally is kept as `job_context.stations`, written at once on a new station or a station's first own frame and otherwise about once a minute, whether or not a lookup has run, and a fresh `origin_lookup` with `job` answers `heard` and `stations` at once; the band form has no job and reports neither. A lit site is a hypothesis, not an identification: report it as "consistent with Example Water Authority at Example Hill", never "it is", unless `confirmed`.

### channel_plans

The channel plans the node knows, each with its channels expanded and sorted by frequency.

JSON: `{"op":"channel_plans"}`

CLI: `zeta plans [--plan ID]`

`channel_plans` lists the channel plans the node knows (`uhf_cb_au`: Australian UHF CB, 80 channels, two rasters 12.5 kHz apart; `acars_vhf`: the seven VHF ACARS channels, 130.025-131.825 MHz, 10 kHz each; `ais_vhf`: the two AIS channels, 161.975 and 162.025 MHz, 25 kHz each). Name a channel by its number to the user ("ch20") and start jobs at its `center_hz` with the plan's `bandwidth_hz`.

### modes_list

The decoder modes the node knows, one row each (`modes/*.json` built in, then the operator's `--modes-dir`), sorted by id, narrowed to `family` and `state` when given (`null` for any).

- family: string or null
- state: string or null

JSON: `{"op":"modes_list","family":null,"state":null}`

CLI: `zeta modes list [--family F] [--state S]`

The decoder modes the node knows, one row each: `id` (the `kind` a decoder job starts with), `name`, `family`, `band` (`bandwidth_hz` the decoder expects, well-known `centers_hz`, `min_hz` the narrowest band it accepts or `null`, `range_hz` the span `[lo, hi]` it is found anywhere in or `null`), `modulation` (the layer-1 labels it presents as), `decoder_kind` (`graph`, run in-process, `program`, an external decoder, or `frame`, an in-process frame chain), `file` (`modes/<id>.json` for a built-in row, the path for an operator row), `program`, `checked` (UNIX seconds at which `zeta modes check` last passed the row on this host; `null` if never or for a graph or frame row), `subject` (the event field naming what events are about, its table columns and how the page folds it; `null` for none), and `state` ready|broken|unavailable with `reason` (`null` when ready). A broken row's `file` is the file to fix and its `reason` names the field (`<field>: <error>`); an unavailable one names the program it lacks. `outputs` lists the stream kinds a job of the row emits (`event`, `audio`; a row listing `audio` gets play and Transcribe on the page, and `job_attach` may attach `transcribe` to its job; a graph row's are its terminal stages', a program or frame row's `event`; empty for a broken row). `access` is `'restricted'` on a row whose events carry content a later access control may gate (pager text); such events carry `access: 'restricted'`. `family` (and the `family` filter) is one of `tv`, `broadcast`, `aviation`, `maritime`, `satellite`, `two_way`, `cellular_rail`, `pagers`, `iot`, `weather_science`, `amateur_digital`, `amateur_voice`, `hf_utility`, `general`. On the CLI, `--family` and `--state` narrow the list. `zeta modes check [ID] [--strict]` (local only, not with `--node`) runs each row's test on a node over a synthesised capture and prints `<id>: ok`, `<id>: fail: <reason>` or `<id>: skipped: <reason>` (no test, or the program missing: `--strict` makes that a fail); it exits 0 when no row failed.

Frame rows: a row with `decoder.kind: frame` decodes framed bursts in-process, with no program. Its `decoder` names the demodulator, `demod: {modulation, symbol_rate_baud, order, tone_spacing_hz}` (`modulation` `fsk`, `psk` or `ook`; `order` 2 when left out, 2 or 4 for fsk, 2, 4 or 8 for psk, 2 for ook; `tone_spacing_hz` `null` for the demodulator's own estimate), the `framing` (the `frame_decode` stage's params, under Stage kinds: sync, length, CRC, line code, whitening, bit order) and the `parser` (`opaque`, the default: the frame's bytes as they are). A job of the row runs `channel → resample → <demod> → clock_recover → slicer → frame_decode` on a band no narrower than `band.min_hz` (else `band.bandwidth_hz`) and publishes one event per cut frame (`sync`, `length_bytes`, `bytes` in hex, `crc_ok`, `payload`) under the row's `protocol` and `summary`; `clock_recover`'s `timing` events go on a separate output. With `identify: {proof: field, field: crc_ok, quick: frame}` a CRC-clean frame proves the row: `classify` runs the chain over the span (evidence `frame <n> B, <crc> ok (<ok>/<frames>)`; a framing without a `crc` never proves) and `identify` runs the row as a child job. `quick: frame` is refused on any other row, and a frame row's `quick` is `frame` or `null`. A bad framing leaves the row broken naming `decoder.framing.<field>`. `zeta modes check` checks a frame row over the `framing` synthesiser family (`test: {kind: synth, family: framing, modulation, symbol_rate_baud, deviation_hz, frames, body_bytes, framing, repeat, expect: {crc_ok: true}, within_ms}`).

### themes_list

The themes the page can use, one row each (built-ins, then the operator's `--themes-dir`), sorted by id.

JSON: `{"op":"themes_list"}`

CLI: `zeta themes list`

The themes the page can use, one row each: `id`, `name`, `description`, `color_scheme` (`dark` or `light`; `null` for a broken theme), `builtin`, `replaces_builtin` (an operator theme with a built-in's id), `file` (`themes/<id>` for a built-in, the folder's path for an operator theme) and `state` ready|broken with `reason` (`null` when ready; a broken theme's `reason` names the field, `<field>: <error>`). A theme is a folder holding `theme.json`, an optional `style.css` and the fonts and images it names. On the CLI, `zeta themes list`.

### set_list

The named sets the node knows (`sets/*.json` built in, then the operator's `--sets-dir`), sorted by id. A set names the jobs of a daily setup by radio role (`radios`: each role with a `note` saying what radio it needs), never by source: each `members` entry is a `job_start` or `plan_start` (`op`) of a `kind` on a role, with a one-line `summary`. A set that does not parse or validate is `broken` with its `reason` (`<field>: <error>`, e.g. `members[0].params.hold_ms: ...`) and never stops the node; an operator set with a built-in's `id` replaces it, and a broken one that meant to is listed as `<id>@<file name>`, the built-in staying in force. A member's kind need not be a ready row here (a set may name a decoder this host lacks). `running` is the set's running instance, `null` when it does not run: `radios` (the source each role runs on), `since_ns`, and one `members` entry per job it started (`id` the member, `job`, `state`: the job's state, `ended` once it no longer runs). An instance none of whose jobs runs reads `null`.

JSON: `{"op":"set_list"}`

CLI: `zeta sets`

Read it before naming a set: set ids, their roles (`radios`, each with a `note` saying what radio it needs) and members come from here, and it only reads. A set that does not load is `broken` and names the field: report it, never work round it. `running` says whether a set runs, on which sources, and each of its jobs' state (`ended` for one stopped or failed since).

### set_start

Start a named set from `set_list`: each member on the source its role is mapped to, as `plan_start` does for one source, under one lease decision per source. In this order: (1) everything is checked first (an unknown `set` or member id is `unknown`; a broken set, a role a chosen member uses left out of `radios` (`radios.<role>: unmapped`), or two roles on one source whose members that source cannot capture at once are `other`; an unknown source is `unknown` on `radios.<role>`; a set that already runs on another mapping is `other`: `set_stop` it first); a role mapped to `null` gets, roles in the file's order, the first source in `source_list` order that `source_free` lists for the band bounding the role's members and no other role of the request names (while the set runs, the source it runs that role on), none being a `conflict` (`source` the first source covering the band, `holders` every covering source's holders, `options` first `radios.<role>: no free source covers <center> Hz ± <half> Hz; busy: <id> (holders [...]), ...`, then `unpin source <id> (source_unpin)` for each pinned one), or `other` with that text when no source covers the band at all; a member whose kind is a mode row that is not ready (`refused` `other`, `<state>: <reason>`), or a kind this node does not know (`refused` `unknown` on `members[<i>].kind`), or a `watch` member whose `params.modes` names a row this node cannot run or one wider than its band (`refused` `other`, `members[<i>].params.modes[<j>]: ...`), is set aside and the rest go on (a role left with no member is neither tuned nor in the result); a `plan_start` member whose `attach` stage is not ready starts without it and a warning says so (`members[<i>]: <attach>: <reason>; started without attach`); (2) the band bounding each source's members is checked against the source's ranges (`out_of_range` on `radios.<role>`); (3) the gates of every source in use are taken in source-id order and held to the end; (4) a member (a `plan_start` member's channel) whose identical rigid job already runs is `existing`, a `watch` member only when that watch's `detect` params are the member's `params` (a key either leaves out compared at its default), a watch on its band with another policy being `other` (`watch <id> holds this band with other policy; stop it or graph_put it`) with nothing started; a `watch` member starts with its `params` on its `detect` node, with no restart; (5) the `replace` jobs, each holding one of the sources in use, are stopped; (6) every source is decided before any is tuned: a `conflict` on any source is the whole answer, naming the role and the source (`radios.<role> (<source>) is held by [...]`), with nothing tuned or started anywhere; then each source is leased (and tuned when needed) for its bounding band; (7) the members start source by source in the set's role order, each inside its capture, a member's own failure being its `refused` entry; (8) the instance is recorded (`set_list.running`), and `attach` follows each started channel. The result: `radios` (each role's `source` and `capture`), `started` (`member`, `channel` for a plan member, `center_hz`, `job`, `attached`, `attach_error`), `existing` (`member`, `channel`, `center_hz`, `job`, `attached`), `refused` (`member`, `channel`, `error`) and `warnings`. Running the same request twice starts only what is missing.

- set: string; an id from `set_list`
- radios: object; each role of the set (from `set_list`) to an id from `source list`
- members: array or null; member ids of the set (strings), `null` for every member
- replace: array; ids from `job list` (strings), `[]` for none

JSON: `{"op":"set_start","set":"airband","radios":{"second":"<id>","wide":"<id>"},"members":null,"replace":[]}`

CLI: `zeta set start --set airband [--radio wide=<id>,second=<id>] [--members acars,adsb] [--replace ID,ID] (no --radio: every role null, the node picks)`

One call starts a whole set; never start its members by hand. Map every role the chosen members use to a source from `source_list` that can capture the role's band (the role's `note` says what it needs), or to `null` to let the node pick the first free one, and report the picks from the result's `radios`. A watch member on a band another watch holds with another policy is `other` naming that watch: report it, never stop or `graph_put` that watch unasked. A `conflict` names the role and the source with the holders, and nothing started anywhere (a `null` role the node could not fill too, `options[0]` `radios.<role>: no free source covers ...`): report it; the operator decides, and `members` narrows the next request to what is free. `replace`: as on `job_start`, never a job the request did not name. A `null` role keeps the source it picked while the set runs. Read the four lists: `started`, `existing` (already running, skipped), `refused` (a member this node cannot run, `unavailable` or `unknown`, such as a decoder whose program is missing, or a start that failed; the others went on) and `warnings` (repeat them). A set's watch member decodes every channel of a span: its `job_context.watch` (`cap`, `known`, `cooled`) and `decoding` events say how, see "Decoding every channel of a band" in the field craft. Run it again to complete a partial set: running members are `existing`.

### set_stop

Stop the jobs of a set's running instance that still run, those on a radio first and then those attached to them, and drop the instance. Jobs the instance did not start are left alone. The result lists `stopped`, `ended` (jobs that no longer ran when asked, such as one stopped by hand) and `failed` (`job`, `error`; the others go on). When a stop fails the instance is kept, so a second `set_stop` can finish it. A set that does not run stops nothing.

- set: string; an id from `set_list`

JSON: `{"op":"set_stop","set":"airband"}`

CLI: `zeta set stop --set airband`

Stops only the jobs the set's instance started; jobs started by hand on the same radios keep running. Stopping a set is the operator's decision: use it only when the request names the set, or you started it whole in this run (a `set_start` that answered `existing` members leaves `set_stop` to the operator). Through `ask` the run must allow stops, else the call is refused as `not_allowed`: report that, do not retry. Report how many stopped, those that had already `ended`, and any `failed`; a failed stop keeps the instance, so a second `set_stop` finishes it.

### modes_show

One mode's row as loaded, with its `state` and `reason`.

- id: string; an id from `modes list`

JSON: `{"op":"modes_show","id":"<id>"}`

CLI: `zeta modes show <id>`

One mode's full row as loaded (`row`: band, decoder graph or program block (an audio program row adds its `demod` and `input.container`), event schema and `summary` template (`{field:.N}` and `{field:+.N}` format numbers), `identify` rule, catalogue ids, `decoder.args` placeholders (`$port`, `$rate_hz`, `$center_hz`, `$center_khz` the centre in kHz, an integer when it divides, else up to three decimals, `$fifo`), `decoder.output.fields` (each event field <- the JSON pointer(s) into the program's JSON line it is read from; unmapped rows emit the line as is) and `subject.merge` (the event fields whose equal values mean the same subject across rows sharing `subject.name`, so the page draws one table: `[]` for none) and `subject.merge_skip` (field = value pairs naming the subjects that keep a row of their own, a VDL2 ground station: `{}` for none) and `subject.station` (`{when, key, addressed}`: an event matching every `when` pair is a data link's fixed station's own frame, named by its `key` field (`null`: the anonymous `*`); any other event's `addressed` field names the station it is sent to, so it should be a station identity, never a per-aircraft field, since every distinct value becomes a key of the tally; `null` for no stations; `origin_lookup` reads it), `position` (the event fields of a fix, added to each event as `position: {lat, lon, alt_m}`; `alt_unit` `ft` converts a feet field), `derive` (a list of `{when, from, extract, into}`: the event fields `into` filled from the text of field `from` by the named extractor, only on events matching every `when` pair, and skipped whole when any `into` field already holds a value, so a decoder's structured fix always wins; `acars_position` is the one extractor, giving `lat`, `lon`, `alt_ft`; `[]` for none), `subject` (`key`, `name`, `waterfall` `summary` or `fold`, `columns` of `field`, `label` and `unit`, `heading`, `label`, `identity`: how a subject matches the licence register, `{field, register, strip}`, the event `field` whose value, after `strip` (`ssid` removes an AX.25 `-N`; `null` for nothing), confirms a kept origin whose `register` field (only `callsign`) equals it; `null` for none) and `time` (the event field whose ISO 8601 UTC time becomes `t_ns`), each `null` for none) with its `state`, `reason`, `file`, `program` and `checked`. An unknown id is an `unknown` error (`field: "id"`) listing the known ids in `known`; take ids from `modes list`, never invent them.

### classify

Run every registered classifier over the selection's band and span of the source ring (no upstream features).

- selection: object (Selection: source, span{start_ns,end_ns: integer (ns since epoch)}, center_hz, bandwidth_hz)

JSON: `{"op":"classify","selection":{"source":"<id>","span":{"start_ns":1760000000000000000,"end_ns":1760000001000000000},"center_hz":144800000,"bandwidth_hz":12500}}`

CLI: `zeta classify --source <id> --start-ns 1760000000000000000 --end-ns 1760000001000000000 --center-hz 144800000 --bandwidth-hz 12500`

Layer 1 labels the measured band (`wfm`, `fm`, `am`, `ssb`, `fsk`, `carrier`, `noise` or `unknown`; an `ssb` label's evidence gives the dial frequency). Layer 2 first sweeps the decoder modes (`modes_list`) as decode-to-identify: each ready or unavailable row is scored against the layer-1 measurement (0.4 for the measured centre within `band.bandwidth_hz` of one of its `band.centers_hz` or inside its `band.range_hz`, or always when it has neither; 0.3 for a measured bandwidth within 0.5-1.5 times its `band.bandwidth_hz`; 0.3 for the layer-1 label in its `modulation`; plus its best catalogue score, below) and the best 5 are considered in score order (ties by `identify.priority`, highest first, then `family`, then `id`); a graph row with no quick attempt (`m17_decode`) is not considered at all, since only `identify` can try it. The result is `{labels, tried, allocations, origins}`; `origins` is `origin_lookup`'s answer for the band, scored on the measured layer-1 label (`[]` when none), asked once per run beside `allocations`. `tried` lists those 5 as `{mode, score, outcome}`, `outcome` one of `proved` (its quick decode passed its CRC/BCH check), `no_frame` (tried, nothing decoded), `skipped_span` (the span is shorter than the row's `identify.min_span_ms`, 1000 ms for the built-in rows), `skipped_quick` (the row has no quick attempt: a program row such as `adsb_decode` is tried only by identify, never by `classify`; use `adsb_decode`), `skipped_band` (the selection is narrower than the row's `band.min_hz`, else its `band.bandwidth_hz`: `rds_decode` needs 150000 Hz, `adsb_decode` 2000000; on a whole block the measured occupied band counts, read as 150000 Hz once it reaches 100000) or `unavailable` (its program is missing). A proof adds a layer-2 label named by the row's `label` (`APRS`, `POCSAG`, `ACARS`, `RDS`, `AIS`) at confidence 1.0, evidence `decode <mode>: ...`; a protocol is labelled only then. A `no_frame` does not prove the protocol absent: the span may have held no frame. Layer 2 also adds up to three signal names from the catalogue (evidence `catalogue <id>: ...`), scored 0.4 for the measured centre lying in one of the signal's frequency ranges (required), 0.3 for a matching bandwidth (required for signals wider than 50 kHz) and 0.3 for a matching layer-1 modulation. Layer 3 is an empty slot. A span older than the source's ring fails `ring` with `detail.cause` `too_old` and `detail.earliest_ns`, the ring's first retained sample (`source_list` gives its `ring.seconds`): re-issue on a span at or after it, never guess.

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

### ais_decode

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

### composite_decode

- inputs: audio
- outputs: video, event
- params.report_s: integer (optional, default 5)
- params.scale: string (Scale: full|half|quarter) (optional, default "full")
- params.standard: string (StandardChoice: auto|pal|ntsc) (optional, default "auto")

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
- params.confirm_ms: integer (optional, default 0)
- params.cooldown_ms: integer (optional, default 30000)
- params.dc_mask_hz: integer (optional, default 25000)
- params.decode_on: string (DecodeOn: capture|free) (optional, default "capture")
- params.edge_mask: number (optional, default 0.05)
- params.energy_db: number (optional, default 30.0)
- params.gone_ms: integer (optional, default 30000)
- params.hold_ms: integer (optional, default 120000)
- params.max_decoders: integer (optional, default 16)
- params.modes: array or null (optional, default null)
- params.on_unknown: string (OnUnknown: report|capture) (optional, default "report")
- params.persist_ms: integer (optional, default 3000)
- params.prefer: array (optional, default [])
- params.seed_known: boolean (optional, default false)
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
- params.average: integer or null (optional, default null); `scan` only, sweeps averaged (1-64)
- params.bin_hz: integer or null (optional, default null); `scan` only, the bin width asked of a source that sweeps for itself
- params.bits: integer or null (optional, default null); `scan` only, bits per sample, one of the source's depths
- params.dwell_ms: integer or null (optional, default null); `scan` only, in ms (1-5000)
- params.edge_mask: number (optional, default 0.0)
- params.method: string or null (optional, default null); `scan` only, `stepped` or `source`; `null` lets the node choose
- params.notch_hz: integer (optional, default 0)
- params.row_frames: integer or null (optional, default null)
- params.rows_per_s: integer (optional, default 20)

### frame_decode

- inputs: event
- outputs: event
- params.sync: object (Word: hex, bits)
- params.length: any
- params.bit_order: string (BitOrder: msb|lsb) (optional, default "msb")
- params.crc: any (optional, default null)
- params.encoding: string (LineCode: nrz|nrzi|manchester|differential) (optional, default "nrz")
- params.max_errors: integer (optional, default 0)
- params.preamble: any (optional, default null)
- params.whitening: any (optional, default "none")

### frame_parse

- inputs: event
- outputs: event
- params.parser: string

### fsk_demod

- inputs: iq
- outputs: audio
- params.symbol_rate_baud: integer
- params.order: integer (optional, default 2)
- params.tone_spacing_hz: integer or null (optional, default null)

### m17_decode

- inputs: iq
- outputs: event, audio

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

### video_fm_demod

- inputs: iq
- outputs: audio
- params.composite_max_hz: integer (optional, default 16000000)
- params.deemphasis: string (optional, default "ccir405")
- params.deviation_hz: integer (optional, default 8000000)
- params.standard: any (optional, default null)

### wfm_demod

- inputs: iq
- outputs: audio
- params.deviation_hz: integer
- params.audio_rate_hz: integer
- params.deemphasis_us: integer

## Mode rows

The built-in decoder rows with notes (`modes_show` has every row in full; an operator row's notes reach `modes_show` only):
- `acars_decode`: A downlink (a digit block id, downlink true) whose text is a position report (labels H1, 80, 16, 3L, QQ) carries lat, lon, sometimes alt_ft, and a position: the aircraft's own report, typically a few minutes old by the time it is relayed. Uplinks are never parsed. An uplink lights the register's transmitter sites on the channel as `heard`; `job_context.stations` counts uplinks under `*`.
- `ais_decode`: AIS on 161975000 Hz (channel A, 87B) and 162025000 Hz (channel B, 88B), 25000 Hz band, one job per channel. A moving class A ship reports every 2-10 s, one at anchor every 3 min, class B every 30 s; names arrive in separate static messages, so a vessel's row fills over a few minutes.
- `aprs_decode`: APRS at 145 175 000 Hz in AU, 144 800 000 Hz in EU, 144 390 000 Hz in the US, 12 500 Hz band (identify tries it only from half a channel below the lowest centre to half a channel above the highest, its band.range_hz); one job hears every station on the channel. Positions in the three common formats (uncompressed, compressed, Mic-E) land on the map; objects and items are named, not placed.
- `dfm_decode`: Graw DFM-06/09/17 radiosondes anywhere in 400.05-406 MHz; start the job on the measured centre. One job decodes one sonde. dfm09mod 1.8.1 (sonde-decoders) takes --json and --ecc (Hamming ECC on the frames, wanted); it has no --crc (an unknown option is read as the input file, error: open), and no --dist/--auto, so the id comes from the frames as decoded.
- `flex_decode`: FLEX pagers at 1600, 3200 or 6400 bps (2- or 4-level FSK, ±4800 Hz), 25000 Hz band; 929-932 MHz in the US, 151.8-153.4 MHz elsewhere per the catalogue. Identify tries it only in 151.837-153.363 MHz (`band.range_hz`, the catalogue's VHF range: a row holds one range), so in 929-932 MHz start it with job_start; start it on the measured centre. One event per page, like POCSAG. It runs multimon-ng on the node's host.
- `hfdl_decode`: HFDL (PSK on a 1440 Hz subcarrier, 300-1800 bit/s) between airliners and ground stations at 2.8-22.1 MHz, decoded by dumphfdl, typically on a KiwiSDR's 12 kHz band. Start the job with center_hz on the listed (dial) frequency: the signal sits 1.44 kHz above it, inside the 6 kHz band. Take the frequencies from a list (dumphfdl's system table, or the operator's own), not a raster; they change with the hour. Each ground station sends a squitter every 32 s, so a squitter (pdu absent, gs_id set) within a minute proves the channel even when no aircraft is heard. An aircraft is keyed on its ICAO address once dumphfdl has heard its logon. src_type is the LPDU's source as dumphfdl prints it, Aircraft or Ground station. Without a structured pos, an Aircraft frame whose ACARS text is a position report (labels H1, 80, 16, 3L, QQ) carries lat, lon and a position (no altitude): the aircraft's own report, typically a few minutes old by the time it is relayed; a ground station's uplinks are never parsed. Neither dumpvdl2 nor dumphfdl is in Debian or Ubuntu: the OpenWebRX+ apt repository packages dumpvdl2, dumphfdl and libacars2, and both build from source in a few minutes (libacars first).
- `m17_decode`: M17 (4FSK, 4800 baud, Codec 2) on any 2 m or 70 cm simplex or repeater channel, 12500 Hz band; start it on the measured centre. Its audio is ordinary job audio: listen to it or attach transcribe. Encrypted streams are reported, not decoded. It needs c2dec (package codec2) on the node's host.
- `m20_decode`: Meteomodem M20 radiosondes anywhere in 400.05-406 MHz; start the job on the measured centre. One job decodes one sonde. m20mod 1.8.1 (sonde-decoders) takes --json only: it has no --ecc or --crc (an unknown option is read as the input file, error: open).
- `rs41_decode`: Vaisala RS41 radiosondes (GFSK, 4800 baud, one frame a second) anywhere in 400.05-406 MHz; the frequency varies by flight, so start the job on the measured centre, not a raster. One job decodes one sonde.
- `vdl2_decode`: VDL Mode 2 (D8PSK, 10.5 kbaud) in 25 kHz channels at 136.725-136.975 MHz, decoded by dumpvdl2; every AVLC frame is an event keyed on its source address, and an ADS-C report riding an ACARS downlink carries the aircraft's position. Without one, an aircraft frame whose ACARS text is a position report (labels H1, 80, 16, 3L, QQ) carries lat, lon, sometimes alt_ft, and a position: the aircraft's own report, typically a few minutes old by the time it is relayed; a ground station's uplinks are never parsed. A ground station gets its own row, whose ACARS columns name the aircraft of each uplink. Beside ACARS on one radio the order matters, because a running capture cannot widen: pin the radio at 133 500 000 Hz with 8 000 000 Hz first, then start the ACARS plan, then the VDL2 job (136 975 000 Hz, 25 000 Hz); started the other way round, VDL2 is a conflict naming the ACARS jobs. Neither dumpvdl2 nor dumphfdl is in Debian or Ubuntu: the OpenWebRX+ apt repository packages dumpvdl2, dumphfdl and libacars2, and both build from source in a few minutes (libacars first). A ground station's frames, and the aircraft frames addressed to it (`dst`), light the register's transmitter sites on the channel as `heard`; `job_context.stations` counts them.
- `video_decode`: Analog FM video (FPV, reverse cameras, analog HDMI senders): luma only; sync negative; a digital link (OFDM, no line structure) never locks and is reported as not yet decodable.

## Field craft

Measured facts first, then method. Verify against the node's `knowledge lookup` before relying on any of this.

### Where to look

- Radiosondes: 400-406 MHz. RS41 is GFSK at 4800 baud, about 10 kHz wide.
- APRS: 145.175 MHz in AU, 144.800 MHz in EU, 144.390 MHz in the US. AFSK at 1200 baud. Identify tries `aprs_decode` only in 144.38375-145.18125 MHz (its `band.range_hz`: the centres ± half its 12.5 kHz channel), not on the rest of 2 m.
- POCSAG: the preamble is 576 bits of alternating 1/0, then the 32-bit sync word 0x7CD215D8. Common rates are 512, 1200 and 2400 baud.
- ADS-B: 1090 MHz, PPM at 1 Mbaud, 112-bit extended squitter.
- AIS: 161.975 MHz (channel A, 87B) and 162.025 MHz (channel B, 88B), worldwide. GMSK at 9600 baud, 25 kHz channels; HDLC frames with a CRC-16.
- Digital video links (WHDI, WiFi HDMI kits, DJI): a 20-40 MHz OFDM carrier at 5 GHz with no line structure; `video_decode` never locks on it. Report it as not yet decodable; do not try another demod. That is a different answer from `insufficient bandwidth`, which names the source's cap.
- MeshCore: LoRa around 915 MHz in AU and 869 MHz in EU, 250 kHz wide. Not decodable without the network key; classify and report, do not try to decode.
- Licensed bands: with the ACMA register attached (`--knowledge-acma`) and a station set (`station_set`), call `knowledge_lookup` on the band first, before demodulating anything. Report the nearest `tx` assignment's `licensee`, `site`, `distance_km`, `power_w` and decoded emission, and the answer's `as_of` (the register is as fresh as its last sync); say so when `as_of` is `null` or old. An assignment whose `reach` says the site cannot plausibly arrive here is context, not an explanation. A register match is a hypothesis about who transmits, never a proof of what a signal is: only a CRC-clean decode proves a protocol. For a running job, ask `origin_lookup` with its `job` before guessing who transmits: it scores the register's rows on the job's band (width, the modulation the job implies, reach), publishes one `origin` event and keeps `job_context.origins`, and the page lights the sites on the map. Report a lit site as "the register's Example Water Authority at Example Hill is consistent with this signal", never "it is", and quote its `agree` (`fits`, `disagrees`, `unknown`). A site with `heard` (a VDL2 or ACARS job: a station's own frame, grade `from`, or a frame addressed to a station, grade `to`; `job_context.stations` counts them) is consistent with the channel's decode and still not identified: say "consistent with, and heard", and treat `from` as the stronger. A site `confirmed` by a decoded identity (an APRS packet whose source callsign equals the register's callsign) is still a register match, so say "consistent with, and the packet's callsign matches", not "it is". The register may lack callsigns: the acma provider then reads them as null and nothing is ever confirmed, so an unconfirmed site is the normal case. An `origin` summary of `no licensed transmitter on <MHz> MHz` is a finding (the register lists none there); `register gave no rows` and `no register provider` are not (the register was empty or absent). The band form (`source`, `center_hz`, `bandwidth_hz`) answers without publishing or keeping anything, and with no measurement it scores the modulation as unknown: run `classify` first when the label matters. `origin_lookup` refuses both forms at once or neither, a job kind with no band (`mix`, `spectrum`, `scan`), a source the node does not have in the band form, and a band of no width. A `bits` job's `origin` event stays on the job's own stream, not on a radio's feed or in `events.log`: read the op's answer, or `job_context.origins`, rather than waiting on a feed. A remote source such as a KiwiSDR is queried from its own position (reported by the Kiwi, else `--source-position`), so the distances and the reach cut count from the receiver, not the station; with neither, `from_note` says `remote source without a position` and the rows are not cut by reach.

### Decoding APRS and reading measurements

To decode APRS from a source, start the job and tail its events in one call: `zeta job start --kind aprs_decode --source S --center-hz 144800000 --bandwidth-hz 12500 --then-tail 1,5000`. It prints `[job_result, frames_result, {"stopped": true}]` (the job is stopped after the tail; add `--keep` to leave it running); each frame's `payload` is one decoded packet (`src`, `dst`, `info`, `fcs_ok`). A packet with a position in any of the three common formats carries `lat`, `lon`, `alt_m`, `course_deg`, `speed_kt` and `position`, so the station lands on the map and in the Subjects table; `data_type` says what the packet was. An empty frame list means no packet arrived within the timeout, not that none exist. Exit 2 with `[job_result, job_record]` means the job is not running: read `state.reason` (e.g. a stage that failed on the source's blocks). A `measure` job emits one event per second of IQ: `center_hz` and `bandwidth_hz` are the occupied band (spectrum bins at least 6 dB above the noise floor, the 15th-percentile bin of the band), `snr_db` is the peak above that floor, `burst` is true when the loudest 10 ms is more than 3 times the median 10 ms power, `symbol_rate_baud` is the keying rate when a symbol-rate line stands out and `null` otherwise, and `label.name` is one of `noise` (SNR under 6 dB), `carrier` (steady envelope, no modulation), `fm` (steady envelope with deviation), `fsk` (two tones 500-3000 Hz apart, on RF or in FM audio), `psk`, `ook` or `unknown`. Report `unknown` as unknown, with the measured fields. A `measure` event with `partial: true` covers a window cut short by a gap, lag or retune and spans only its samples; `partial: false` is a full second. `classify` and `measure` analyse only the selection/job band (a `measure` event's `center_hz` is absolute RF, within that band); `spectrum` covers the whole source band. A `spectrum` event is one row of power-averaged spectrum (20 per second of signal time) with `size` bins lowest frequency first, `bin_hz`, `center_hz` and the levels quantised as `q` (0-255 per bin): the dB of bin k is `floor_db + q[k] * step_db`; `floor_db` sits 5 dB under the row's 2nd-percentile bin, so a `q` near 10 is the noise floor and 255 means at least 127.5 dB above `floor_db`. A row may carry `masked: [[lo, hi], ...]`, bin index ranges (`lo` inclusive, `hi` exclusive) the receiver could not see honestly (a scan step's DC notch, its band edges): read them as no data, never as a quiet band; the `q` under them is present only so an older reader sees a whole row. A scan's steps alternate between two grids a quarter step apart, so a step's notch and its edges in one sweep fall a quarter of the way into a row of the next, clear of its mask: judge a frequency from two consecutive sweeps. On a local hardware receiver a `measure` job ignores the bins within 25 kHz of the capture centre (`job context` shows `measure.dc_mask_hz`, 0 when none), unless the box is centred on it (a lone job's capture: then the box is measured as today), so a box that sits beside the DC spike in a wider capture measures the signal.

A `measure` event also carries the burst features, always: `duty` (the share of the window the signal is on), `bursts`, `burst_ms` and `period_ms` (`null` when there is no burst train), and, when a keying line or tone clusters stand out, `order` (the FSK or PSK order: 2, 4, 8) and `tone_spacing_hz`. When the window holds a burst train (at least two on-runs, or one of 3 ms or more, with the signal off between them: the quiet chunks within 6 dB of the in-band noise floor, so clicks over a continuous signal do not count) the band, SNR, label, `symbol_rate_baud` and `order` are measured over the on-time only, not averaged into the silence. So a low `duty` means the features were measured over the bursts: report the burst timing (`duty`, `burst_ms`, `period_ms`) with them and say the measurement is over the bursts, never that the band is quiet or `noise` on the strength of a whole-window average. `symbol_rate_baud` comes, for on-off keying whose envelope sits at two levels, from its run lengths (the chip: the unit every on and off run is a whole multiple of, so pulse-width keying such as a 433 MHz remote reads its chip rate, not its bit rate; fitted over the whole window even when it gated as bursts, in the measured band or, for a weak signal whose measured band is mostly noise, a narrower one, and such a window labels `ook` with no tones), else from a cyclostationary line (the |x|^2 spectrum, or the envelope edges for on-off keying); `symbol_rate_line` says which (`runs`, `cyclo` or `keying`; `null` without a rate). A run-length rate is dropped for the other lines when its off runs are signal rather than the noise (a PSK burst's envelope dips), twice its band holds FSK tones, or the band carries a PSK order. A rate is a hypothesis about the keying rate, and a short burst may give none (`null`).

### Decoding pagers, aircraft and FM data

Each of these is a mode row: `zeta modes list` gives its id (the `kind` to start), the `band` it expects with well-known `centers_hz`, and whether it is `ready`. `zeta modes show ID` prints the whole row. Read the band from the row, not from memory; the built-in rows say:

- POCSAG (`pocsag_decode`): 148-149 MHz in AU (Telstra at 148 637 500 Hz among them), 466 075 000 Hz in much of Europe, 929-932 MHz in the US. 12 500 Hz band.
- FLEX (`flex_decode`): 1600, 3200 or 6400 bps pagers, 929-932 MHz in the US, 151.8-153.4 MHz elsewhere per the catalogue; no built-in centre, start it on the measured one. Identify tries it only in 151.837-153.363 MHz (its one `band.range_hz`); in 929-932 MHz start it with `job_start`. 25 000 Hz band. It needs multimon-ng on the node's host.
- ACARS (`acars_decode`): 131 550 000 Hz (worldwide primary), 131 725 000 and 131 825 000 Hz (Europe), 130 025 000, 130 450 000 and 131 125 000 Hz (North America), 131 450 000 Hz (Asia). 10 000 Hz each. An event whose text carries a position report (ACARS labels H1, 80, 16, 3L and QQ, downlinks only, never an uplink) carries `lat`, `lon`, sometimes `alt_ft`, and `position`: the aircraft's own report, minutes old.
- VDL Mode 2 (`vdl2_decode`): 136 975 000 Hz worldwide (136 725 000, 136 775 000, 136 825 000 and 136 875 000 Hz in Europe), 25 000 Hz band. It needs dumpvdl2 on the node's host (the install hint names it; the OpenWebRX+ apt repository packages it). An event is one AVLC frame keyed on the ICAO address `icao` (`addr_type` tells an aircraft from a ground station): report its `summary`. A ground-station row's ACARS columns (`reg`, `flight`, `label`, `text`) name the aircraft of each uplink, and an ADS-C report gives the frame a `position`; an aircraft frame whose ACARS text carries a position report (labels H1, 80, 16, 3L, QQ) gets `lat`, `lon`, sometimes `alt_ft`, and `position` too, the aircraft's own report, minutes old, never over a decoder's structured fix.
- HFDL (`hfdl_decode`): see "Remote HF receivers (KiwiSDR)"; 6 000 Hz band at a listed dial frequency, 2 800 000-22 100 000 Hz.
- AIS (`ais_decode`): 161 975 000 Hz (channel A) and 162 025 000 Hz (channel B), 25 000 Hz band, one job per channel (identify tries it only there, its `band.range_hz` 161.9625-162.0375 MHz); `plan_start` with `plan: "ais_vhf"` runs both in one 75 000 Hz capture. In-process, nothing to install. An event is one message keyed on the vessel's `mmsi` (`msg_type`, `channel`, `nmea` the `!AIVDM` sentences, and per type the position, `sog_kt`, `cog_deg`, `name`, `callsign`, `destination`); vessels land on the map and in the Subjects table. A moving class A ship reports every 2-10 s, one at anchor every 3 min, class B every 30 s; names arrive in separate static messages, so a vessel's row fills over a few minutes.
- RDS (`rds_decode`): any FM broadcast station, 200 000 Hz band. The first event takes a few seconds.
- ADS-B (`adsb_decode`): 1 090 000 000 Hz with a 2 400 000 Hz band; under 2 000 000 Hz is refused. It needs dump1090 on the node's host: when it is missing, the startup handler report names it with its package and `modes_list` shows `adsb_decode` `unavailable` with the reason.

"Set up a background decoder for all ACARS and ADS-B activity" is the named set `airband` (`sets/airband.json`; see "Starting a named set"): `set_list`, then `set_start` with `set: "airband"` and `radios` mapping `wide` (a radio that captures 130-137 MHz at once) and `second` (1090 MHz, 2.4 MHz) to sources. It starts one `acars_decode` on each of the seven channels, `vdl2_decode` at 136 975 000 Hz in the same capture, and `adsb_decode` on the second radio. Without dump1090 or dumpvdl2 on the host that member is `refused` and the rest start. By hand, `plan_start` with `plan: "acars_vhf"` and `kind: "acars_decode"` is the ACARS part alone (`channels` narrows it, `[5]` for 131 550 000 Hz), and VDL2 on the same radio needs the pin first: `source_pin` at 133 500 000 Hz / 8 000 000 Hz, then `plan_start`, then `job_start --kind vdl2_decode --center-hz 136975000 --bandwidth-hz 25000` (started the other way round it is a `conflict` naming the ACARS jobs). A radio serves one band at a time, so ADS-B waits for a second radio. With one radio, the named set `airband_vhf` (`sets/airband_vhf.json`) is `airband` without its ADS-B member: one role, `wide`, the seven ACARS channels and VDL2 in one `set_start` with `members: null`. `airband_vhf_and_sos` adds the 121.5 MHz distress (guard) channel as an `am_demod` to listen to (voice, not a data link: nothing from it reaches the map) in a 16 MHz capture instead of 8. It is the set for "aircraft telemetry on VHF", and the one to start when `airband`'s `second` role has no radio (`set_start airband` is then a `conflict` on `radios.second`, or `other` when both roles name one source).

When rows declare `subject.merge` (`acars_decode` and `vdl2_decode` both do), the page shows one aircraft table across the links, with a `Via` column listing the protocols heard. `merge_skip` names the subjects a row keeps apart (a VDL2 ground station, whose `reg` is the last uplink's aircraft). The seven `acars_vhf` jobs share that one table too: an aircraft heard on two ACARS channels is one row, and the heading lists the jobs.

`knowledge_lookup` says which decoder proves a band's signature (`decoder` names the mode id, the job kind; `null` when no mode proves it yet). `classify` tries the best-scoring modes with a quick attempt (APRS, POCSAG, ACARS, RDS, AIS, and frame rows) as decode-to-identify and lists each under `tried` with its outcome; it never runs a program row such as ADS-B (`skipped_quick`: use `adsb_decode`). A CRC-clean decode is the strongest layer 2 evidence there is. A signature match without one is a guess: say so.

### Decoding modes

A decoder is a row, not code. `zeta modes list [--family F] [--state S]` shows every row with its `state`:

- `ready`: the node can start it. `job_start --kind <id>` takes the row's id.
- `unavailable`: the row is fine but its program is not on this host. `reason` names the program.
- `broken`: the row did not parse or validate. `file` is the file to fix and `reason` is `<field>: <error>`. A broken row never stops the node: it is reported at startup as `zeta: mode <id> (<file>): <field>: <error>` and starts nothing.

Never invent an id: `job_start` with a kind that is neither a built-in nor a row's id fails with an `unknown` error (`field: "kind"`, `known` the built-in kinds and every row id), and a row that is not ready fails `mode <id> is <state>: <reason>`.

**Handlers.** A program row (`adsb_decode` running dump1090, `flex_decode` running multimon-ng, and others) needs an external program. At startup the node prints one line per program: `zeta: handlers: <program>: found <path> (<version>)`, or `zeta: handlers: <program>: missing (apt install <pkg>; <url>) — modes: <ids>`. A missing handler is not an error and is never fetched or built by zeta. To install it, run the package command from the line (`apt install dump1090-fa` for dump1090), then restart the node: the row turns `ready`. A program outside `PATH` is named with `ZETA_HANDLER_<PROGRAM>` (`ZETA_HANDLER_DUMP1090`; `ZETA_DUMP1090` still works). Programs run with a cleared environment, as plugins do. A graph row may list `handlers`, programs a stage of it needs on the host; a missing one makes the row `unavailable` like a program row's program.

**Audio-input rows.** A program row can take demodulated audio instead of IQ. Its `decoder.demod` (`{kind: nbfm, deviation_hz, audio_rate_hz}`) names the demodulator and `input.container` (`raw` or `wav`) how the audio reaches the program; the node expands it to `channel → nbfm_demod (squelch off) → external_decode`. The sonde rows are the example: `rs41_decode`, `dfm_decode` and `m20_decode` run the rs1729 decoders (`rs41mod`, `dfm09mod`, `m20mod`; package `sonde-decoders`, <https://github.com/rs1729/RS>) over a 12 500 Hz channel anywhere in 400.05-406 MHz (`band.range_hz`, no `centers_hz`: a sonde's frequency varies by flight, so start the job on the measured centre). Their rows map the decoder's fields to `position` (`lat`, `lon`, `alt_m`) and name the GPS-time field in `time`, so events carry `position` and `t_ns`, and `summary` reads `{id} {lat?:.4},{lon?:.4} {alt?:.0} m {vel_v?:+.1} m/s` (a `?` field the decoder left out is dropped with its separator). One sonde per job. `zeta modes check --strict` passes `rs41_decode` and `m20_decode` against the real `rs41mod` and `m20mod` on a host that has them; `dfm_decode` has no synthesised test yet, so it prints `skipped: no test` (and `--strict` fails a row without one).

**Checking a row.** `zeta modes check [ID] [--strict]` runs each row's test on a local node over a synthesised capture (APRS, POCSAG, RDS, ACARS, mode S identity, NBFM tone, RS41 (`rs41`: `serial`, `lat`, `lon`, `alt_m`, `vel_v`, `frames`), Graw DFM (`dfm`: `serial` as `DFM-<n>`, `lat`, `lon`, `alt_m`, `vel_v`, `frames`; `dfm_decode` has no row test yet, since `dfm09mod` never reads the serial back), Meteomodem M20 (`m20`: `serial` as `M20-YMM-S-NNNNN`, `lat`, `lon`, `alt_m`, `vel_v`, `frames`), AIS (`ais`: `mmsi`, `lat`, `lon`, `sog_kt`, `cog_deg`, optional `name`, `repeat`, optional `gap_ms`), FLEX (`flex`: `capcode`, `text`, `baud` (1600 only), `repeat`), M17 (`m17`: `src`, `dst` (callsigns of at most nine base-40 characters, `ALL` for broadcast), `can` 0-15, `frames` 1-250 of 40 ms) and the generic `fsk`, `psk` and `ook` families, whose `bits` are hex or `pattern:<hex>`; impairments `snr_db`, `offset_hz`, `ppm` and `echo` `{delay_us, gain_db}`) and prints `<id>: ok`, `<id>: fail: <reason>` or `<id>: skipped: <reason>`. Skipped means no test, or the program is missing; `--strict` makes that a fail. It exits 0 when no row failed. A pass is recorded (`checked` in `modes list`) for program rows and graph rows with `handlers`. It is local only: it is refused with `--node` and while `zeta serve` owns the state directory. Run it after writing a row and before trusting it.

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

**What the events look like.** Every decoded event carries `protocol`, `t_ns` and `summary`, the row's one-line rendering (`{address}: {text?}` for POCSAG). Report `summary`; the other fields are the decoder's own. An event carrying `access: "restricted"` is content a later access control may gate (pager text): treat it as you would a private message, and report what the operator asks for, not every word.

**Subjects.** A row may name what its events are about: `subject` is `{key, name, waterfall, columns, heading, label, track_points, track_min_m}`. `key` is the event field naming the subject (`icao` for an aircraft, `id` for a sonde, `src` for an APRS station), so the page keeps one table row per key rather than one ribbon per event; `name` is the plural noun ("aircraft"); `waterfall` is `summary` (one line for the job) or `fold` (one line per subject); `columns` are the event fields shown, each with a `label` and a `unit` (`ft`, `m`, `kt`, `km_h`, `m_s`, `deg` or `null`) that the page shows as the operator reads it; `heading` and `label` name the event fields holding the heading in degrees and the name it is called by, or `null`; `track_points` (100 when left out, at least 4) and `track_min_m` (0 when left out, not negative) shape its track on the map: a fix joins when it is at least `track_min_m` metres from the last kept point, altitude counted where both have one, or when `heading` has turned more than 10 degrees from that point's, the marker and the line's end always at the latest fix, and at `track_points` the older half loses every other point, its first kept. A bad one is `broken` naming `subject.track_points` or `subject.track_min_m`. Every named field must be in the event schema, else the row is `broken` naming it (`subject.key`, `subject.columns[<i>].field`, `subject.heading`, `subject.label`), and a word outside `summary|fold`, the unit words or `m|ft` is `broken` naming `subject.waterfall`, `subject.columns[<i>].unit` or `position.alt_unit`. A mode that reports altitude in feet says so in its `position` with `alt_unit: "ft"` (`m` when left out): the event keeps its field, and `position.alt_m` is converted. A row without `subject` keeps one ribbon per event; an operator row may add one, as it may add a `position`.

**Quick answer or thorough.** `classify` is the quick answer: it scores every row against the layer-1 measurement and tries the best 5 with a short decode, listing each under `tried` with `proved`, `no_frame`, `skipped_band`, `skipped_span`, `skipped_quick` or `unavailable`. It never runs a program row, and it leaves out `m17_decode` (no quick decode), which only `identify` tries. A `no_frame` means nothing decoded in that span, not that the protocol is absent. When `classify` leaves the answer open, or the signal may be ADS-B, use `identify`: `job_start --kind identify` on the box, or one-shot `zeta job start --kind identify ... --then-tail 3,10000`. The tail is capped at 10 000 ms (a larger timeout is `out_of_range`), so a one-shot `--then-tail` only sees a verdict that lands inside that window, while the dwells run in sequence (5 s for APRS, POCSAG and the sondes, 10 s for RDS and ACARS, 15 s for ADS-B). For anything longer, start `identify` against `zeta serve` (`--node`) and read `job_context` (`identify.verdict`), or `stream tail` the `candidate` events. It holds the radio, measures 1 s, then runs each fitting ready row as a child job in score order for the row's dwell. Read its `candidate` events in order: each has `mode`, `score`, `outcome`, `evidence`, `summary` and the child's `job`. A `proved` candidate ends the job with the verdict in `job_context.identify` and leaves that decoder running under `verdict.job`; stop it with `job_stop` when done (`--then-tail` already stops it unless `--keep`). An exhausted list ends with `verdict.mode: null` and the layer-1 features: report that as unknown, with those features. Do not guess a protocol from a `no_frame` list.

### Frame rows and signatures

A frame row (`decoder.kind: frame`) decodes a framed burst in-process. Its `decoder` has `demod` (`{modulation, symbol_rate_baud, order, tone_spacing_hz}`), `framing` (the `frame_decode` params: `sync`, `length`, `crc`, `encoding`, `whitening`, `bit_order`) and `parser`. `parser` is `opaque` by default: the frame's bytes in hex, with `crc_ok`. Each cut frame is one event.

`zeta modes check` tests a frame row over the `framing` family: it synthesises a framed capture from the row's own demod and framing and expects a CRC-clean frame back.

A learned signature is such a row, written by the operator's accept (`learned_` and the id lowercased, in the node's modes directory; the record's `decoder` field names it). A node with no modes directory writes no row and says so. An existing row is kept as it is. A built-in or operator row that proves an overlapping signal supersedes the learned one.

Parsers read a frame's bytes into fields, each behind the protocol's own check, and publish an event only when the check passes. Four exist, named by `decoder.parser` of an operator frame row: `dnp3` (every CRC-16/DNP), `iec101` (the repeated length, the checksum and the end byte; the variable-length frame only), `iec104` (the APCI and the ASDU header) and `modbus_rtu` (CRC-16/MODBUS). No built-in rows ship for them: a row with no well-known centre would be tried on every band, and `iec104` has no check strong enough to keep noise from proving it. The operator names the band, the centre and the framing in use. Modbus RTU frames carry no sync word of their own (they are UART characters delimited by silence), so its row names the modem framing. For `dnp3`, `iec101` and `iec104` the frame starts with the framing's sync word (`05 64`, `68`), and the parser is handed the sync and the frame's bytes together.

The boundary the parsers keep is the spec's:

This decodes clear, unencrypted, standards-based framing that the node is lawfully receiving. It never attempts to break encryption, never targets a specific operator's link as the deliverable, and emits no transmit path. An encrypted or proprietary payload is reported as `framed, opaque` with its length and CRC status, not decoded.

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

The page shows a watch as an outline on the waterfall with its signals marked ("Watch this band" in the box menu starts one; a `signal` is a mark, not a ribbon). Its owned identify jobs are hidden. A watch's `unknown`, `decoding` and `lost` events and an `identify` job's `verdict` are also kept as transcript lines (`<kind> <id tail>: <summary>`, the register's nearest transmitter in an unknown's), and What is this (on a box or a capture) ends with `register says <EMISSION> <licensee>, <site>, <d> km, <p> W` when the register has a transmitter there. The map is a tab beside the radio tabs, in the waterfall's place while it is selected (`view set main map|rf`; `view set map on|off` is the alias). Events with a `position` appear on it: a marker and track per subject for a row that names one (thinned by the row's `track_points` and `track_min_m`, the marker turned to its heading and labelled with its name and altitude), per job otherwise, whichever source it runs on (the decoder on the second radio included), and with `view set home LAT,LON` (`none` clears it; while it is `none` the page uses the station's position, read once at page load, when `station_set` gave one) the jobs pane shows distance and bearing from home for a job with one marker. A row with `subject` columns also shows on the Subjects tab of the pane strip beside the radio tabs (one pane at a time under the waterfall, chosen by its tab or `view set pane subjects|none`; `view set subjects on|off` is the same choice, `auto` only shows the tab while such a job runs), one table row per subject in metres and km/h, climb rate in m/s; a subject silent for `subject_dim_s` (60) dims and for `subject_drop_s` (300) goes, and a click on a table row selects its marker and the reverse. This state lives on the page, so a refresh refills from live traffic. The map uses vendored Leaflet and fetches its tiles only when the browser is online; `map_tiles` picks the provider (`auto` is the theme's choice, else OpenStreetMap darkened by a filter under a dark theme and plain OpenStreetMap under a light one). The Origins button on a running job's row, in the Bits header and in the box menu runs `origin_lookup` for that job or band and lights the sites on the map: one circle per register site, its brightness the site's score times its freshness (the later of the signal's last activity and the lookup, fading to nothing over `origin_fade_s`, 300 s), a line from where the query was made (the station, or the Kiwi marked with its id) to each site that fits or is unknown, and a dashed ring with no fill for a site whose emission disagrees with the measured modulation. A confirmed site has its own ring. A lit site is a hypothesis; read its tooltip (licensee, site, emission, score and agree, distance and bearing) before saying anything about it, and say "consistent with". The selected pane docks under the main view or, with `view set dock right`, as a third folding section of the side column (`auto` follows the theme's `layout.dock`). The box menu's "Capture bursts" starts a `capture` job on the box (fitted to the signal first, as a demodulator is), and the Explore tab of the pane strip (`view set pane explore`, shown once a capture exists and selected the first time one appears) lists the captures newest first with What is this (`capture_classify`), Download (both SigMF files) and Delete (a second click within 3 s), and a Delete all above the rows that removes every capture not pinned (also a second click within 3 s). The Jobs pane's Clear all (a second click within 3 s) is the typed `stop all`, one `jobs_stop` that keeps the page's spectrum, and then drops the finished rows.

SondeHub is a feed out of the node, so nothing uploads unless the operator gave a callsign. Put `sondehub: {callsign}` (with `lat`, `lon`, `alt_m` only to override the station) on the watch's `detect` node (`batch_ms` and `url` are optional) and every decoder it keeps gains a `sink_sondehub` node (with no station set and no position given, the decoder is kept without the sink: its `decoding` entry carries `sink_error` and the summary says `no station set`; a hand-placed sink in that state idles, `job_context.sondehub.idle` true with `last_error`, and never fails its job); or `graph put` a `sink_sondehub` node on any sonde job yourself. `job_context.sondehub` counts `sent` and `failed` and keeps `last_error`; events SondeHub would reject (no position, no `datetime`, no `frame`, no numeric altitude) are skipped, not counted, so `failed` means transport trouble. `lat`, `lon` and `alt_m` may be left out: the node's station (`station_get`) supplies them, and the sink fails `sink.params.lat: no station set` when there is none. Do not invent a callsign or a position.

### Decoding every channel of a band

A watch can decode every channel that carries a signal across a span, not only identify one signal at a time: the pager carriers of 148-149 MHz, say. It is a watch with `modes` on its `detect` node, the list of mode ids it may start. A watch without `modes` behaves as "Watching a band for sondes" says.

Two ways to start one:

- The set: `set_start` with `set: "pagers_au"` and one radio that captures 148.0-149.0 MHz (see "Starting a named set"). Its `pocsag_decode` watch is stored in `sets/pagers_au.json` with the detector's policy set.
- By hand, two operations: `job_start` with `kind: watch` on the span; `graph_get` the watch; on the `detect` node set `modes` (`["pocsag_decode"]`), `persist_ms` (500 for a pager's bursts), `width_hz` (`[6000, 25000]`) and any of `gone_ms`, `hold_ms`, `cooldown_ms`, `confirm_ms`, `max_decoders`; `graph_put`. The page's "Decode across this band" in the box menu does the same and sets `width_hz` to 0.5x to 2x of the first listed row's channel width. A `modes` entry the node cannot run, a row the watch's band is narrower than or a bad field is refused naming it, before anything starts or restarts.

What the watch does, and what you read of it:

- A signal is matched to the listed rows by width: it fits a row when its measured width is within 0.5x to 2x of the row's channel width (a strong signal reads wider than its channel, which is why the fit is tolerant). A signal read at the detector's `width_hz` floor (within 10 % of it) fits from 0.4x instead: the detector drops narrower runs, so a clean pager that reads narrow shows at the floor, and a 6 kHz floor still reaches a 12.5 kHz row. The nearest ratio starts a decoder directly, on the signal's measured centre, never on a raster or a register centre: the `decoding` event says `via: modes` and starts no `identify` run. Rows tied for nearest run an `identify` restricted to them. A signal no row fits is `unknown` with its measured features and is retried as for any watch.
- A steady spur at a fixed RF reads as a signal the watch keeps starting a decoder on; centre the capture on it so the DC mask hides it (pagers_au sits on the b210's 148.500 MHz spur for this reason).
- `decode_on` stays at `capture` (the default) for a span: the radio does both. `decode_on: free` is honoured and the cap bounds it, but a span's decoders are many and a free source holds few: leave it.
- A watch never retunes, widens or pins the radio, and never stops a job it did not start. A decoder you started by hand inside the band is left alone, and a signal it already decodes is not started again.
- At most `max_decoders` (16) decoders at once, on any source. A signal over the cap is `waiting` (one event per signal); a slot freed by a `lost` decoder goes to the present waiting signal that ranks highest (a known channel first, then score, then SNR). Nothing is pre-empted. `jobs_stop` of the decoders' kind frees the slots.
- A decoder is `lost` with a `why` from a closed set: `gone and silent` (its signal is gone and it sent no event for `hold_ms`, 120 000 ms by default), `not seen again` (a known channel seeded at start that no signal appeared on within `hold_ms`), `unproven` (a direct start that published no event within `confirm_ms` while its signal was present; `confirm_ms` 0 turns this off) and `relaunch failed`. A lost decoder's channel, and that of one you stopped, is cooled for `cooldown_ms` (30 000 ms; `job_context.watch.cooled`): a signal on it is tracked but not started until the cooldown passes. A signal still present after a loss the watch raised, or after its decoder failed (`cooled` `why: "failed"`), counts a try, so after 3 tries a carrier no decoder proves is left alone until it goes and comes back; a decoder you stopped counts none. These cooldown and retry rules apply to span watches only.
- `job_context.watch` holds, beside `signals`, `decoding` and `current`: `cap` (`max`, `decoding`, `waiting`), `known` (`{providers, assignments, channels}`: what the knowledge providers were asked and answered, the licensed transmitters whose emission fits a listed row, and the listed rows' centres in the band; an unreachable register cannot be told from an empty one, so say "the register gave none", not "nothing is licensed") and `cooled` (the channels in cooldown). A known channel ranks first for a slot; `seed_known: true` on `detect` starts a decoder on each at once. A known channel never moves a decoder.
- A node restart or `graph_put` re-adopts the decoders: one whose signal does not reappear within the longer of `gone_ms` and `hold_ms` is stopped.
- `job_stop` on the watch stops its decoders too; that is the operator's call, as for any stop. The page folds the decoders under the watch's row and merges their subjects into one table with a Channel column.
- The pager rows (`pocsag_decode`, `flex_decode`) carry `access: restricted`: see "Legal" in the README and the rule above on decoding pager traffic; the watch does not change it.

### Remote HF receivers (KiwiSDR)

A KiwiSDR source covers 0-30 MHz and delivers 12 kHz of IQ per connection, so `bandwidth_hz` is at most 12 000: work one HF channel at a time with `measure` jobs or `classify` selections inside the band you tuned (e.g. 7 100 000 Hz with 10 000 Hz for 40 m), not wide `spectrum` sweeps. A Kiwi has a limited number of user slots; when they are all taken the source faults with `too_busy` and the job fails with that reason: report it, do not retry in a loop. The connection opens on the first tune and holds a slot until the node exits. Reception is site-dependent: the receiver's location, antenna, local noise and the time of day decide what is audible, so an empty band on one Kiwi says nothing about the band elsewhere.

`source_list` gives each source a `position` `{lat, lon, alt_m, from}`: `from` is `status` when the source reported it (a Kiwi with GPS reads `gps=` and `asl=` from its status page once per connection) or `configured` from `--source-position ID=LAT,LON[,ALT_M]`; `null` otherwise. A signal heard through a Kiwi was received there, not at the station: do not measure its range or bearing from the station.

HFDL (`hfdl_decode`, needs dumphfdl on the node's host) works on a Kiwi unresampled: `center_hz` is the listed (dial) frequency and `bandwidth_hz` 6 000. The signal sits 1.44 kHz above the dial frequency, so a `measure` box on the carrier reads 2.4 kHz of PSK offset up. A ground-station squitter every 32 s proves the channel (an identify dwell of 40 s holds one). Frequencies come from a list (dumphfdl's `etc/systable.conf` upstream, or the operator's own), not a raster. Events are keyed on `icao`, known once the aircraft's logon was heard, with `gs` the ground station, `pdu`, `flight` and a `position` from performance-data PDUs.

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

`stream_tail` the source's feed (`stream_list`: the `event` stream with that source and no `job`) shows every transcript, `vad` and `squelch` event of every job on the radio in one call, each frame naming its job.

Transcription is per demodulator: a `mix` (see "Listening to many channels") is not transcribed unless the operator asks for it.

The node needs a speech backend (`--stt-socket` or `--stt-node`); without one the attached job fails naming it, so report that and do not retry. Language: the attached job transcribes `en` unless it was attached with `language`; a language the backend does not list (see `GET /stt/languages`) fails the job naming the languages it has. Do not guess a language from the band.

Music and noise are gated, not separated: `vad` passes any loud audio, so a music station or a broadcast with a bed under the voice yields poor or empty text. Say so rather than reporting the text as the speech. Separation is a slot: an external audio-to-audio plugin (`--plugin separate=PATH`, a Demucs wrapper for instance) replaces the `denoise` node with `graph get` of the attached job, edit, `graph put`. The job restarts on `graph put`. `docs/speech.md` has the daemon install and the plugin stub.

`queued_ms` on a transcript is how long it waited for the speech daemon (at most 4 calls at once); `dropped` with `reason: busy` means the daemon had no slot for 30 s, `reason: queue` that the job's own queue of 4 was full. Utterances under `min_utterance_ms` (700) are not sent at all; the job's context counts them under `transcribe.short`.

An `fm_demod` shapes its audio as a voice receiver does: a 3.4 kHz low-pass (`audio_lowpass_hz`) and a 6 dB per octave de-emphasis (`deemphasis_us`, 530) after the discriminator take out the hiss above the voice that a weak signal carries; the audio-input decoder rows keep the flat discriminator they decode from (`null` for both). Every voice demodulator (`fm_demod`, `am_demod`, `ssb_demod`) also takes `denoise: true` on its node (`graph get`, edit, `graph put`; the page's outline menu offers "Denoise on/off"): RNNoise runs on its audio output, so the page, a `--then-tail` and any attached transcriber all get the cleaned audio, and a `transcribe` attached to such a job runs without a `denoise` stage of its own. It is a speech model: off by default, and never on a decoder row (a tone or a data burst comes out mangled). It adds one 10 ms frame of delay, which the audio frames' spans account for, so transcripts and ribbons keep their time. These params are carried like the squelch: a `graph put` that changes only them keeps the job re-expandable, and a `job retune` keeps them.

A `fm_demod` or `am_demod` has an automatic squelch: an idle channel publishes no audio, so a quiet transcript is normal. `squelch` events say why a row is quiet (idle, or open). It opens 6 dB over the tracked noise floor (`squelch_db: null`, the default) and closes after 300 ms under it. To fix or disable it, `graph get`, set `squelch_db` (an absolute threshold in dBFS, -120..=0, not dB above the floor) or `squelch: false` on the demodulator's node, `graph put`; such a put keeps the job re-expandable at later capture changes. A `--then-tail` of an idle squelched channel is empty: that is the squelch, not a fault.

### Decoding digital voice

M17 is decoded in the node: `job start --kind m17_decode --source S --center-hz HZ --bandwidth-hz 12500` on a 2 m or 70 cm channel, `HZ` the measured centre (4FSK at 4800 baud). Each stream gives two events: one when its link setup is in hand (at its first frame, or within six frames from the LICH when the job joins mid-stream; `frame` says which) and one at its end (`eot: true`, the last frame number). Both name `src`, `dst`, `can`, `type`, `encryption` and `codec`, and `lat`/`lon` when the stream carries a GNSS position. Its speech is ordinary job audio: the page plays it, and `job attach --kind transcribe --job THAT_ID` transcribes it as it does a demodulator's. An encrypted stream (`encryption` not null) is reported with its fields and never decoded, so it has no audio. The speech comes from `c2dec` (package codec2) on the node's host; without it the row is `unavailable` naming the package. When `c2dec` exits mid-stream, the events keep coming, that stream's audio stops, and `job context` says why under `m17.vocoder`; the next stream starts it again. DMR and D-STAR have no decoder row.

### Reading transcripts

Every transcript event of an attached job is also written to the node's `transcripts.log`. `zeta transcripts tail --lines N --job ID` reads the last lines (default 50, one JSON line each: `job`, `source`, `center_hz`, `bandwidth_hz`, `start_ns`, `end_ns`, `text`, `language`, `dropped`, `error`), so you can answer "what was said on 476.4 MHz" after the fact without a live `stream tail`. It reads the state directory, so it does not work with `--node`. A line with `error` set is a failed utterance, not silence.

### Reading past events

Every event of every job on a source (every decoded frame, detection or transcript the source's feed carried, whether or not a page or a `stream tail` was open) is also written to the node's `events.log`. `zeta events tail --lines N [--job ID] [--source ID] [--protocol NAME]` reads the last lines (default 50, one JSON line each: `t_ns`, `source`, `job`, `channel`, `center_hz`, `bandwidth_hz`, `start_ns`, `end_ns` and the event under `event`), so "what did ACARS hear overnight" is `zeta events tail --protocol acars --lines 500` after the fact, and "which aircraft were up on 131.450" is the same filtered by `--job`. It reads the state directory, so it does not work with `--node`. The log holds the last 32 MiB plus one rotated file; a job's events before that are gone.

### Taking the radio from another job

A source holds one tuning, and a second job that needs a different one gets a `conflict` error. Read it before acting: it names the holders (job ids, kinds and what each is doing) and lists options, the last being `retry with replace: [<ids>]`. Decide whether the holders matter. If they do not, or the user asked for the new job, retry the same request with `--replace ID[,ID]` (`job start`, `source ensure`; `replace` is an array in the op). The named jobs are stopped and the new job started in one step under the source's gate, so nothing else can slip in between. Replace is not transactional: if a holder you did not name still conflicts, the jobs you named stay stopped and the new conflict names the remaining holders. Every id must hold that source as a rigid lease (`unknown_job` or `job <id> does not hold source <src>` otherwise); naming an elastic job is that error and nothing is stopped, the elastic job keeps running. Never replace a holder you have not read, and say in the report what you stopped. Stopping or replacing a job is the operator's explicit decision; hand-made jobs survive any request that did not name them.

The web page's own `spectrum` job is elastic (`job start --elastic`, `spectrum` only): it never conflicts, never blocks a retune and follows each retune of its source, so it is not a holder to wait for or replace. `source list` shows such jobs under `elastic`, not `holders`. A scan on the source stops it, and the page restarts its view afterwards. After a restart of the node the capture is restored (the band bounding a source's rigid jobs, when the radio can capture it) and the jobs resume in it, a scan first, then widest first. `zeta ops tail --lines N` (default 50) shows the last applied ops with the transport that sent them (`local`, `http`, `ui`, `agent`): use it to see who changed the radio.

### Holding a band

To keep many channels running on one radio, pin the capture instead of letting each job retune it: `source pin --source S --center-hz C --bandwidth-hz B` (add `--replace ID` for the jobs in the way). Centre the pin between the channels you want, not on one of them, so no channel sits on the DC spike. Example: UHF CB (80 channels) is held by `--center-hz 476918750 --bandwidth-hz 1000000`; the lease may round up (1 024 000 Hz) and every channel fits. The pin retunes the radio to its band, even one inside a wider capture the page's spectrum holds (the spectrum follows); only when a decoder already holds the source inside the band does the pin join the current capture instead. Jobs started inside the pin run on it without a retune; the pin and the gain (`source gain --source S --db N`, or `--auto`) are saved and come back after a restart, and one-shot commands, read-only ones included, restore the pins and the gain of the sources they attach.

A pin admits jobs inside it: `source_free` lists a pinned source free for a band the pin holds whole (with no job holding it), and a set's `null` role may pick it. A start outside the pin is a `conflict` offering `unpin source S (source_unpin)` (and `stop one of the holders` only when jobs hold the source). A pin is not a holder and is not in `--replace`. Unpinning frees the radio for other work and does not stop the jobs that were inside it; they conflict with the next retune. Read the conflict, and unpin only when the user wants the pinned channels given up.

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

A result of `not_allowed` means the run may not do that: the agent is not allowed to stop jobs, or to put any in `replace`, unless the operator unlocked it for the run. Do not retry and do not work around it with other calls. Tell the operator to say `allow stops on` (`allow deletes on` for a refused `capture_delete`), or press the retry button, then ask again.

### Starting a named set

"Start the airband set" or "decode every pager carrier" is one `set_start`, not its members by hand. A set is a stored recipe: roles (`radios`), never sources, and members that are `job_start`, `plan_start` or a watch.

1. `set_list` first: the ids, each role's `note` (what radio it needs), the members, and `running` (on which sources, and each job's state; `ended` for one stopped or failed since). A set that is `broken` names the field; report it, never work round it. `set_list` only reads.
2. `source_list`: note the holders other than `elastic` on the sources you would use.
3. `set_start` with `set`, `radios` mapping every role the chosen members use to a source that can capture the role's band, or to `null` to have the node pick the first free source in role order (it keeps that source while the set runs; report the picks from the result's `radios`), `members` (`null` for all) and `replace` (`[]` unless the request named jobs).
4. Read the four lists: `started`, `existing` (already running, skipped), `refused` (a member this node cannot run: `unavailable` or `unknown` for a decoder whose program is missing or whose kind it lacks, or a start that failed; the others went on) and `warnings` (repeat them).
5. A `conflict` names the role, the source and the holders, and nothing started anywhere. A `null` role the node could not fill is a `conflict` too: `options[0]` reads `radios.<role>: no free source covers ...; busy: ...`, `source` is the first source covering the band and `holders` every covering source's holders (`other` with the same text when no source covers the band). Report it and stop: the operator decides. `members` narrows the next request to what is free. Never retry with `replace` unnamed or `job_stop` the holders. A watch member on a band another watch holds with a different policy is `other` naming that watch: report it, never stop or `graph_put` it unasked.
6. Run it again to complete a partial set: running members are `existing`.
7. `set_stop` is the operator's: use it only when the request names the set, or you started the whole set in this run. It stops only the jobs the instance started, those on a radio first; jobs started by hand on the same radios keep running. Report `stopped`, `ended` (already stopped by hand or failed) and `failed`; when a stop failed the instance is kept, so a second `set_stop` can finish it. Through `ask` a set started with `existing` members leaves `set_stop` to the operator: a run may stop only a set it started whole.

A set file never names a source: the operator's own sets (`--sets-dir`) are refused at load when they carry `source`, `replace` or `params.prefer`. A row the host lacks does not break a set at load; that member is refused at start.

### Listening to many channels

"Let me hear everything" is one `mix_start`, not a player per channel. `sources: null` mixes every source of the node, a list only those; one mix spans any set of tuners. It takes no lease, tunes nothing and stops nothing, so start it whenever; running it twice on the same set answers the same job.

1. `mix_start`, then play the job's audio or `stream tail` its streams (`stream list` shows an `audio` and an `event` stream with provenance source `node` and the mix's `job`; the `ops` stream is the one with no `job`).
2. Its `event` stream publishes `active` (`job`, `source`, `center_hz` of every channel being heard) whenever that changes; a channel stays active for 500 ms after its last audio. `job_context.mix` holds `inputs` (how many jobs it mixes), `active`, `last` (the last channel heard), `muted`, `dropped_ms` and `underruns` (each per job), and `skipped` (jobs whose audio is not 48 kHz mono, each named once).
3. `dropped_ms` counts audio the mix threw away because a channel's queue ran over 200 ms; `underruns` counts the times a channel ran dry after it had started feeding. Both are per job, not per channel. Either means the mix could not keep time with that channel: report it with the job, do not hide it.
4. A mute is a channel, `{source, center_hz}`, not a job: `graph get` the mix, add it to `muted` on the `mix` node, `graph put` (this restarts the mix under its id). It matches any job on that source centred within 1 kHz, so it outlives the job: a channel muted and restarted stays muted, a fit that moves the centre a little keeps it, and a retune elsewhere is heard again. Mutes live in the mix, so stopping the mix (`stop all` included) forgets them.
5. `wfm_demod` and `ssb_demod` have no squelch: they always produce audio, so a mix holding one is open all the time until that channel is muted. Say so rather than calling the mix broken.

On the page, many idle plan channels fold into one `<n> idle <kind> on <plan>` group (more than 8 idle), but only on evidence the page has: a kind with no squelch, a job on the shown source (all are watched, through its feed) whose squelch is known closed for 60 s, or an unwatched job on a source covered by the mix the page is playing that the mix has not heard for 60 s. Idle plan channels on the shown source therefore fold on their own squelch evidence, with no mix playing; only jobs on other sources need the mix's evidence. An unfolded idle channel is not broken. `view set side both|transcript|jobs|none` picks which side panes show (`jobs` gives the job list the whole column) and `view set theme <id>` the look, the ids being the ready themes of `themes_list` (`console` the default); in `console` job tags sit on a rail left of the frequency axis, in `plain` on the outlines.

View settings live on the page, for the tab that asked (`radio_policy` and `radio_prefer` are the exception, kept in the browser for every tab); they are not node operations. From the page, change one with the `view_set` tool (`{"key": "theme", "value": "terminal"}`; read `themes_list` first for a theme, never invent an id), never with a `graph_put` or any job operation. It takes the presentation keys only: `theme`, `side`, `dock`, `main`, `map`, `pane`, `subjects`, `finished_rows_s`, `subject_dim_s`, `subject_drop_s`, `plan`, `follow`, `origin_fade_s` and `map_tiles` (the map's tiles: `auto`, `osm`, `osm-dark`, `opentopo`, `esri-street` or `esri-imagery`); `zoom_out`, `scanner`, `auto_scan_stop` (they drive the page's scans) and `home` are the operator's to type, and any other key is refused with the list. A `view_set` takes effect only when the run's answer reaches the page: a failed run applies nothing. Without that tool (`zeta ask`, MCP) answer with the command to type, for instance `view set theme terminal`. The default radio is the one the page has focused: its tab carries an accent dot (hover: `default radio for the agent (policy <p>)`), on the RF view and on the Map's kept tab. `view set radio_policy focus|focus-fallback|free-first|ask` chooses what the agent does with it (default `focus`) and `view set radio_prefer none|a,b` sets the preference order for the fallback; both are saved in the browser across tabs and restarts, `view reset` clears them, and the agent cannot set them (they are the operator's to type). Under `focus` use only the default radio: when it cannot take the band, say why, name the free radios and ask which; never start on another unnamed radio (the gate answers `policy`). Under `focus-fallback` use the first free radio in the preference order and say which and why. `free-first` is the first free radio, and `ask` asks before starting. A radio the request names always wins. The box menu's Send to agent names its radio (`... on <source>, from ...`), and after an answer the page notes `ran on <src> (<tool>), not the default radio <radio> (<policy>)` when the run used another radio. Changing a view setting never needs a job stopped or replaced: running jobs do not block it. When the operator says the map is blank or black, its tiles are not loading (the map says so in its corner): set `map_tiles` to another provider, say `osm` or `esri-imagery`.

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

`source list` gives each Soapy source a `stats` record (`null` for a recording, a KiwiSDR, a `--source-node` and a `--source-vrt` source; `worker` shows the child that owns the device (`pid`, `restarts`, `since_ns`, `transport` and `transport_dropped`; `null` when the source runs in the node)): `measured_rate_hz`, the samples per second the device actually delivered (an integer, `null` until two blocks have arrived), and `overflows`, `timeouts` and `restarts`. Compare `measured_rate_hz` with the pinned rate: within 1 % is healthy. `restarts` counts clock-guard stream restarts, each logged as `zeta: soapy <id>: samples ran <n> ms ahead of the clock; stream restarted`; more than 5 in 60 s fault the source with `soapy: samples arrive faster than <rate> Hz`. `overflows` and `timeouts` are zeta's counts of the driver's read results, not the device's own. `bank.dropped` in the same record counts input blocks the shared channel bank dropped under load; a number that climbs means the node cannot keep up, so drop jobs or narrow the pin. `worker.transport.path` is `shm` when the worker hands its blocks over through a shared-memory ring (`ring_bytes` long) and `pipe` when they travel in the pipe itself (a worker of an older build gives `null`); `worker.transport_dropped` counts what the worker could not hand over: on the ring, 2 MiB pieces the node had not yet released; on the pipe, or when the worker's own broadcast lags, whole blocks its pump could not take. Like `bank.dropped`, a number that climbs means the node cannot keep up; it starts again from 0 when the worker is respawned.

`GET /health` carries `stages_running`, the live stage tasks: every graph stage plus one IQ forwarder per job on the radio, none for an attached job, and one loop for a scan whatever its graph. A number that keeps climbing after jobs are stopped is a leak: report it with the job history, do not restart quietly.

A `measure` job's `job context` carries `measure: {discarded, partial, last}`: windows discarded, windows emitted short, and the last cause (`gap`, `lag` or `retune`). A `measure` event with `partial: true` spans only the samples it held (at least 250 ms); read its fields with that in mind.

### A radio that comes back

A Soapy device runs in a `zeta soapy-worker` child that the node spawns, so a driver crash or a pulled cable faults the source and never ends the node. Expect this on a cable pull: within about a second `source list` shows `fault` as `no samples for 1000 ms (device unplugged?)` (the worker's exit 5), every job on the source fails with `source <id>: <fault>`, and `job_start` is refused with `source <id>: <fault>` (`source_ensure` and `source_pin` still answer `io: <fault>`). A driver crash reads `soapy worker for <id> died: signal 11 (SIGSEGV)` (or `exit code N`); a device that is not there reads `device not found: <args>`, and an open that fails `soapy open failed: <last stderr line>`. Nothing needs restarting: the node respawns the worker (1 s doubling to 5 s, reset after 60 s alive; every 5 s while the device is absent), logs `zeta: soapy worker for <id>: <fault>; waiting for the device` once and, when the device is back, `zeta: soapy worker for <id>: back after <n> attempts, <s> s`. Then it restores the gain and the pin and relaunches every job that failed with that fault, under its own id (a scan first, then widest first; an attached `transcribe` after its input; the `watch` re-adopting its decoders), logging `zeta: source <id>: recovered: pin <c>/<bw> Hz, gain <g> dB, <n> jobs relaunched`. A healed fault stays in `source list` until recovery has seen the jobs end, normally up to one poll (200 ms) after the hello; during that hold a `job_start` is accepted, and its job may fail with the fault and be relaunched under its id. Jobs you stopped, jobs that failed for their own reason and jobs that failed in an earlier episode stay as they are. The node awaits the worker's hello for up to 150 s (`soapy worker for <id>: no hello in 150 s`); on the field host the B210 says hello in 2-4 s from cached figures, and a first start that measures every sample depth takes about 70-110 s. The worker hands its IQ to the node through a shared-memory ring the node creates for each spawn (64 MiB by default, slots of 2 MiB in bytes holding `cf32` or, from a device whose samples are 16-bit, `sc16` (twice the samples, so twice the time), inherited by the child as fd 3; `worker.transport.format` says which, and the node converts `sc16` with the one function a capture's `cf32` uses), and the pipe carries only control and small block descriptors, so a burst of samples never delays a fault or a status. A worker without a ring (`memfd_create` unavailable) sends the blocks on the pipe, and `worker.transport.path` says which (`shm` or `pipe`).

`worker` in `source list` is `{pid, restarts, since_ns, transport, transport_dropped, starting, attachment, link_measure}` (`null` for a source that runs in the node; the last three are in "Measuring a radio's link" below): `restarts` counts respawns since the node started, `since_ns` is when the live worker said hello, and a `restarts` that climbs while the device is healthy means the driver keeps crashing: read the worker's own stderr lines, prefixed `zeta-soapy-worker[<id>]: `. A source that stays faulted while `restarts` rises is waiting for a device that is not on the bus. A device absent at startup attaches faulted (`device not found: <args>`) when an earlier start saw it, else the flag is refused (`device not found: <args> (never seen on this host; plug it in once)`, exit 1); a worker that crashes opening a device an earlier start saw (`soapy worker for <id> died: signal 11 (SIGSEGV)`, UHD still releasing it from a node that just stopped) faults the source the same way and is respawned with backoff. `--soapy-in-process` opens the device in the node as before, for diagnosing the worker only: a crash then ends the node.

### A radio's ceiling

A radio can be asked for more than it can stream: a B210 will tune 61.44 Msps over USB 3 and then fall 16 % short with more than a thousand overflows a minute, so every job in the capture sees gaps. At open the worker therefore probes the device's rate candidates from the top (a UHD ladder's rungs, at most 6; `max`, `max/2`, `max/4` for a range device), each for a 1.5 s settle after the retune (a device may drop and overflow for a second or two after a rate change, a B210's master-clock switch among them) and then a 1.5 s dwell, and reports the first rung with at most one overflow in the dwell and at least 98 % of the nominal rate delivered as `capabilities.max_bandwidth_hz: {measured: N}` (one overflow is allowed because a healthy B210 still overflows now and then at every rate; a rung rejected for overflows gets one more settle and dwell before the probe moves down). The probe runs for every sample depth the link carries (below) inside the worker's 150 s to say hello. It logs `zeta-soapy-worker[<id>]: rate cap <N> Hz measured (<k> rungs tried)`. If no rung is clean nothing is measured: the worker keeps the driver's asserted cap, logs `rate cap not measured: no rung clean (<k> rungs tried, lowest <hz> Hz); set max_rate_hz=<hz> to pin one`, and probes again at every respawn. A rung that loses the device ends the probe with `rate probe: device lost at <rate> Hz: ...` in the journal, and the worker keeps the asserted cap too. A request wider than a measured cap is refused `out_of_range` with `why: "insufficient bandwidth: <source> delivers at most <N> Hz (measured at open)"` (`why` is `null` for other refusals): the radio cannot do it, so narrow the request or take a second radio. That is a different thing from a digital link the node reports but does not yet decode, which is not a bandwidth problem. A measured cap is kept with the capabilities in `sources/<id>.json` and passed to every respawn (`--rate-cap-hz N`), so healing never re-probes; a respawn that measures a cap where the first open could not replaces the asserted one.

To state a ceiling yourself and skip the probe, put `max_rate_hz=N` among the `--source-soapy` args (zeta's own item, stripped before the driver sees it, like `gain=` and `ppm=`). That is the remedy for `no rung clean` and for a top rung that reliably kills the device (`rate probe: device lost at <rate>` at every open). An asserted cap is never cached: it is read from the args at every start, so changing or removing the item takes effect at the next one. To probe again instead (a new cable, a faster host, devices added around the radio), the operator runs `source_measure` (below); there is no state file to edit. An agent that meets `insufficient bandwidth` reports it to the operator with the source and the cap, and never deletes or edits the node's state files.

### Measuring a radio's link

A link that carries more than one sample depth (a B2xx over USB: `sc16`, `sc12` and the lossy `sc8`) is probed at each depth at open, so `source list` shows `capabilities.depths` (`bits`, `lossless`, `max_bandwidth_hz` for each, a `measured` claim once probed) and `depth`, the one in force, whose cap is the top-level `max_bandwidth_hz`. The source runs the fastest lossless depth unless `bits=N` among its `--source-soapy` args names another; a lossy depth is chosen only that way, or for a `scan`'s run. The probe streams the radio flat out, so a start with no cached figures says so first, in the journal: `measuring what this radio's link can sustain (up to about <N> s; the host will be busy; max_rate_hz= skips it)`, then `measured in <N> s`. A node's first start shows only those two lines (it serves after its sources' first hello). A respawn that measures (a replug onto another link speed, `source_measure`) also shows `worker.starting` (`what`, `detail`, `step`, `of`, `eta_ms`) in `source list`, the page's notice on the source's tab and `measuring` and `measured` events on the source's feed; its line then opens the parentheses with `link speed changed (<old> to <new>)` or `as asked`. Tell a user whose system stalls for a minute right after a start that this is what it is. `max_rate_hz=N` skips the probe.

The figures belong to the link's speed: `worker.attachment` is `usb:<bus>-<path>@<speed>` (Mb/s, `null` where the source cannot tell) and `worker.link_measure` the latest hello's figures with `at_speed`. A source measures again by itself only when its negotiated speed differs from the one its figures were measured at (a USB 3 radio now on a USB 2 port, or the reverse); another port at the same speed keeps the cache. `source_measure` (the page's "Run link diagnostics" on the radio's tab) measures again on request, about a minute: use it when the system stutters after devices were added around the radio, and only when the operator asks. It is a `conflict` naming the holders while a job or a pin holds the radio (retry with `replace` only on the operator's word: stopping a job is theirs to decide), and `other` for a source with no worker. Its figures govern tunes at once through `depth` and are cached for the next start; `capabilities` is set once a run (the first hello of the run that differs from it replaces it, later ones leave it), so read `depth` and `worker.link_measure` for the figures in force.

`stats.tune_gaps` counts the overflows the reader's own blocked tune caused: they fall among the samples a tune discards, so `overflows` stays the figure for loss while streaming. Compare `overflows` at a rate measured clean; a climb there is the system, not the sweep.

### Sweeping a span

`scan` steps the source across a span wider than one capture and paints one spectrum row per step (`job start --kind scan`, see the `job_start` notes). The pace is the radio's: each step is the source's whole capture (`context.scan.rate_hz`), advancing by its usable part (`step_hz`; 0.9 of the capture on a SoapySDR device, `usable=F` among its args to change it), taking 20 ms of settled samples (`dwell_ms`) and tuning on; the FFT of one step runs while the next tune happens. Report how long a sweep takes from `context.scan.timing.sweep_ms`, not from step counts or a guess, and a calibrating tune from `timing.slow_tunes` (a B210's AD9361 calibrates about every fourth tune and a tune that does costs over ten times a plain one; the node reports it and does not remove it). A source settles for `settle_ms` after a tune (1 ms on a B2xx, `settle_ms=N` among the args to change it); the first row of a step should match its later ones.

Choose the look by what the operator wants to find. The default (`dwell_ms` 20, `average` 1) is the fastest sweep and catches a burst. Many quick looks averaged (`average` 16 at 20 ms) lowers the noise ripple as one 320 ms dwell does and still revisits every step each sweep: it shows a weak steady carrier. A long look (`dwell_ms` 250) sees a faint steady signal in one dwell and sweeps slowly. A row carries `sweeps`, how many sweeps it averages so far, and `bits`. Change the look on a running scan with `graph put` on its `fft` node (`dwell_ms`, `average`, `bits`); the page's scan row has a select for it.

`bits` takes a lossy depth for the scan's run (8 on a B210), steps at that depth's wider capture and puts the source's depth back at the end. At 8 bits a weak signal beside a strong one in the same capture is lost in quantisation noise: it is for a coarse survey of occupancy, never for faint signals, and never the default. It is refused as a `conflict` naming the other holders unless the scan is the source's sole holder (every holder would get the coarser samples), and refused naming the depths for one the source does not carry.

A HackRF at `usable=1.0` shows its edge spur at every capture's +-fs/2; the default 0.9 masks it. A row's `masked` ranges are no data, not low power.

Running a second node that owns the radio and attaching it with `--source-node` is federation (a radio on another host), not crash protection; one node with `--source-soapy hackrf=driver=hackrf,gain=32` is the form for a local HackRF.

### Watching analog video

Analog video (an FPV VTX, a reverse camera, a 1.3 GHz or 5.8 GHz link, an HDMI sender that is really analog) is wide FM carrying a composite signal. `video_decode` (family `tv`) is `channel`, then `video_fm_demod`, then `composite_decode`: it centres where it is started, so centre it on the measured carrier (`classify`, or the waterfall's box), never on a channel grid. The default band is 24 MHz (at least 6 MHz). On the field host's B210 a 24 MHz band lands on the 30.72 Msps rung (the cap plan 28.1 measured); a 20 MHz band keeps the 20.48 rung. A source that cannot carry the band refuses it `insufficient bandwidth`, naming its cap, and the page greys "Decode video" with those words: take a second radio or narrow the band.

The job's `event` stream carries `protocol: "video"` with `system`, `lock` (`locked` or `no lock`), `standard`, `line_hz`, `fields`, `snr_db` and `dropped` (input the decoder lost: blocks its own queue or input dropped, plus one per gap in the composite's time from a block lost upstream, in the `channel` stage or the demodulator; each costs a field, not the lock). `locked` with a sensible `snr_db` is a picture. `no lock` looks like a noise thumbnail and an `snr_db` roughly under 10 dB (a rule of thumb, not a threshold the node applies): the carrier may be off-centre, too weak, too narrow a band, or not analog at all. Brightness is referenced to the measured sync depth, so the `deviation_hz` setting does not dim the picture. The composite stream carries `role: composite` and is not audio: it is never a job's audio for `job_attach`, the mix or `--then-tail` (a `stream tail` on it still reads it).

To look, open the pane's tab (labelled `video`, shown only while a `video_decode` job runs on some radio) or click the live thumbnail in the job's tag on the waterfall (hidden whenever the rail collapses the tag to a thin segment, which happens when tags overlap and is common in the narrow footprint's short rail; a hidden thumbnail holds no socket). The full view fills the main area with the selected feed, a status line and a strip of every running feed across radios. `view set video smooth|sharp` picks the profile: `smooth` follows every field at 360 x 288 (every line: a field is already anamorphic and the page stretches it to 4:3), `sharp` is full width at a few frames a second; the tag's thumbnail is 120 x 96. To keep a still, run `zeta stream save --stream <id> --path still.png --node URL` (take the id from `stream list`; it writes the next frame as a `sharp` picture).

A digital video link never locks; see the verdict under "Where to look". The audio subcarrier is not decoded yet.

### Exploring an unknown signal

A signal nobody has named goes up a ladder; stop at the first rung that answers, and report what each rung found.

1. Direct. Layer 1 measures the band (`measure`, `classify`) and labels it; layer 2 scores the catalogue, the seed signatures and every mode row. `classify` tries the quick rows; `identify` runs each fitting row for its dwell, in score order, and publishes a `candidate` event as it reaches each row. A proof (a CRC-clean decode) ends the ladder and leaves the decoder running under `verdict.job`.
2. The register and the decoders it suggests. With the register attached, `knowledge_lookup`, `classify` and `identify` list the licensed assignments overlapping the band, nearest first, and a row whose width fits the licensed channel scores 0.2 higher, so the decoder that fits is tried before one that does not. The allocations ride along in the result whatever happens next. The `reach` of the answer (`km`, `class`, `antenna`, `note`) says how far a site can plausibly be heard on the station's antenna: skywave is not cut, tropo and UHF are; give it when the register is empty or the nearest site is out of reach.
3. Features and the register, as an `unknown`. With no proof the answer is `unknown`, never a guess: `verdict.mode` is `null` and the verdict carries the layer-1 features and `allocations`; the watch's `unknown` summary pairs the measurement with the register as `register says <EMISSION> (<bw> kHz <modulation> <info>) <licensee>, <site>, <d> km`. Report both together, for instance "unknown at 404.025 MHz: burst fsk, 16 kHz, 4800 Bd, 1 % duty every 1.0 s; the register says 16K0F1D (16 kHz FM data) Example Water Authority, Example Hill site, 17 km". Say which numbers were measured over the bursts (a low `duty`). A match between the measurement and the register's emission is a coincidence of features, not a decode: say "consistent with", never "is". When the measurement matches a signature the operator accepted (layer 2's `learned <name>`, a match score of 0.5 or more, features-only signatures included), the unknown verdict says so: `verdict.label` is `learned <name>` and `verdict.learned` gives its `id`, `name` and `confidence` (the label's: the signature's confidence times the score; `null` otherwise); the watch's summary reads `learned <name> at <MHz>: <measurement>` and it neither captures nor proposes that signal again, though it keeps retrying identify. Report it as "matches the learned signature <name>", still not a decode.
4. Captures. The explore policy of a watch, `on_unknown: capture` on its `detect` node (default `report`), writes an unknown signal's bursts to disk once per run (the count resets when the watch restarts) as a `capture` child, and publishes `capturing` and `captured`; `job_context.watch.signals[].captures` lists the ids. By hand, `job_start` a `capture` job on the signal's measured band, or use the box menu's "Capture bursts". `capture_list` shows the store newest first, `capture_show` one record, `capture_classify` classifies the band a capture recorded, so a signal that was gone before anyone looked can still be asked "what is this?", and `capture_delete` removes one (an agent run needs `allow.delete_captures`). The store keeps 256 MB and 7 days by default (`--bursts-keep-mb`, `--bursts-keep-days`); pinned captures are never expired. The page's Explore tab (`view set pane explore`) lists them with What is this, Download and Delete.
5. A signature. The node proposes one itself when a capture job ends with features only (confidence 0.3) or a bits or graph job reads five CRC-clean frames (0.9; a mode row's job, a frame row's included, proposes nothing: its frames prove the row): the `signature` event and `job_context.proposal` (the proposal's id; `signature_show` gives the rest) carry it. A hand `signature_propose` with a framing is 0.6. The operator accepts or rejects it (`signature_accept`, `signature_reject`). A framed one becomes a `learned_` mode, named by the record's `decoder` field (read it with `signature_show`; never build it from the id), so the next encounter is proved by `identify` or decoded by `job_start --kind <decoder>`. A features-only one only labels later encounters `learned <name>`: a layer-2 match, not a decode. The ladder ends here.

When `classify` answers `too_old` the ring has moved on: the error carries the ring's earliest start in its detail; re-issue on the latest span it still holds, and say so.

The playbook ends at classification and, when the operator accepts one, the signature written back so the next encounter is a layer 2 match or, for a framed signature with a CRC and a symbol rate, a decode. It stops before decoding the content of a third-party transmission: measuring, classifying, capturing a few bursts to identify the signal and saying who the register says is licensed is in scope; recovering and reading the messages of someone else's link is not, and interception law varies by country, so that is the operator's decision and legal responsibility. State this legal line once when a request goes that way, and do not repeat it. A known public protocol the node has a decoder row for (APRS, POCSAG, ADS-B, ACARS, AIS, radiosondes, M17) is the ordinary use of the radio and is not held back by it. Bits and frame hypotheses are ops now (below), and so is the signature write-back (item 11 below).

### Working layer 4

When a signal has no known decoder and a capture of it exists, work its bits with the ops, in this order:

1. `capture_list` (narrow with `signal` when the band is known); take the capture's id from it, never invent one. No capture: start a `capture` job on the signal's measured band first.
2. `bits_start` with `capture`, `modulation`, `symbol_rate_baud` and `order`, each `null` to take it from the capture's feature record and its layer-1 label (`fsk` and `fm` give `fsk`; `psk` and `ook` themselves). It takes every capture of the capture's `set` in time order and builds `capture_in → resample → <demod> → clock_recover → slicer → bit_view`; a signal off the capture's centre whose measured band reaches past the resampler's passband (offset plus half the measured width over 0.45 times the chain rate: a wide capture) is shifted to its measured centre first by a `channel` stage, unless layer 1 labels it `noise` (`job_context.bits.shift_hz` says by how much); the channel's band is the measured width × 1.25, at least 4 × the baud, cut to what the capture holds about the measured centre (a weak signal's measured band reaches into the noise). fsk takes order 2 or 4, psk 2, 4 or 8, ook 2. It refuses with `other`: `no symbol rate measured; give symbol_rate_baud`, `no modulation measured (label <x>); give modulation`, `symbol_rate_baud <r> needs <4r> Hz; the capture holds <rate> Hz`, `symbol_rate_baud: at least 1` for a rate of 0, a measured centre the capture holds under 4 × the baud about (`the measured centre <c> Hz leaves <room> Hz of the capture <lo>..=<hi> Hz about it, under the <floor> Hz the chain needs (4 x <baud> Bd)`), and a set of mixed rates or centres (`capture <id>: <rate> Hz differs from <first>'s <rate> Hz`). Short FSK bursts, real or synthesised, often have no measured rate: give it. On a refusal, report it and the features; do not guess a rate.
3. The chain's rate: about 8 samples per symbol for fsk and ook; for psk the smallest multiple of 4 times the baud that also covers a carrier offset of 2000 Hz (`psk_demod`'s coarse estimate is unambiguous only within the rate over twice the order). A 4-FSK or OOK chain takes `clock_recover` `algorithm: early_late` (the default Mueller and Müller mistimes 4 levels and reads no timing across an OOK pulse's flat top); `bits_start` sets it, a chain built by hand must too, or it mistimes.
4. The replay loops (`loop: true`, `realtime: true`, a 500 ms pause between passes) and the job runs until stopped. A one-pass chain (`loop: false`) drains to its end, its last chunk delivered, and ends `stopped`. An unpaced loop (`loop: true`, `realtime: false`) needs `loop_pause_ms` of at least 100.
5. Read, with `job_context`: `bit_view` (the hypotheses over the whole set: line code, frame period, sync-word candidates, CRC candidates, each with its counts; its window spans the set's captures, each a segment no word or frame crosses, the counts summed over them, and a looping replay's next pass starts it afresh, so a pass is never counted twice), `clock` (`rate_est_baud`, `locked`: a clock that is not locked, or an estimate far from the rate, means the rate or the modulation is wrong) and `slicer` (`symbols`, `events`, `stream`). Never tail the whole bits stream: `stream_tail` of `job_context.slicer.stream` with `max_frames` at most 4. A CRC candidate with `period_bits` 0 means the frames run from each sync to the next.
6. Change a running chain with `graph_get`, edit, `graph_put`: a `slicer` with `differential: true` or another `mapping`, or a `correlate` node (`{pattern, bits, max_errors}`) between the slicer and `bit_view`. `correlate` forwards every event and adds `sync` hits, so the hypotheses keep coming. A node's output feeds one edge: there is no fan-out. When a hypothesis checks (a sync word, a length rule, a CRC), `graph_put` a `frame_decode` node (`{sync, length, crc, encoding}`) after the slicer and read `job_context.frame_decode` (its counters) and the `frame` events' `crc_ok`. Use `length: {to_next_sync: true}` for a CRC candidate with `period_bits` 0.
7. PSK's phase ambiguity is not resolved: a BPSK stream may come out inverted and a QPSK stream rotated. `slicer` `differential: true` is the answer; try it before calling a PSK stream noise.
8. Report the hypotheses with their counts (`sync` hits, matching frames out of tried, CRC passes), and say which are guesses. When nothing checks, stop at "unknown" with the measured features: do not claim a protocol, a framing or a payload from a hypothesis without a CRC-clean check.
9. A chain of your own: `graph_start` runs a graph that has no radio (a `capture_in`, `stream_in` or `mix` at its root) on the node source (`zeta graph start --file`). `graph` and `bits` are reserved kind names, so `job_start` refuses them (an operator mode row with either id cannot be started).
10. You may stop your own `bits_start` and `graph_start` jobs when done (`job_stop`); never one you did not start. `jobs_stop` with a plan filter skips them (their centre is the capture's, not a channel they hold).
11. The node proposes a signature itself: a capture job's end with features only (confidence 0.3), a bits or graph job on five CRC-clean frames (0.9). A mode row's job (a frame row's, a `learned_` row's) proposes nothing: its CRC-clean frames prove the row, and its session says `proved`. It publishes a `signature` event and sets `job_context.proposal` (the proposal's id; a capture job's is `job_context.capture.proposal`). A hand `signature_propose` with a framing is 0.6. `signature_list {state: "proposed"}` lists the proposals; `signature_show` gives one's evidence. Report a proposal to the operator with its confidence and evidence. `signature_accept` and `signature_reject` are the operator's (`allow.accept_signatures`): never call them unasked. `signature_propose` is for when the operator names a framing by hand. An accepted framed signature (a CRC and a symbol rate) becomes a `learned_` mode like any other, named by the record's `decoder` field (take it from `signature_show`; never build it from the id): the next encounter is proved by `identify` or decoded by `job_start --kind <decoder>`. A features-only one labels later encounters `learned <name>` (an identify verdict with `learned`, the watch's summary). That is a layer-2 match, not a decode: say so.

### The rule

When nothing matches, say "unknown" and report the measured features (centre, bandwidth, modulation, symbol rate, burst timing). Do not guess.
