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
    "Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it. A `why` of `insufficient bandwidth: ...` names the source's measured ceiling, not a decoder gap: narrow the request or move to a wider radio rather than wait for a mode, and report it to the operator, who can pin or re-measure the cap. Never delete or edit the node's state files.",
    "Unknown signals are reported as unknown, never guessed.",
    "Ids are never invented: `source` values come from `source list`, `job` values from `job list`.",
    "Radios: when the request names a radio (a source id as a whole word), use it. Otherwise use the default radio the request's context names (`## This request` in the system prompt); with none, follow the `## This request` paragraph (under `focus` with no radio in focus, list the free radios and ask; under `focus-fallback`, the first free); without that section (an MCP client), the first free id `source_free` gives. The policy decides when the default radio cannot take the band (it does not cover it, a job or a pin holds it, it is faulted): under `focus`, the default, use only the default radio; start nothing, say why, name the free radios and ask which to use; never start on another unnamed radio (the gate answers `policy`). Under `focus-fallback`, use the first free radio in the preference order and say which and why. Under `free-first`, use the first free radio. Under `ask`, ask before starting. Never stop or replace a job to free a radio.",
    "Jobs belong to the operator: never `job_stop` a job or put one in `replace` unless the request names it, the run allows it or, for `job_stop`, you started it in this run. A `conflict` is reported with its holders; the operator decides.",
    "For a kind on every channel of a plan use `plan_start`, never one `job_start` per channel.",
    "Plan ids come from `channel_plans`: read it before naming a plan. A band with no plan takes one `job_start` per measured station; never invent a plan.",
    "To stop every job, or every job of a source, kind or plan, use `jobs_stop`, never one `job_stop` per job.",
    "For a named set use `set_start`, never its members by hand; set ids come from `set_list`.",
];

const RUNNING: &str = "Every command takes these global flags:

- `--state-dir PATH`: where jobs and captures persist (default `$XDG_STATE_HOME/zetasdr`, else `~/.local/state/zetasdr`). Pass the same directory on every invocation.
- `--source-file ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]`: attach a SigMF recording as source `ID`. With the range it plays as a radio tunable over `N`-`M` Hz (its `tune_min_hz`/`tune_max_hz` in `source list`): a band inside the range moves the capture's centre there, the samples staying the recording's (a test and demo aid); without it only the recording's own band is accepted.
- `--source-kiwi ID=HOST:PORT[/PASSWORD]`: attach a KiwiSDR receiver (0-30 MHz, at most 12 kHz wide) as source `ID`; the port is required and the password is never printed. Without `/PASSWORD` the password comes from `ZETA_KIWI_PASSWORD_<ID>` (`ID` upper-cased, other characters as `_`), which keeps it out of `ps`; the flag wins when both are given.
- `--source-position ID=LAT,LON[,ALT_M]`: where source `ID` is (degrees, metres; repeatable); a source that reports its own position (a KiwiSDR with GPS) overrides it. The station (`station_set`) stays the node's.
- `--source-node ID=SOURCE@URL[,max_bandwidth_hz=N]`: use source `SOURCE` of another ZetaSDR node, `URL` being its `http://HOST:PORT` (a `zeta serve --listen` node), as source `ID`. The node's secret comes from `ZETA_NODE_SECRET_<ID>` (`ID` mapped as for Kiwi passwords) and is never printed; without it the command is a usage error (exit 1). A remote source ships narrowband by default: `bandwidth_hz` is capped at 250 000 Hz (or the remote source's own maximum when smaller) unless `max_bandwidth_hz=N` (1-20 000 000) widens it explicitly. Each tune holds a `passthrough` job on the other node, so its `source list` and `job list` show who is using the source; the job is stopped when the tuning changes or this process exits normally (a killed process leaves it running; stop it there with `job stop`).
- `--source-vrt ID=udp://ADDR:PORT`: receive VITA 49.2 (VRT) packets, one per UDP datagram, on `ADDR:PORT` as source `ID`; `ADDR` must be a LAN address (`0.0.0.0` listens on every interface). The sender sets the frequency and rate: a job's band must lie inside the sender's last context packet (`source list` shows it as `current`; a job started before any context packet waits up to 1.5 s for one), otherwise the job fails with `unsupported` saying where the sender is; retune the sender, not the job. Packets that do not decode are skipped.
- `--source-soapy ID=ARGS`: attach a SoapySDR device as source `ID`, `ARGS` being SoapySDR device arguments such as `driver=hackrf,serial=...`. The device runs in a `zeta soapy-worker` child the node spawns and respawns: a driver crash or an unplugged device faults the source (`source list` shows `fault`, jobs on it fail with `source <id>: <fault>`) and never ends the node. The node then respawns the worker and waits for the device; when it is back it restores the pin and the gain and relaunches every job that failed with that fault under its own id, the `watch` and its decoders and an attached `transcribe` included, logging `zeta: source <id>: recovered: ...`. `worker` in `source list` shows the child's pid, how many times it was respawned and `transport` (`path` `shm`, a shared-memory ring the node creates per spawn, or `pipe`; `format`, `cf32` or `sc16`: a device whose samples are 16-bit puts them on the ring unconverted, half the bytes and twice the time in the same ring, and the node converts them; a worker of another protocol or ring version is refused with a fault naming both) with `transport_dropped`, `attachment` (`usb:<bus>-<path>@<speed>`, the link's negotiated speed in Mb/s, `null` where it cannot be told) and `link_measure` (the figures of the latest hello and the speed they were measured at). At open the worker probes the rates the device offers and reports the first clean one as `max_bandwidth_hz: {measured: N}`: a request wider than it is refused `out_of_range` with `why: insufficient bandwidth: ...`. A measured cap is cached in `sources/<id>.json` and passed to every respawn; a probe with no clean rate measures nothing and keeps the driver's cap. A `max_rate_hz=N` item in `ARGS` (zeta's own, like `gain=`) asserts a cap and skips the probe; it is read from `ARGS` at every start, never cached. It is the remedy when the journal shows `rate cap not measured: no rung clean` or, at every open, `rate probe: device lost at <rate>`. A device whose link carries more than one sample depth (a B2xx: `sc16`, `sc12` and the lossy `sc8`; `capabilities.depths`, each `{bits, lossless, max_bandwidth_hz}`) has each probed at open, and the top-level `max_bandwidth_hz` is the figure for the depth in force (`depth` in `source list`): the narrowest lossless one that measures as wide as the widest (a lossless depth carries all the converter gives, so a narrower one that streams as wide is the same fidelity in fewer bytes; one that measured narrower or was never measured clean does not win, so the source's bandwidth never shrinks under a resumed job) unless `bits=N` in `ARGS` asks for another (the only way to a lossy one; a `scan` takes a lossy depth for its run with `bits`). Three more items in `ARGS` are zeta's own: `settle_ms=N` (0-10000) is how long a tune takes to settle after the device returns from it, in place of the radio's (1 ms on the B2xx family, 10 ms for a radio zeta has not measured), `usable=F` (0.5-1) the part of a capture the front end passes flat, in place of 0.9, and `bits=N`. A fourth, `antenna=NAME`, picks the receive port by the device's own name for it (a LimeSDR's `LNAW`, a B2xx's `RX2`), set when the device opens and logged as `zeta: antenna for <id>: <NAME>`; a name the device does not list refuses to attach the source, naming the ones it has, and without the item the driver chooses the port, which is not always the one the antenna is on (a radio that hears nothing on a band another radio hears is the sign). While a tune blocks the reader the device overflows, and that is counted as `stats.tune_gaps`, not as `overflows`: `overflows` stays the figure for loss while streaming. A start that has no cached figures measures first, and says so: the journal reads `measuring what this radio's link can sustain (up to about <N> s; the host will be busy; max_rate_hz= skips it)` and, when the worker says hello, `measured in <N> s` (a B210's three depths take about 70-110 s, within the 150 s the node waits for the hello). A node's very first start shows only those two lines, since it serves after its sources' first hello; a respawn (a replug, a link-speed change, `source_measure`) also shows `worker.starting` (`what`, `detail`, `step`, `of`, `eta_ms`) in `source list`, a notice on the source's tab and the feed's `measuring` and `measured` events, and its line opens the parentheses with the reason (`link speed changed (<old> to <new>)` or `as asked`). `max_rate_hz=N` asserts the cap and skips the probe. The measured figures are cached in `sources/<id>.json` with the link's speed (`measured_at_speed`): a source measures again by itself only when its negotiated speed differs from the one its figures were measured at (a USB 3 radio on a USB 2 port, or the reverse); another port at the same speed keeps the cache. To measure again otherwise (the system stutters after devices were added around the radio, a new cable, a faster host), `source_measure`; an agent reports `insufficient bandwidth` to the operator, asks before stopping a job to measure, and never deletes or edits state files. A device absent at startup is attached faulted when the host has seen it before, else the flag is refused (exit 1). `zeta soapy-worker --id ID --args ARGS` is what the node spawns; it is not run by hand. The worker inherits the node's environment (the driver reads `UHD_IMAGES_DIR`, `SOAPY_SDR_PLUGIN_PATH`) minus the secrets: every `ZETA_*` variable whose name contains `SECRET`, `PASSWORD` or `KEY` (`ZETA_SECRET`, `ZETA_NODE_SECRET_<ID>`, `ZETA_KIWI_PASSWORD_<ID>`, `ZETA_STT_NODE_SECRET`, `ZETA_LLM_KEY` among them). A `gain=DB` item in `ARGS` is zeta's own: automatic gain is turned off and the overall receive gain set to `DB` dB when the device opens (logged with the device's range as `zeta: gain for <id>: ...`), and a `DB` outside that range refuses to attach the source (exit 1); without it the driver's default gain is used, which on a USRP is its minimum. A gain set live with `source_set_gain` is persisted and wins over `gain=` on the next start. `ppm=N` corrects the device's frequency error: the hardware is tuned `N` parts per million higher than every frequency zeta reports, so a device that reads high by `N` ppm (a carrier on 146.975 MHz seen at 146.9754) reads true. `zeta soapy find [--filter ARGS]` lists the devices attached to this machine as JSON. Only in a zeta built with `RUSTFLAGS=\"--cfg soapy\"` (needs libsoapysdr-dev); otherwise both exit 1 saying so.
- `--soapy-in-process`: open `--source-soapy` devices inside the node instead of a worker child; for diagnosis only, as a driver crash then ends the node.
- `--ring-budget-mb N` (env `ZETA_RING_BUDGET_MB`): memory budget for one source's IQ history (default a quarter of RAM, at most 4 GiB). The ring keeps 10 s at narrow rates and shrinks to as little as 1 s at wide ones (a ring holds 8 bytes per sample, so 16 Msps gets 8 s of a 1 GiB budget); `iq read` and `classify` spans older than that fail `too_old`. A gap in the capture (the source lost samples) does not clear the history: it is kept in segments, and a span that crosses a gap fails `gap` with its `gap_start_ns` and `gap_end_ns` (the last sample before and the first after), so read either side of it. `source_list`'s `ring` gives `seconds` (the size), `held_s` (the seconds of samples held, at most `seconds`, less while the ring fills), `run_s` (the seconds in the newest contiguous run, after the last gap: the longest span ending now that an `iq read` can serve) and `gaps` (how many are in the history). The size chosen is logged to stderr as `zeta: ring for <source>: <s> s at <rate> Hz (<MB> MB)`.
- `--bursts-keep-mb N` (env `ZETA_BURSTS_KEEP_MB`) and `--bursts-keep-days N` (env `ZETA_BURSTS_KEEP_DAYS`): how much the burst capture store keeps (defaults 256 MB and 7 days; zero is a usage error). Expiry removes the oldest unpinned captures until the unpinned rest fit the megabytes, then every unpinned capture written more than the days ago; it runs when `zeta serve` opens (a one-shot expires at open only when given one of these flags while no serve owns the state directory, so a plain one-shot never drops what serve's policy keeps) and after each write (a plain one-shot never expires on write either), and a capture just written is spared by its own write. A pinned capture is never expired. `--bursts-trial-mb N` (env `ZETA_BURSTS_TRIAL_MB`, default 64 MB) is the separate budget of the trial slices a `choose` job records: they expire oldest first against it alone, and neither kind ever expires the other. An unreadable `bursts/index.json` is moved aside as `index.json.broken-<ulid>` and the index rebuilt from the `.sigmf-meta` files (pins are lost), logged as `zeta: bursts: index unreadable (<e>); moved aside`. All three belong to the serving node: with `--node` they are a usage error (exit 1).
- `--knowledge-acma URL[,reach_km=N][,propagation=PATH]` (env `ZETA_KNOWLEDGE_ACMA`): attach the ACMA licence register service (an MCP server over HTTP): the service's `http://HOST:PORT` (a trailing `/mcp` is accepted) as the `acma` knowledge provider of `knowledge_lookup` and `classify`. `reach_km=N` (a positive number) replaces the search bound's kilometres. `propagation=PATH` names a JSON file holding any of the propagation estimate's assumptions (`k_factor`, `beyond_horizon_db_per_km`, `off_beam_default_db`, `ground_wave_km`, `skywave_min_hz`, `skywave_max_hz`, `in_range_floor_dbm`, `search_margin`, `assumed_site_height_m`; the rest keep their defaults), read at start: a file that cannot be read or parsed, names another field, or breaks a rule (every number finite, `k_factor` above 0, `ground_wave_km` above 0, `beyond_horizon_db_per_km`, `off_beam_default_db` and `assumed_site_height_m` at least 0, `search_margin` at least 1, `skywave_min_hz` not above `skywave_max_hz`) is a usage error naming the file and the field. Every register answer echoes the assumptions it used in `assumptions`. The options come in any order, each once. The service is probed once at startup (one-shot commands pay at most 5 s when the host is unreachable); an unreachable one is logged as `zeta: knowledge acma: <url> unreachable: <e>` and the provider still attaches, giving no allocations until it answers. Needs `station set` for distances. It belongs to the serving node: with `--node` it is a usage error (exit 1).
- `--run-dir PATH` (env `ZETA_RUN_DIR`): the directory under which each external program run (a program row fed through a fifo) makes its own `external-<random>` subdirectory, mode 0700, removed when the run ends (default `<state-dir>/run`; a packaged install sets it in its unit file, for example `/opt/<package>/var/run` or the system temp directory). It is made with mode 0700 when missing; `serve` exits at startup naming it when it is not a directory or cannot be written.
- `--registers-dir DIR` (env `ZETA_REGISTERS_DIR`): the operator's register queries, one `<provider>/<id>.json` file each, read after the built-in ones (no default; a missing directory is no error). A query with a built-in query's provider and `id` replaces it. A query is a row like those in `registers/acma/`: `id`, `provider`, `name`, `call` (only `execute_sql`), `sql` (a `SELECT` or `WITH` with `{name}` placeholders and no `;`), `params` (each a `name` and a `kind`, `number` or `id`: only numbers and register ids are ever substituted), `limit` and the result `columns` it must return. A directory with a file that does not load is reported at startup as `zeta: registers: <file>: <field>: <error>; using the built-in register queries`. `zeta registers check [PROVIDER] [--center-hz HZ] [--bandwidth-hz HZ]` (local only; no radio needed) runs every query against the `--knowledge-acma` register with sample values (a signal at 400.5 MHz, 1 MHz wide, unless the flags give another; the station's position when one is set in the state directory; for a query that takes a register id, the ids of the first `candidates` row) and prints `<provider>/<id>: ok <rows> rows <ms> ms`, `<provider>/<id>: fail: <reason>` or `<provider>/<id>: skipped: <reason>` for each, then `<provider>: skipped <n> of <total> queries (<q> quiet band, <d> not runnable)` when any was skipped (a quiet band, with no `candidates` row to take a register id from, is no failure; a skip the check cannot run, such as a `candidates` row without the id column, fails it), then `<provider>: answer <ms> ms of <deadline> ms` for one whole answer. A register that does not answer is `<provider>: unreachable: <reason>` alone, and with no register it prints `no register configured`. It exits 0 only when nothing failed.
- `--plugin KIND=PATH`: register the program at absolute `PATH` as stage kind `KIND` (lowercase, not a built-in kind). It is probed at startup; a failed probe exits 1 naming the kind and the reason (a `hello` of a protocol other than 1 or 2 is refused). A plugin runs with a cleared environment: only `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM` and `PYTHONPATH` pass through, and each `ZETA_PLUGIN_ENV_<NAME>` variable reaches it as `<NAME>`; nothing else (no `ZETA_SECRET`) does.
- `--modes-dir PATH`: the operator's decoder mode rows, one `*.json` file each, read after the built-in ones (default `$XDG_CONFIG_HOME/zetasdr/modes`, else `~/.config/zetasdr/modes`; a missing directory is no error). A row with a built-in mode's `id` replaces it. A row that does not parse or validate never stops the node: it is listed `broken` by `modes list` and reported at startup as `zeta: mode <id> (<file>): <field>: <error>`, and a broken file that meant to replace a built-in row leaves that row in force.
- `--handlers-dir PATH`: the operator's handler files, one `*.json` file each, read after the built-in ones (default `$XDG_CONFIG_HOME/zetasdr/handlers`, else `~/.config/zetasdr/handlers`; a missing directory is no error); each adds an external program as a handler of the modes it lists (`handlers/README.md`). A file with a built-in file's `id` replaces it. A file, or a mode entry, that does not load never stops the node (a bad entry breaks only itself): it is reported at startup as `zeta: handler <id> (<file>): <reason>`, and the file's program is looked up at startup and on a reload that reads a new or edited file, never run (`zeta: handlers: <program>: found <path> (handler <id>)`, or `zeta: handlers: <program>: missing (<hint>; <url>) — modes: <ids>`, the handler then unavailable).
- `--sets-dir PATH`: the operator's named sets, one `*.json` file each, read after the built-in ones (`airband`, `airband_vhf`, `airband_vhf_and_sos`, `ais`, `pagers_au`; default `$XDG_CONFIG_HOME/zetasdr/sets`, else `~/.config/zetasdr/sets`; a missing directory is no error). A set with a built-in set's `id` replaces it. A serving node reads the directory again whenever its files have changed: a file added, edited or removed is in force at the next `set_list`, `set_start` or `set_stop`, with no restart, and a set already running keeps the jobs it started (`set_stop` stops them whatever its file now says; the next `set_start` runs the file as it stands). A set names roles (`radios`), never sources, and never carries `source` or `replace` on a member. A set that does not parse or validate never stops the node: it is listed `broken` by `zeta sets` (`set_list`) and reported on stderr, at startup and when an edit breaks it, as `zeta: set <id> (<file>): <field>: <error>`, and a broken file that meant to replace a built-in set leaves that set in force.
- `--themes-dir PATH`: the operator's themes, one folder each (default `$XDG_CONFIG_HOME/zetasdr/themes`, else `~/.config/zetasdr/themes`; a missing directory is no error), read after the built-in ones. A theme with a built-in theme's `id` replaces it. A folder that does not validate never stops the node: it is listed `broken` by `themes list` and reported at startup as `zeta: theme <id> (<path>): <field>: <error>`, and a broken folder that meant to replace a built-in theme leaves that theme in force.

Sources and plugins are not persisted: repeat the flags, with the same ids and kinds, on every invocation, `serve` included. A source id may be used once across all the source flags. `source list` shows each source's capability record: `remote` is true for a KiwiSDR, a `--source-node` and a `--source-vrt` source, `push` false for SoapySDR devices, and `fault` names a source that has failed (a full Kiwi, an unplugged device); a job on a faulted source fails with that reason.

There are two ways to run:

- One-shot, `zeta <group> <verb> ...`: opens the node, applies one operation, prints the JSON result and exits (0 on success, 2 with a JSON error). It never resumes jobs: a job started one-shot is persisted as `running` and runs once `zeta serve` starts. `job start --then-tail MAX_FRAMES,TIMEOUT_MS` keeps that one-shot node up after the start, tails the job's audio output when it has one (a demodulator), else its decoder's event stream (the event output of its downstream-most decoder node, never a demod's `squelch` events), then stops the job and prints `[job_result, frames_result, {\"stopped\": true}]`; add `--keep` to leave it running (`{\"stopped\": false}`). If the job is not running (e.g. failed) it prints `[job_result, job_record]`, the record's `state` carrying `status` and `reason`, and exits 2; `stream tail` on its own only sees streams of jobs running in the same process, and `job attach` needs its input job running in the same node, so attach against `zeta serve` (`--node`). A one-shot `iq read` or `classify` reads the ring that the command itself fills from the source, waiting up to 5 s for it to cover the span. One-shot commands, read-only ones included, restore the pins and the gain of the sources they attach (so even a one-shot `source list` tunes a pinned radio). One-shot commands, like `zeta serve`, start and probe every `--plugin` program at startup, so with plugins configured even a one-shot `source list` costs one process start per plugin.
- `zeta serve`: resumes persisted jobs and keeps them running until Ctrl-C.
- Against a running node, `zeta --node URL --secret SECRET <group> <verb> ...` (or the `ZETA_NODE` and `ZETA_SECRET` variables): the operation is sent to that node over HTTP and applied there, so jobs started this way keep running; `--state-dir`, the `--source-*` flags, `--plugin`, `--modes-dir`, `--handlers-dir`, `--themes-dir`, `--sets-dir`, `--registers-dir`, `--run-dir`, `--knowledge-acma` and `--bursts-keep-*`, `--bursts-trial-mb` belong to the serving node and are rejected (a usage error, exit 1); `--stt-socket`, `--stt-node`, `--stt-url`, `--stt-url-model`, `--stt-url-languages` and `--soapy-in-process` belong to it too but are ignored, and so are `ZETA_KNOWLEDGE_ACMA`, `ZETA_REGISTERS_DIR`, `ZETA_RUN_DIR`, `ZETA_BURSTS_KEEP_MB`, `ZETA_BURSTS_KEEP_DAYS`, `ZETA_BURSTS_TRIAL_MB` and `ZETA_RING_BUDGET_MB` from the environment. `zeta soapy find` always runs locally; `zeta modes check` and `zeta registers check` are refused (exit 1); `zeta handlers list` lists the serving node's handlers (from its `modes_list`). `--then-tail` is not supported with `--node` yet (a usage error, exit 1): start the job, then `stream list` and `stream tail` against the node. Start that node with `zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) and a secret. `--max-streams N` (env `ZETA_MAX_STREAMS`, default 64) bounds the stream websockets it holds open at once; past that a stream is refused `too many stream connections`. `zeta-mcp --node URL` offers the same operations as MCP tools, and `zeta ask \"REQUEST\"` lets a local model call them.
- `zeta serve --listen PORT --llm-url URL --llm-model NAME` (the other `--llm-*` flags of `zeta ask` apply too) enables `POST /ask` with `{\"request\": \"TEXT\", \"max_turns\": 8, \"allow\": {\"stop_jobs\": false, \"replace\": false, \"delete_captures\": false, \"accept_signatures\": false}}` (`max_turns` 1-32, default 8; `allow` optional, all false by default: without them the agent's `job_stop` and non-empty `replace` calls, its `capture_delete` and `packets_delete` and its `signature_accept`, `signature_reject`, `signature_retract` and `signature_delete` are refused as `not_allowed`; `page_tools` optional, false by default: the web page sends `true`, which adds the page-only `view_set` tool, answered by the agent itself and applied by the page, never by the node): the node runs the agent against itself, so every operation it applies appears on the `ops` stream, and answers the transcript (`request`, `answer`, `calls`, `turns`) as `zeta ask` prints it, plus `allow` as the run had it and `turn_log`, one entry per model completion with its reasoning, content and call count. One run at a time (409 while one is in progress); 503 when serve has no `--llm-url`; a failed run is 502 with the error and the transcript so far; 504 after 300 s.
- `zeta serve --listen PORT --stt-socket PATH` (env `ZETA_STT_SOCKET`; default `$XDG_RUNTIME_DIR/voiced.sock` when that file exists) enables `POST /stt?lang=CODE` (`lang` defaults to `en`): the body is a 16 kHz mono `audio/wav` (at most 4 MiB), the node hands it to the speech daemon at that Unix socket and answers `{\"text\": \"TRANSCRIPT\"}`. The daemon is `local-voice` or any daemon speaking its protocol: one line per connection, `file:<absolute wav path>` answered `ok:<text>` or `error:<message>`, and `languages` answered `ok:en,de,...` (a daemon answering `error:` there is taken to know English only). Instead of a daemon, `--stt-node URL` (env `ZETA_STT_NODE`, secret in `ZETA_STT_NODE_SECRET`, never printed) forwards each request to that node's `POST /stt`, and `--stt-url URL` (env `ZETA_STT_URL`; bearer, if any, in `ZETA_STT_URL_KEY`, never printed; `--stt-url-model NAME`, `--stt-url-languages en,de`) posts each utterance as a multipart `file` to a Whisper server's full endpoint URL (an OpenAI-compatible `/v1/audio/transcriptions` or whisper.cpp's `/inference`), so the audio leaves the host; the three flags are exclusive, for every command (exit 1). A forwarded request carries `X-Zeta-Stt-Hop: 1`, and a node that would forward it again answers 508 (`stt forwarding loop`). `GET /stt/languages` answers `{\"backend\": NAME, \"languages\": [CODES]}` (503 without a backend). 415 when the body is not WAV, 413 when too large, 422 `{\"error\": \"unsupported\", \"language\", \"supported\"}` for a language the backend lacks, 502 when the daemon reports an error, 503 when serve has no backend or cannot reach it, 503 `{\"error\": \"busy\", \"message\", \"wait_ms\"}` when every one of the daemon's 4 slots stayed taken for 30 s (a node forwarding with `--stt-node` reports that as a `busy` drop, not an error), 504 when the daemon does not answer within 30 s of its turn.

`zeta stream save --stream ID --path FILE [--timeout-ms N] --node URL` writes the next video frame to arrive on a stream (a `video_decode` job's `video` stream) as a full-width PNG. It needs a running node; an audio or IQ stream is refused with a pointer to `zeta iq read`.

A video stream on the stream websocket is always PNG in binary mode: `GET /stream/ID?format=binary&video=smooth|sharp|thumb`. `smooth` (the default) is 360 px wide with every line of the field (360 x 288 for PAL: a field is already anamorphic, so its height is not scaled), one frame per field; `sharp` is full width at most 5 a second; `thumb` is 120 px wide with a third of the lines (120 x 96) at most 2 a second. Only the newest frame is sent, and `lagged.missed` counts the frames skipped.

While `zeta serve` runs it owns the state directory (it holds `serve.lock` there). Mutating one-shot commands (`job start`, `job attach`, `job retune`, `job stop`, `jobs stop`, `plan start`, `set start`, `set stop`, `mix start`, `bits start`, `graph put`, `graph start`, `station set`, `station clear`, `handlers policy set`, `capture delete`, `packets delete`, `signature propose`, `signature accept`, `signature reject`, `source ensure`, `source pin`, `source unpin`, `source measure`, `source gain`, and `origin lookup` with `--job`, and `iq read`, which writes a capture into the burst store) are then refused with an `other` error naming the server's pid; read-only commands still work. Use `--node` to change a node that `zeta serve` owns.

Every operation the node applies is appended to `ops.log` in the state directory (rotated at 8 MiB) with its caller's transport (read-only operations are logged without their result); `zeta ops tail --lines N` prints the last N (local only). Every `transcript` event of a `transcribe` job is appended to `transcripts.log` in the state directory (rotated at 8 MiB) with the job, its input, the source and the channel's centre; `zeta transcripts tail --lines N [--job ID]` prints the last N (local only). Every event of every job on a source (what the source's feed carries: every decoded frame, detection, transcript, a `watch`'s and an `identify`'s own events, and an `origin_lookup`'s `origin` answer, whether or not a page or a `stream tail` is open; an `fft`'s rows stay out) is appended to `events.log` in the state directory (rotated at 32 MiB) as `t_ns`, `source`, `job`, `channel`, `center_hz`, `bandwidth_hz`, `start_ns`, `end_ns` and the event itself under `event`; `zeta events tail --lines N [--job ID] [--source ID] [--protocol NAME]` prints the last N (local only), so \"what did ACARS hear overnight\" is `zeta events tail --protocol acars --lines 500` after the fact, and the sites lit for a job are `zeta events tail --protocol origin --job ID`.";

/// What a `register` field holds, for the `classify` and `identify` notes.
macro_rules! register_note {
    () => {
        "`register` holds `provider`, `region`, `status` (`state` `ok`; `partial`, with the queries that failed in `failed`; or `unreachable`, with its `reason`), `as_of` (the register's data date: quote it), `plan` (the band plan segments and their services), `candidates` in rank order (each a `device`, its `site`, `licence` and `holder`, the `geometry` from where it was heard (`distance_km`, `bearing_deg`, `horizon_km`) and the `expected` reception (path `mode`, `level_dbm`, `in_range` `yes`, `no` or `possible`, and the `unknown` inputs), with `related` channels for the top three, and `network_sites`, how many of the answer's sites send the same thing on the frequency: over 1 is a same-frequency network, whose sites the register cannot tell apart, so name the count and the site's `polarisation` instead of naming one site as the source), `area_candidates` (area-wide licences), `matched`, `truncated`, the `assumptions` the estimate used, and `verdict`, the node's reading of the answer against the measurement: `kind` `agrees` (a candidate in range fits the measured width and modulation), `disagrees` (candidates are in range and none fits; the summary names what is licensed and what was measured; in an `identify` verdict whose decoder proved, the comparison is with what was decoded instead: the row's `modulation`, and the width the proving event names as `bandwidth_hz` when it has one, since the first-layer measurement has no label for many signals and under-reads a wide flat one), `unlicensed` (no candidate in range; the summary gives the band's primary services and how many area-wide licences cover it) or `unknown` (the register could not be asked: nothing is known, which is not the same as nothing licensed); `weak` is true when range cannot be bounded (a skywave band, or no position to measure from), and `summary` is one plain line. An `expected` level is the register's estimate from its records and the `assumptions`, never a measurement: say \"expected about -95 dBm\". The node measures only a signal-to-noise ratio, so the verdict does not compare levels. A row's `power_w` is the transmitter's power and `eirp_w` its radiated power, each `null` when the register holds none (a broadcast licence holds only the EIRP: say \"50 kW EIRP\", never \"0 W\"); `knowledge_lookup` and `origin_lookup` rows carry both, with `expected_level_dbm`, and sites that tie on score are ordered by it, strongest first."
    };
}

/// Hand-written sentence appended to an operation's section.
fn note(op: &str) -> Option<&'static str> {
    match op {
        "graph_put" => Some(
            "Fetch the current graph with `graph get`, edit it, and put it back; stage kinds and their params are listed under Stage kinds below and by `schema`. A put that changes only a demodulator's `squelch`/`squelch_db`, its guard's `guard_hz`/`guard_db`/`channel_hz`, its audio (`denoise`, `audio_lowpass_hz`, `deemphasis_us`, `wfm_demod`'s `stereo`), the channel node's `wide_hz` (or `deviation_hz`/`sideband`) keeps the job re-expandable: a later capture change (another rate after a restart) re-expands it with those values carried. Any other change launches the graph as stored from then on.",
        ),
        "job_start" => Some(concat!(
            "`fm_demod` is narrowband FM voice; `wfm_demod` is wideband broadcast FM (50 us de-emphasis, 15 kHz audio; give it a band of about 200000 Hz). It is mono by default; `stereo: true` on its `wfm_demod` node, set with `graph put` and kept through a retune, makes its audio two channels, L and R, for the whole run: decoded L and R while the 19 kHz pilot is present and strong, both carrying the mono L+R otherwise, as receivers fall back (stereo needs about 20 dB more signal than mono for the same audio SNR). `job_context.wfm` holds `pilot_db` (the locked pilot's power against the multiplex's: over -30 dB the pilot is present, under -36 it is gone; a clean station reads -24 or more), `pilot_snr_db` (the pilot's power against the noise beside it: it falls dB for dB as the station weakens; stereo needs it over 35 dB and drops to mono under 30) and `stereo` (true while L and R are decoded); the two figures are `null` when the band is too narrow to carry stereo. It is noted when `stereo` switches and every 5 s, and cleared (`wfm: null`) by a run without `stereo`. Read them together: `stereo: false` with a low `pilot_db` means no pilot (a mono station, or nothing received); with `pilot_db` over -30 and a low `pilot_snr_db` it means a pilot too weak for clean stereo, so the job plays mono until the signal improves. `am_demod` is AM voice (envelope detector, 5 kHz audio, slow AGC; give it a band of about 10000 Hz). `ssb_demod` is SSB voice (2800 Hz audio, slow AGC; `center_hz` is the suppressed-carrier (dial) frequency, not the middle of the voice band, and the band must cover both sides of it: give it about 6000 Hz). All four output 48 kHz audio, mono unless a `wfm_demod` has `stereo: true`: an audio frame's payload is `{\"sample_rate_hz\", \"channels\", \"samples\": N, \"peak\"}`, `channels` 1 here (2 for a stereo `wfm_demod`; an attached `transcribe` downmixes it, a `mix` skips it and lists it in `job_context.mix.skipped`), the samples interleaved and `samples` the per-channel count (a frame without `channels` means 1). `fm_demod` shapes its audio as a voice receiver does (a 3.4 kHz low-pass and a 530 us de-emphasis after the discriminator: `audio_lowpass_hz` and `deemphasis_us` on its node, `null` for the flat audio a decoder wants), and every voice demodulator (`fm_demod`, `am_demod`, `ssb_demod`) takes `denoise: true` on its node to run RNNoise on its audio output (off by default; speech only; one 10 ms frame of delay the frames' spans account for; set with `graph put`, kept through a retune; a `transcribe` attached to such a job skips its own `denoise` stage). `fm_demod` and `am_demod` carry an automatic squelch: it opens 6 dB over the channel's tracked noise floor (never taken above -40 dBFS: with no fixed threshold `floor_db` reads at most -40) and closes after 300 ms under it. While closed the job publishes no audio (an attached transcriber idles; `--then-tail` tails the audio, so an idle channel's tail is empty) and the first block and each change publish a `squelch` event (`open`, `power_db`, `floor_db`) on the job's event stream. The demod node's params set it with `graph get` and `graph put`: `squelch_db` is an absolute threshold in dBFS, -120..=0 (`null`, the default, is the automatic one; a value outside the range fails the job with `squelch_db <v> out of range -120..=0`), and `squelch: false` turns it off. `aprs_decode`'s demodulator runs with the squelch off. `fm_demod` on a channel of a plan (`channel_plans`) carries an adjacent-channel guard: its squelch stays closed while a channel `guard_hz` away is more than `guard_db` (12) dB stronger, so a transmission on ch20 does not open ch59 or ch60. The `squelch` event carries `adjacent_db` and `guarded`; once guarded, the gate stays held until the neighbour falls 2 dB under `guard_db`. The job's `job_context` keeps the last `squelch` event under `squelch` (removed when a run has the squelch off), so a client that subscribes after the gate changed reads it there. `graph put` with `guard_hz: null` on the demod node turns it off. `pocsag_decode` decodes POCSAG pagers (512, 1200 and 2400 baud detected automatically) from a 12.5 kHz FM channel into one event per message (`address`, `function`, `kind`, `text`). `flex_decode` runs multimon-ng on the node's host over a 25 000 Hz channel at the measured centre, one event per page like POCSAG; its events carry `access: 'restricted'`. `rds_decode` reads the RDS data of a broadcast FM station (give it the same 200000 Hz band as `wfm_demod`) into events carrying `pi`, `ps` (station name) and `rt` (radio text) as they complete; the first event takes a few seconds. `adsb_decode` decodes Mode S / ADS-B at 1090000000 Hz with `bandwidth_hz` 2400000 (anything under 2000000 is refused) by running dump1090 on the node's host; one event per decoded message (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`, `track_deg`), position and identity arriving in separate messages. When the host has no dump1090 the node's startup handler report names it (`zeta: handlers: dump1090: missing (apt install dump1090-fa; <url>) — modes: adsb_decode`; the one command is chosen by the package managers on the host's `PATH`, in the order `apt`, `dnf`, `brew`, `snap`, `flatpak`, from the row's `install` entries, and a row's `script` comes first), `modes_list` shows `adsb_decode` `unavailable` with that line as its `reason`, and `job_start` fails with `mode adsb_decode is unavailable: <reason>`. `acars_decode` decodes ACARS (VHF, AM, 2400 baud MSK) from a 10000 Hz channel on 131550000 Hz (worldwide primary), 131725000 / 131825000 (Europe), 130025000 / 130450000 / 131125000 (North America), 131450000 (Asia) into one event per CRC-clean block (`reg`, `flight`, `label`, `text`, `downlink`: true for a downlink); a downlink whose text is a position report (labels H1, 80, 16, 3L, QQ) also carries `lat`, `lon`, sometimes `alt_ft`, and `position`. `vdl2_decode` decodes VDL Mode 2 (VHF data link, D8PSK) from a 25000 Hz channel on 136975000 Hz worldwide (136725000 / 136775000 / 136825000 / 136875000 in Europe) by running dumpvdl2 on the node's host: one event per AVLC frame keyed on the ICAO address `icao` (`addr_type` tells an aircraft from a ground station), with the ACARS fields (`reg`, `flight`, `label`, `text`) and a `position` on ADS-C reports; pin the radio first beside the ACARS jobs (`source_pin`, then `plan_start`, then the job, or it is a `conflict` naming them). `hfdl_decode` decodes HFDL (HF data link) on an HF source such as a KiwiSDR by running dumphfdl on the node's host: `center_hz` is the listed dial frequency from the operator's frequency list (dumphfdl's `etc/systable.conf` upstream), not a raster, `bandwidth_hz` 6000, the signal 1440 Hz above it; a ground-station squitter within 40 s proves the channel. `ais_decode` hears one AIS channel (A or B) and emits one event per message, vessels as subjects by MMSI; `plan_start ais_vhf` runs both. `rs41_decode` decodes a Vaisala RS41 radiosonde anywhere in 400050000-406000000 Hz (`dfm_decode` and `m20_decode` a Graw DFM and a Meteomodem M20 alike) by running the rs1729 decoder on the node's host over NBFM audio: start it on the sonde's measured centre (its frequency varies by flight; never a raster) with `bandwidth_hz` 12500, one sonde per job, and each event carries `id`, `position` and the frame's GPS time as `t_ns`. `deviation_hz` is the FM peak deviation for `fm_demod` and `wfm_demod` (100-75000 Hz; `null` means the kind's default: 5000 Hz for `fm_demod`, right for standard voice FM, use 2500 for narrow 12.5 kHz channels, and a channel plan that carries a `deviation_hz` (`channel_plans`; UHF CB 2500) gives `plan_start`'s `fm_demod` that one when you pass `null`; 75000 Hz for `wfm_demod`, right for broadcast FM). Every other kind takes `null`. `sideband` is `usb` or `lsb` for `ssb_demod` (`null` means `usb`; amateur convention is `lsb` below 10 MHz, `usb` above); every other kind takes `null`. On the CLI, omit `--deviation-hz` and `--sideband` for `null`. `scan` sweeps a band wider than one capture: the node steps the source across it in captures of its whole `max_bandwidth_hz`, each step advancing by the capture's usable part (`usable_fraction` in the source's capabilities: 0.9 on a SoapySDR device, `usable=F` in its args to change it; a `--source-node` takes its remote's; 1.0 on the other kinds), takes `dwell_ms` of samples on each (20 ms unless `job_start` asks otherwise, 1-5000; at least four FFT frames and never more than the source's ring holds; `scan.dwell_ms` in its context is the dwell in force, `scan.dwell_ms_asked` the one asked when they differ), and emits `spectrum` rows whose `center_hz` is that step's (frame `channel` `scan/<step index>`), wrapping until stopped. With `average` N (1-64, default 1) each row is the mean power of its step's last N sweeps of the same grid (an even step with the even sweeps of its centre; N is lowered to what an eighth of the node's ring budget holds for every step's rows, and then `scan.average` is the one in force and `scan.average_asked` the one asked), and every row carries `sweeps`, how many it averages so far (1 without averaging; the averages start over when the scan restarts or its grid changes). The look is the operator's choice: many quick looks averaged (20 ms × 16) lower the noise ripple as one 320 ms dwell does while each step is still revisited every sweep; a long look (250 ms × 1) sees a faint steady signal in one dwell; the default (20 ms × 1) is the fastest sweep. With `bits` (one of the source's `capabilities.depths[].bits`; `null`, the default, is the source's own `depth`) the scan holds the source's link at that sample depth for its run, steps at that depth's `max_bandwidth_hz`, and puts the source's own depth back before anything else is granted the source; `scan.bits` and every row's `bits` say the depth in force (on a source with depths). Such a scan does not give way (below). A lossy depth (8 bits on a B210) sweeps faster, but a weak signal beside a strong one in the same capture is lost in its quantisation noise: it is for a coarse survey of occupancy, never for faint signals, and never the default. A `bits` the source does not carry is refused naming its depths. Alternate sweeps shift the step grid down by a quarter of the stride, `step_hz` (the odd sweep has at most one more step), so a step's centre and edges in one sweep fall a quarter of the way into a row of the next and every frequency is seen clean in one of two sweeps, whatever edge mask is under `(1 - u) / 2 + u / 8` for the usable fraction `u` (0.1625 at 0.9, 0.125 at 1.0); `scan.alternate` in the job context gives `shift_hz` and `steps_shifted` (`null` for a one-step span). A scan on a local hardware receiver masks each row's centre notch (`notch_hz`, 50000 Hz), and every scan masks each row's outer edges that the usable fraction leaves unused (`edge_mask`, `(1 - usable_fraction) / 2` of the bins each side: 0.05 on a SoapySDR device, none where the whole capture is usable), never less whatever `edge_mask` its stored node says. A row with masked bins carries `masked: [[lo, hi], ...]` (bin index ranges, `lo` inclusive, `hi` exclusive): those bins are no data, not low power; their `q` is still present. `scan.mask` reports the values in force (a step narrower than four notches gets no notch; a span no wider than one capture's usable part is one step, asking for the span divided by the usable fraction so the unused edges it masks lie outside the span; it does not alternate and masks no more than those edges, since nothing could fill a wider mask in), and `graph get`, edit the `fft` node's `notch_hz`/`edge_mask`, `graph put` changes them (keep `edge_mask` under that bound, 0.1625 on a SoapySDR device, or the two grids' masks overlap). Of the scan's stored `fft` node only `notch_hz`, `edge_mask`, `dwell_ms`, `average` and `bits` are read (`graph put` changes the look as it changes the mask; the scan restarts): its `size`, `window` and `rows_per_s` are information only (the scan derives them per step), and a `graph put` whose graph has no `fft` node is re-expanded with the default mask on the next launch rather than refused. A stepped scan time-shares the radio but never blocks anything: it is secondary and gives way. While it steps, `source_list` lists it among `holders` and shows the step it is on as `current`; any other request on the source (a rigid or elastic `job_start`, `source_ensure`, `source_pin`, `job_retune`, `plan_start`, `set_start`, `source_measure`, another stepped scan) is granted as if the scan were not there, and the scan stops tuning and waits, no job stopped: it stays `running`, `job_context.scan.waiting` says who holds the source (`source <id> is held by <kind> <id>, ...`, `pin` for a pin; `null` while it steps), `timing` keeps its last figures, and `source_list` lists it under `scans_waiting`, not `holders`. It takes the source back by itself, starting a new sweep, once no rigid job, no pin and no other scan holds it, and then stops the source's elastic jobs as its start does (so an elastic job started beside a running scan is granted and ends at once). A stepped scan started on a source a rigid job or a pin holds returns its id and waits the same way; with `replace` naming the holders it stops them and steps at once. To decode where a scan runs, start the decoder: never stop or `replace` the scan for it. The exception is a scan whose `bits` is a lossy depth (or another depth than the one in force): it changes the samples every holder gets, so it is still the source's sole holder, refused as a `conflict` naming the holders on a held source, and a job started beside it is a `conflict` naming it. A source that sweeps for itself is the exception (next paragraph). A stepped scan's `job_context` holds `scan.steps` (how many captures one sweep takes), `scan.step_hz` (the stride), `scan.rate_hz` (each capture's width; both rewritten at a sweep's start when the source's capture, usable fraction or depth changed, a rate cap measured after the scan started, or a scan's `bits` taking another depth's width), `scan.dwell_ms`, `scan.average`, `scan.bits`, `scan.alternate` and `scan.mask`: report step counts from it, never from guesswork. `scan.timing` is the cost of the pace, rewritten about once a second: `sweep_ms` (the last whole sweep), `step_ms` (the median step), `tune_ms` (`median` and `max`), `slow_tunes` (tunes over ten times the median: the AD9361's calibrating tunes on a B210), `skipped` (steps whose samples did not arrive; both counted since the last write), `fft_wait_ms` (time the loop waited on the FFT), `hz_per_s` (the span over the sweep) and `at_ns`. Report how long a sweep takes from `scan.timing.sweep_ms`, never from step counts or the dwell: the radio sets the pace (a step is its tune and the dwell's samples, the FFT running beside the next tune, and the step is the source's whole capture, not a fixed width), and a scan started before the source's rate cap was measured sweeps at the asserted width until the cap arrives. A source delivers only settled samples after a tune (the first block of a new centre is taken after the radio settled), so a step's first row is as good as its later ones; a `--source-node` source takes its far node's, and a KiwiSDR drops the frames still in flight at the old frequency. A source whose `capabilities.sweep` holds the span (`source_list`: `null` for a source that cannot sweep for itself, else `min_hz`, `max_hz`, `bin_hz_min`, `rows_per_s_max`, `calibrated`; a KiwiSDR's waterfall is the first) sweeps for a `scan` itself: `context.scan.method` is `source`, the scan takes no lease and no IQ, leaves the source's capture and its holders alone, runs beside any demodulator on it, and is listed under `sweeps` in `source_list`, never under `holders`. `method` `stepped` forces the stepped scan; `source` never steps; `null` lets the node choose. `bin_hz` asks the source for that bin width (the source picks its zoom and step count from it; the default is the coarser of 6000 Hz and the source's `bin_hz_min`); `dwell_ms` and `average` apply to the source's rows as they do to a stepped scan. `bits` beside `method: source` or beside `bin_hz` is refused (`bits applies to a stepped scan`), and so is a non-empty `replace` beside `method: source` (`replace applies to a stepped scan`). `replace` on a scan is honoured only if the scan has to step: a scan the source sweeps for itself stops nothing; if a refusal makes the scan step, the named jobs are stopped before it takes the radio. Pass `replace` only when a `conflict` named those holders. A stepped scan's `context.scan.waiting` is the reason it waits for its source, `null` while it steps. A `source` scan has `context.scan` keys `method`, `steps`, `step_hz`, `bin_hz`, `dwell_ms` (`dwell_ms_asked`), `average` (`average_asked`), `alternate`, `mask`, `calibrated`, `waiting` and `timing` (the stepped scan's keys, no tune time, and `rows_per_s`: the rows a second since the last write, the figure to read for a one-view sweep; rewritten at most once a second, re-aims included); while it is `waiting` it holds only `method`, `mask`, `calibrated` and `waiting` (the plan's keys arrive when the source grants the sweep, `timing` after its first sweep); it has no `rate_hz` and no `bits`, and the source masks its own rows (the first pixels of each view are masked as `masked`). Its rows carry `calibrated`: true means absolute dBm at the antenna, which a local radio's rows are not (never compare levels across the two). A sweep the source refuses at a first start makes the scan step instead and `context.scan.fallback` says why; when the source's IQ channel is held that is the usual `conflict` carrying the source's reason, as the stepped scan cannot have the radio. A refusal while another scan already sweeps the source (a KiwiSDR has one waterfall connection, so the page's own scan of its tab holds it) is the start's error instead, naming that scan: re-aim it with `job_retune`, or ask `method: stepped` to step the source's IQ channel. A scan that ran as `source` and is restarted (a node restart, a `graph_put`, a look change) never falls back: if the source refuses then, it stays up with `context.scan.waiting` set to the reason, asks again after 1, 2, 4, 8 and 16 s (then every 16 s), and clears `waiting` when granted. A source's sweep start is bounded at 5 s (`sweep start timed out` reads as a refusal). On a KiwiSDR a sweep takes one more user slot while it runs (its waterfall socket) and none otherwise, and paints the whole band about once a second at the default; a second scan on one KiwiSDR runs stepped or is a `conflict` (one waterfall connection). A `--source-node` source does not forward a far source's sweep, so a scan on it steps; run the same version on both ends of a `--source-node` pair and upgrade them together. `identify` is the thorough answer to \"what is this?\": started like a decoder (`job_start` with `kind: identify` on the box's centre and band), it holds a rigid lease on that band, measures it for 1000 ms (layer 1), scores every mode row against it and tries each ready row that fits, one at a time in score order, as an ordinary child job on the same band (`job_list` shows it, `source_list.holders` shows both while it runs) for the row's `identify.dwell_ms` (5000 ms for APRS, POCSAG and the sonde rows, 10000 for RDS and ACARS, 15000 for ADS-B and VDL2, 40000 for HFDL). Each row it considers publishes one `candidate` event on its event stream: `mode`, `score`, `outcome` (`proved`, `no_frame`, `skipped_band` for a row wider than the band, `unavailable` for a row whose program is missing or whose job would not start), `evidence`, `summary` (`<mode>: <outcome>`) and `job` (the child's id, `null` when none ran). A proof ends the job `stopped` with `job_context.identify` = `{\"verdict\": {\"mode\", \"label\", \"summary\", \"evidence\", \"job\"}, \"attempts\": n}` and leaves that mode's job running under the id in `verdict.job`, already decoding; stop it with `job_stop` when done. A list exhausted with no proof ends it `stopped` with `verdict` `{\"mode\": null, \"label\": <layer-1 name>, \"features\": <the layer-1 record>}`: report that as unknown with those measured features. Either way the verdict is also the stream's last event, one `verdict` (`mode`, `label`, `center_hz` the measured centre, `job`, `allocations`, `summary`: `<label> at <MHz>: <the decoder's summary>`, or `unknown at <MHz>: <label> <features>; register says ...` as a watch's `unknown` reads). Both verdicts and that event also carry `register`, the register's whole answer for the measured band (`null` when no provider describes it, as without `--knowledge-acma`), asked once per run: `allocations` and `origins` are read from the same answer, and with one an unknown's summary ends with the register verdict's `summary` in place of `register says ...`. ",
            register_note!(),
            " Stopping an `identify` job stops the child it is trying; a source that faults fails it with the fault as its reason. `dwell_scale` (`identify` only, 1-6, `null` is 1) multiplies each tried row's `identify.dwell_ms`, for a signal whose frames come only now and then (a weak sonde gives one in tens of seconds); no row then waits over 60 s unless its own dwell is longer, and the run takes as many times longer for every row it tries. A `watch` passes 1, 3 and 6 on its first, second and later tries of one signal; it identifies one signal at a time and starts nothing else meanwhile, so a long retry holds back every other new signal until it ends. With `--then-tail`, an `identify` that ends with a verdict during the tail is a success: the candidates are printed and the proved job (`verdict.job`) is stopped, or left running with `--keep` (`{\"stopped\": false}`). `watch` keeps an eye on a band and decodes what appears in it (with `modes` on its `detect` node it decodes every channel of the span; `modes`, `max_decoders`, `hold_ms`, `cooldown_ms`, `confirm_ms`, `seed_known` and a `lost` event's `why`, one of `gone and silent`, `not seen again`, `unproven`, `relaunch failed`, are described in \"Decoding every channel of a band\" in the field craft): `job_start` with `kind: watch` on the band (400-406 MHz is `center_hz: 403025000`, `bandwidth_hz: 5950000`; put the centre off any channel of interest, the receiver's DC spike sits there). It holds a rigid lease on the band like `identify` and runs `fft → detect` over the capture: `detect` publishes a `signal` event (`state` `new` or `gone`, `center_hz`, `bandwidth_hz`, `snr_db`, `first_ns`, `last_ns`) for each run of bins that persists (at most 10 `signal` events a second, the rest delayed), and each new signal no other job's band overlaps gets an `identify` run on its measured centre, one at a time, best-scoring first, on the band the best-scoring fitting row expects (12500 Hz for the sondes). A proof leaves the decoder running: inside the watch's capture with `decode_on: capture` (the default; one radio does both), or with `decode_on: free` on a free source other than the watch's (`source_free`), the ids in `prefer` first, back in the capture when none takes it. `on_unknown: capture` (with `capture: {bursts, window_ms, pre_ms, post_ms, timeout_ms}`) captures an unknown signal's bursts once per run as a `capture` child, publishing `capturing` and `captured`; `job_context.watch.signals[].captures` lists them. A signal no row proves is identified again after 60000 ms while it persists, at most 3 times; a decoder whose signal is `gone` and that has sent no event for 120000 ms is stopped (a landed sonde). The watch's own events, `protocol: watch`, go on the same event stream: `state` `identifying` (`signal`, `job` the identify run), `decoding` (`mode`, `job`, `source`, `placed` `capture` or `free`; the `summary` says why it fell back to the capture), `unknown` (`features`, the layer-1 record: report it as unknown with them) and `lost`, each with `summary` and `t_ns`. `job_context.watch` holds `signals` (each with `tries`, `next_retry_ns`, `handled`), `decoding` (`mode`, `job`, `source`, `signal`, `since_ns`, `placed`) and `current` (the identify run in progress, else `null`). `detect` also honours a row's `masked` ranges (those bins take no part in a run) and keeps to the watch's own band: in a wider capture (a pin, a shared one) the bins outside it take no part either, so a narrow watch announces only signals inside its box. Tune it with `graph get`, edit the `detect` node's params, `graph put`: `threshold_db` (6, over the row's median bin), `energy_db` (30, the least sum over a run's bins of dB over that median), `persist_ms` (3000; a signal also needs 3 sighting rows before `new`), `width_hz` (`[4000, 30000]`), `dc_mask_hz` (25000), `edge_mask` (0.05), `spurs_hz` (`[]`), `gone_ms` (30000), and the policy `decode_on`, `prefer`, `sink`, `sondehub`, `on_unknown` (`report`, the default, or `capture`) and `capture`. `sink` (`{kind, params}`, or a list of them) appends a node of stage `kind` with `params` to every decoder the watch keeps, fed by its events: `kind` is a stage with one `event` input and no outputs (`sink_sondehub` today; the schema's `stages` lists every stage's inputs and outputs), each sink its own node (its id the kind, `<kind>_1` and on for more of one kind); without one nothing leaves the node. `sondehub` (`{callsign, lat, lon, alt_m, batch_ms, url}`, the last two optional: 5000 ms and SondeHub's telemetry endpoint) is shorthand for `sink: {kind: sink_sondehub, params: <it>}`; giving both is refused. Each sink is checked like the detect params: a bad one is refused at `job_start` and `graph put`, before anything starts or restarts, naming `sink.kind` (not a registered stage, or not a sink) or `sink.params.<field>` (`sink[<i>].` in a list; the `sondehub` shorthand's errors read `sink.params.<field>` too). If the sinks cannot be added to a kept decoder it runs without them (they restart and fall back as one set; the `decoding` event's `summary` says `sink refused: <why>` and its `decoding` entry carries `sink_error`); one that cannot be relaunched at all is `lost`. `sink_sondehub` can be put on any sonde job with `graph put` (an `event` edge from the decoder's event node): it sends the `rs41`, `dfm` and `m20` events that carry a `position` (with a numeric altitude), a `datetime` and a `frame` to SondeHub in a `PUT` every `batch_ms` (and what it holds when the job stops or restarts), and `job_context.sondehub` holds `sent`, `failed` and `last_error` (a failed batch is counted and dropped, not retried). The identify runs and their trial jobs are the watch's (`owner` is its id in `job_list`) and leave no history; the decoders it keeps are ordinary jobs. `job_stop` on the watch stops its identify run and every decoder it keeps, and so does a watch that ends `failed`; a `graph put` on it or a node restart re-adopts them instead, stopping any whose signal does not reappear within `gone_ms`; a re-adopted decoder whose sinks differ from the watch's (a `graph put` that changed `sink` or `sondehub`) is restarted with the watch's. A kept decoder stays its row's expansion, its sinks stored beside it, so a retune or another capture rate re-expands it with them. A `job_start` or `graph put` with an `edge_mask` outside 0..0.5, a `width_hz` not ordered `[min, max]`, a bad `sink`, `sondehub` (naming `sink.params.<field>`) or `capture` (naming `capture.<field>`) or a `prefer` naming a source not in `source_list` (`prefer[<i>]: unknown source <id>`) is refused naming the field, before anything starts or restarts. `transcribe` is not started on the radio: `job_start` refuses it; use `job_attach`. `capture` writes a signal's bursts to disk as SigMF windows (`burst_capture`: `bursts`, `window_ms`, `pre_ms`, `post_ms`, `timeout_ms`, `whole`; a signal already on when it starts, which makes no burst edge, is cut into back-to-back windows after 500 ms, `job_context.capture.whole_auto` true) and ends `stopped` with `job_context.capture`; one started by hand classifies its first window at its end into `job_context.capture.classified` (`labels`, `tried`, `allocations`). When the job ends with a features-only signature proposed from its captures (confidence 0.3), `job_context.capture.proposal` holds the proposal's id (`signature_show` gives its name, confidence and evidence): it waits for the operator (`signature_list`, `signature_accept`). `kind` also takes the id of any ready mode row (`modes_list`), an operator's own rows included; an id that is neither a built-in kind nor a row's fails with an `unknown` error (`field: \"kind\"`, `known` the built-in kinds and every row id), a row that is not ready with `mode <id> is <state>: <reason>`. `elastic: true` (spectrum only) makes the job follow the source's capture instead of holding it: it never conflicts, is relaunched on every retune and is listed under `elastic` in `source_list`; a rigid request inside the capture elastic jobs follow is granted on it without a retune, one outside it retunes the source; the web page's own spectrum job is elastic. `replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder. An identical rigid start (same source, kind, band, `deviation_hz`, `sideband` and handler) on a running job is refused as `duplicate` naming that job; stop it or use it. The same mode on the band with another handler is another job. `m17_decode` decodes M17 digital voice (4FSK 4800 baud) on any 2 m or 70 cm channel at the measured centre, 12 500 Hz band; its audio is ordinary job audio (play it, or `job_attach` transcribe), its events name the stream's callsigns; it needs `c2dec` (package codec2) on the node's host.",
            " A mode whose row lists `voices` (`modes_list`: a DMR row's `slot1` and `slot2`, two conversations on one channel) decodes every voice at once, each on an audio stream of its own with the voice's role, beside the job's main audio stream (no role), which the play button, the mix and a plain `job_attach` read. `audio_policy` says what the main stream carries: `first_active` (the call that started first, until it ends; a call that began meanwhile never joins it; `null` is this), `follow` (that call, then whichever is still going when it ends, from that moment) or `all` (every call, mixed). It is set at the start only: the job's record keeps it as `audio_policy`, so a restart or a retune resumes it. A second start on the same channel with another policy is refused as identical: to change it, start the job again with `replace` naming the running one. On any other kind a policy is refused, `audio_policy: <kind> has one voice`. A set's `job_start` member takes the same `audio_policy` (its `set_list` summary ends `audio_policy <name>`). On the CLI, `--audio-policy first_active|follow|all`.",
            " A mode may have more than one handler (`modes_list`'s `handlers`: the row's own, `builtin` or its program, and each handler file's), all decoding into the row's events; a row with no decoder of its own (`decoder_kind` `null`) has only its handler files'. `handler` names the one to run, an id from the mode's `handlers`; `null` takes the handler policy's choice (`modes_list`'s `handler`; `handler_policy_set`). A row with no decoder of its own and no ready handler is refused as `mode <id> is unavailable: <reason>`, its `modes_list` reason. Name one only when the operator asks for it or two jobs must differ (the same mode on one band by two handlers is two jobs); otherwise leave it `null`. `needs` lists the stream kinds the job must output, among `event`, `packet` and `audio`: `[\"packet\"]` is how a job to capture is asked for, and the policy passes over a handler without them before it reads any ranking. A named handler lacking a needed output is refused as `<mode>: handler <id> outputs no <kind>`. Read which handler a job runs from its record's `handler` (`handler_named` says whether it was asked for), and what it outputs from that handler's `outputs`, never the mode's. Its events carry `handler`, and a packet capture of it records `handler` in its `packets_list` entry and in each packet's comment (`handler=<id>`). On the CLI, `--handler ID` and `--needs packet[,audio]`.",
            " `beside` names a running job of a mode and starts a sibling on the same band by another handler, to compare two decoders on one signal or to capture packets from a handler that gives them when the running one gives none. `kind`, `source`, `center_hz` and `bandwidth_hz` must equal that job's (its record's `tuning`), `deviation_hz` and `sideband` must be `null`, `elastic` false and `replace` empty, and `handler` is required: without one the start is refused naming the free ids, `beside needs a handler: one of <ids>`. The named job must be running, not attached, not owned by a controller, not elastic, and of a mode row. A handler the group already runs is a `duplicate` naming that member, `handler <h> already runs beside job <beside> as job <member>`; a job outside the group running that handler on the band is the ordinary `duplicate`. Nothing is tuned and nothing is stopped. Both records carry `group`, the id of the job the first start was beside (the first job gets it once its sibling has launched), so `job_list` shows who runs together. `job_stop` stops one job and leaves the others. Read each job's events and compare. On the CLI, `--beside JOB --handler ID`: the kind, `--source`, `--center-hz` and `--bandwidth-hz` are read from that job when left out, and a value given is sent as given. A node rolled back past this build resumes no jobs, as its records carry `group`.",
            " `choose` climbs the proof ladder for its band and takes no lease, so `replace` must be `[]` and `beside` `null`: step 1 classifies the band over the newest `budget.slice_s` seconds of the source's ring at once, as `classify` does, and step 2 writes that slice into the burst store as a trial capture (`capture_list` shows `trial: true`; pinned while the job runs) and replays it through the answer's `could_prove` rows, `budget.concurrent` at a time within `budget.trial_s` (`choice_policy_get`), each judged by its row's identify rule; the first decode that passes settles it, `settled: proved`, `step: 2`, the step-1 answer its `runner_up`. Step 3 tries the rows still in `could_prove` on the live band, one at a time in score order within what is left of `budget.trial_s`, each as its row's job on the measured centre for the row's dwell: under the lease setting `share` only where a running capture already holds the row's band untuned (else `not tried: no running capture covers it`), under `widen` also, for a band outside the capture, after growing a capture rigid jobs hold to hold it, the holders keeping their bands and restarting on it (the answer's `evidence` says `widened the capture to <centre>/<width> Hz for a trial` when the capture moved); it never stops a job, retunes a held radio away or tunes an unheld one. A live trial gives way: its lease is no holder a request must respect, and any foreground request that changes the capture (a `job_start`, a `job_retune`, a `source_ensure`, a pin, the page moving its view, a scan) is granted as if it were not there, the trial job stopped and its row noted `not tried: a foreground request took the capture`. Its `choose` field: `up_to` (1-3, `null` the surface's default, cut to the surface's cap with `not tried: capped at step <n> for <surface>` in `not_tried`), `surface` (`menu`, `watch`, `agent` or `client`; `null` is the caller's), `start` (start the final answer's job on the source when it ends) and `transient` (a short-lived job: never resumed after a restart, its record dropped as soon as it stops, and left out of `job_list` unless it is asked with `all: true`, `zeta job list --all`; `job_context` and `job_stop` still take its id). A policy `slice_s` under 1 s skips step 2 with `not tried: the policy's slice_s is under 1 s`, a ring holding under 1 s once its oldest 100 ms are set aside with `not tried: the ring holds under 1 s once its oldest 100 ms are set aside`, a slice the trial quota cannot hold `not tried: the slice exceeds the trial quota`, and a row the budget leaves no time for `not tried: budget`; layer 1 measuring nothing skips the trials with `not tried: layer 1 did not measure the band`. Its trial jobs (replays and live trials) are its own (`owner`): `job_stop` of one is refused while it runs, stopping the `choose` stops them, and they leave `job_list` once they end; with `start`, a step-3 proof's job is kept running as the started job, an ordinary job from then on. Its events are `choice` (`step`, `choice`) when step 1 answers and whenever the answer changes, then `verdict` (`choice`); `job_context.choose` holds `choice`, `slice`, `trials` (`{step, row, job, outcome, evidence}`, `outcome` one of `proved`, `no_frame`, `unavailable` or `stopped`) and `verdict`, and with `start` `started` (the job) or `start_error`. It ends `stopped`, a `cannot_tell` too (a source with no ring: `source <id> is not capturing`). Report a `best_guess` as what to listen with, never as an identification.",
        )),
        "source_ensure" => Some(
            "`replace`: job ids that hold the source and may be stopped to make room; they are stopped under the source's gate right before the request, so nothing can take the radio in between. A `conflict` lists the holders; decide whether those jobs may stop, then retry with `replace`. The page's elastic job is never a holder, and a stepped scan never makes a conflict (it gives way and waits, see `scan` below), so there is no need to name one. `widen: true` moves a held source's capture to the band (wider, or shifted) when every holder's band and the pin's still lie inside it: a `conflict` lists `widen the capture to <centre>/<width> Hz` when the radio could capture both the request and what it holds. The source is retuned and every job holding it restarts on the new capture under its id: a short gap in each stream, each decoder's state lost, an external decoder's process started again, a transcription or other attached job relaunched on its input. That is the operator's decision, as a stop is: through `ask` it needs the run to allow stops, else it is refused as `not_allowed`; report the option, do not retry. A widened capture lasts until the node restarts (the jobs resume on their own capture); `source_set_capture` makes the wide capture the source's habit. On the CLI, `--widen`.",
        ),
        "source_fit" => Some(
            "The way back from a widened capture: the source is retuned to the narrowest rate that holds the jobs on it (and its pin) and each of them restarts on it under its id, with the same short gap and lost decoder state as a widen. Use it when the wide view is no longer wanted and the host should work less. Restarting the holders is the operator's decision: through `ask` it needs the run to allow stops, else it is refused as `not_allowed`; report that, do not retry. On the CLI, `zeta source fit --source <id>`.",
        ),
        "source_set_capture" => Some(
            "How wide the source's capture is when a job or a set tunes it free. `fit` (the default) takes the narrowest rate that holds the job or the set's bounding band: the least CPU and memory. `widest` takes the source's whole `max_bandwidth_hz` about the same centre, so the page can zoom out to the radio's full instantaneous width beside the running jobs and a later job megahertz away fits without a restart; the source then runs at its full rate all the time, which costs CPU (the channel bank) and memory (the IQ history) in proportion. It applies from the next time the free source is tuned for a job (stop and start the set, or restart the node); it never moves a capture jobs hold (`source_ensure` with `widen` does that). Persisted. On the CLI, `zeta source capture --source <id> --policy widest`.",
        ),
        "source_pin" => Some(
            "Hold the source at a band without a job: a rigid holder that is not a job, so `replace` cannot name it and a `conflict` offers `unpin source <id> (source_unpin)`. A pin retunes the source to its band (the page's spectrum follows) unless a decoder holds the source inside that band, when it joins the current capture. Persisted; restored before anything else on a restart. Put `center_hz` between two channels of the grid you listen to so the receiver's DC spike falls in a guard band (the UHF CB plan's midpoint 476918750 Hz with 1000000 Hz holds all 80 channels).",
        ),
        "station_set" => Some(
            "A node-level record, set once: the position, region and antennas the node listens from. The ACMA provider's distance and reach, `sink_sondehub`'s defaults and the page's home read it. `antennas: null` takes the defaults (a 0-30 MHz dipole and a 30 MHz-6 GHz discone, both 5 m up); an antenna is `{id, kind, min_hz, max_hz, height_m, azimuth_deg, elevation_deg}` with `kind` one of `discone`, `dipole`, `skyloop`, `vertical`, `yagi`, `dish`, `other` and the two angles `null` for an omnidirectional one. A bad value is refused naming the field (`lat`, `antennas[<i>].<field>`), with nothing written. On the CLI, `--region` defaults to `AU` and each `--antenna KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]` adds one to the defaults (its id is the kind, numbered when taken); a declared antenna wins over a default on its own band. `station_get` answers the station or `null`; `station_clear` forgets it.",
        ),
        "capture_list" => Some(
            "The captures the node's burst store holds (written by `capture` jobs), newest first: each record has `id`, `source`, `center_hz`, `bandwidth_hz`, `sample_rate_hz`, `start_ns`, `end_ns`, `bytes`, `features` (the window's layer-1 record), `signal` (as a watch detected it, else `null`), `job`, `watch`, `allocations`, `pinned` and the two paths on the node's host. `signal` narrows the list to the captures whose band overlaps that band (`{center_hz, bandwidth_hz}`); `null` lists every capture. On the CLI, give `--center-hz` and `--bandwidth-hz` together to narrow. Through `ask` (the agent transport) only the newest 20 are returned, each without `allocations` and the two paths (`capture_show` has them), with `count`, how many the store holds.",
        ),
        "capture_show" => Some(
            "One capture's record and its SigMF `meta` as written. `meta_path` and `data_path` are files on the node's host, for a local caller; a remote caller fetches the same files from the node's HTTP port, with the bearer, at `GET /captures/<id>.sigmf-meta` (`application/json`) and `GET /captures/<id>.sigmf-data` (`application/octet-stream`, `cf32_le`); another name or an unknown id is 404. An unknown `capture` is an `unknown` error (`field: \"capture\"`, `known` the store's ids): take ids from `capture_list`, never invent them.",
        ),
        "capture_classify" => Some(
            "Run the classifiers over the capture's whole window as `classify` does over the ring (the record's stored `allocations` take part): `{labels, tried, allocations, origins}` (`origins` is `origin_lookup`'s answer for the capture's band and source, scored on the measured layer-1 label; a hand `capture` job's `job_context.capture.classified` carries the same). A `no_frame` does not prove the protocol absent. A window shorter than a row's `identify.min_span_ms` is `skipped_span`.",
        ),
        "capture_delete" => Some(
            "Delete a capture and its files. A pinned capture is refused (`capture <id> is pinned`). Deleting is the operator's decision: through `ask` the run needs `allow.delete_captures` (`zeta ask --allow-delete`; on the page, `allow deletes on`), else the call is refused as `not_allowed`: report that, do not retry. Never delete a capture the request did not name.",
        ),
        "signature_list" => Some(
            "A learned signature is what the node, or the operator by hand (`signature_propose`), proposes about a signal no mode row knows; it waits as `proposed` until the operator accepts or rejects it. `state` `null` lists every record. Take ids from here, never invent them.",
        ),
        "signature_show" => Some(
            "One record by `id`. An unknown `id` is an `unknown` error (`field: \"id\"`, `known` the store's ids): take ids from `signature_list`.",
        ),
        "signature_propose" => Some(
            "On the CLI, `--framing-file FILE` holds the `framing` JSON, the params of the `frame_decode` stage (under Stage kinds), e.g. `{\"sync\": {\"hex\": \"2dd4\", \"bits\": 16}, \"length\": {\"fixed_bits\": 272}, \"crc\": \"crc16-ccitt\"}`; a framing the stage refuses is `other` with `framing: <reason>` naming the field. An unknown `capture` is `unknown`; a capture without a feature record (`capture <id> has no feature record`) or labelled `noise` (`capture <id> is noise: nothing to propose`) proposes nothing. The confidence is 0.3 without a framing and 0.6 with one. Report the proposal's `id`, `name` and `confidence`.",
        ),
        "signature_accept" => Some(
            "Through `ask` without `allow.accept_signatures` the call is refused as `not_allowed`: report the proposal (its `id`, `name`, `confidence` and `evidence`), do not retry. Never accept a proposal the request did not name. The captures it was learned from are pinned: never expired nor deleted. A record not `proposed` is `other` (`signature <id> is accepted, not proposed`). Accepting a framed signature (a `framing` with a `crc`, and a `symbol_rate_baud`) also writes its decoder row, `learned_<id>` (the id lowercased), to `learned_<id>.json` in the operator's modes directory (`--modes-dir`, else `$XDG_CONFIG_HOME/zetasdr/modes` or `~/.config/zetasdr/modes`) and reloads the rows: a frame row of the signature's band, demodulator and framing with the `opaque` parser, proved by one CRC-clean frame, listed by `modes_list` at once. The record's `decoder` names the row and `decoder_file` its file: start it as `kind` the record's `decoder` (use that field; never build the id). A file already there is never overwritten (`decoder_note`: `row learned_<id> already present: kept as is`), so the operator may edit the row freely (a parser, a subject); with no modes directory (`no modes directory: row not written`) or a failed write (`row not written: <error>`) the signature is still accepted and `decoder` stays `null`. A features-only signature writes no row. When a built-in or operator row later proves its band (a `proved` in `classify` or `capture_classify`, an `identify` verdict, or a mode job's first proof), an accepted signature whose band holds the proved centre, the proved band 0.5 to 2 times its bandwidth, is `superseded` (`superseded_by` the row's id; its captures stay pinned) and no longer labels matches; a `learned_*` row never supersedes.",
        ),
        "signature_reject" => Some(
            "Through `ask` without `allow.accept_signatures` the call is refused as `not_allowed`: report the proposal, do not retry. Never reject a proposal the request did not name. The record stays, `rejected`; its captures are not pinned.",
        ),
        "signature_retract" => Some(
            "Takes back a wrong accept on the running node, no restart: the record stays, `retracted` (its provenance kept, `retracted_ns` set), labels no later classify, and its `learned_<id>` row leaves `modes_list` (`decoder_note`: `row learned_<id> removed: the signature was retracted`). A row file that was there before the accept, or that has been edited since, is the operator's and stays, and so does one that cannot be removed: the call still succeeds, and `decoder_note` warns that the row `now outlives its record`; a capture that cannot be unpinned is a warning there too. Its captures are unpinned unless another accepted or superseded record lists them. A record not `accepted` is `other` (`signature <id> is proposed, not accepted`); a retracted one is never accepted again, only deleted. Through `ask` without `allow.accept_signatures` the call is refused as `not_allowed`: report it, do not retry. Never retract a signature the request did not name.",
        ),
        "signature_delete" => Some(
            "Removes a record in any state; an accepted or superseded one's row and pins are undone as `signature_retract` undoes them, and what a retract of it could not undo is tried again. The answer is the record as it stood, but for `decoder_note`, which says what became of its row and carries any warning (a row that `now outlives its record`). Prefer `signature_retract` to take back an accept (it keeps the record). Through `ask` without `allow.accept_signatures` the call is refused as `not_allowed`: report it, do not retry. Never delete a signature the request did not name.",
        ),
        "source_unpin" => Some("Release the pin; the source idles if nothing else holds it."),
        "source_measure" => Some(
            "Link diagnostics (the page's \"Run link diagnostics\" on a source with a `worker`): drop the source's cached rate figures and measure what its link sustains again, each sample depth probed as at a first start, about a minute with the host busy meanwhile. The figures belong to the link's speed (`worker.attachment`, `usb:<bus>-<path>@<speed>`): a radio whose USB link came back at another speed measures again by itself, so this is for a system that stutters after devices were added around the radio. It is the operator's call: run it only when asked, never with `replace` unless the operator agreed to stop those jobs. While it runs the source is faulted `measuring the link again` and `worker.starting` reports the probe's steps; the new worker's hello heals it and relaunches what failed. The new figures govern tunes at once (`depth`), show in `worker.link_measure` (`max_bandwidth_hz`, `depths`, `at_speed`) and are cached for the next start; `capabilities` is set once a run: the first hello of the run that differs from it replaces it, and later ones leave it, so read `depth` and `worker.link_measure` for the figures in force. Report each depth's rate against the one before. A source without a worker (a recording, a KiwiSDR, another node) answers `source <id> has no worker`; a source a decoder, a scan or a pin holds is a `conflict` naming them (the page's elastic spectrum never holds it).",
        ),
        "source_free" => Some(
            "Ask which radio could take a band before starting a job on it, then start the job on one of the `free` ids. For a `scan`, a source whose `capabilities.sweep` holds the span can be used while a job holds it, whatever `source_free` says. A source a stepped scan holds is free: the scan gives way. A free source is a suggestion, not a reservation: nothing is leased, and the next `job_start` may still conflict. Its `free` list is in id order, which is no preference: the request's default radio and preference order (`## This request`) come first. A source on another node (`--source-node`) can look free here while that node runs its own jobs on it; that surfaces as a `conflict` on `job_start`.",
        ),
        "source_set_gain" => Some(
            "Set the source's overall receive gain live (`gain_db` in dB within `source list`'s `gain.min_db..=max_db`), or `null` for the driver's automatic gain where `gain.automatic_available`. Persisted: it is re-applied when the node restarts and wins over `gain=` in the device args. A source without adjustable gain (a recording, a KiwiSDR, another node) answers `source has no adjustable gain`. On the CLI, `--auto` passes `null`.",
        ),
        "job_attach" => Some(
            "Attach a processing chain to a running job's output instead of the radio: `transcribe` reads the audio of a job whose kind emits audio: the voice demodulators and any mode row whose `outputs` lists `audio` (`m17_decode`). It cleans it (denoise, voice gate) and emits one `transcript` event per utterance (`text`, `language`, `start_ns`, `end_ns`) plus `vad` events as speech starts and stops; it holds no lease, follows its input through a `job_retune` or a `graph_put` (relaunched under its own id) and ends when the input job stops. `language` is a BCP-47 code (`null` means `en`; the backend's languages are in `GET /stt/languages`). To transcribe a station: `job_start` a `wfm_demod` on it, then `job_attach` with `kind: transcribe` and that job's id. The attached job fails with a reason naming the speech backend when the node has none or it cannot serve the language. Every other kind (`identify`, `watch`, the demodulators and decoders) runs on the radio and is refused here as `<kind> is not attachable`. A stage output marked `role: composite` (a video demodulator's composite) is never a job's audio: `job_attach`, the mix and `--then-tail` skip it (a `stream tail` on it still reads it). `stream` names the voice to read of a job whose row lists `voices` (`modes_list`: `slot1`, `slot2`), each on an audio stream with its role; `null` reads the job's main stream as above. To transcribe both slots of a DMR channel, attach twice, once per slot. A role the job lacks is refused naming those it has, `stream: <job> has no voice <role> (it has: slot1, slot2)`, or `stream: <job> has one voice` for a job with none. The attached job's record keeps it as `stream`, each `transcript` event and `transcripts.log` line carries it (`null` for the main stream), and a relaunch (a retune, a restart) reads the same voice. On the CLI, `--stream ROLE`. `packets` captures a decoder's frames for Wireshark: `job_start` an `aprs_decode` (or a frame row) on the channel, then `job_attach` with `kind: packets`, that job's id, `language: null` and `stream: null`; the job's context `capture` (or `packets_list`) gives the id to fetch at `GET /captures/<id>.pcapng`. The file rolls at 256 MiB into a fresh capture, is downloadable while it is written, and does not resume after a node restart (attach again). Stop the `packets` job to close the file; the decoder runs on.",
        ),
        "packets_list" => Some(
            "The packet captures `packets` jobs wrote (`job_attach` with `kind: packets`), newest first. One capture file is a pcapng that Wireshark opens: one packet per decoded frame (`link_type` `ax25_kiss` for APRS, `user0` for a frame row), each stamped with its frame's start and commented `job=<decoder job> source=<id> freq_hz=<centre>`. A file rolls at 256 MiB: it is closed and a fresh one (a new id) opened, as on a retune of the decoder. Fetch one with the bearer at `GET /captures/<id>.pcapng` (`application/vnd.tcpdump.pcap`), also while it is written (`writing: true`): the download holds every whole packet so far. The store keeps 4 GiB of captures, a fixed budget of its own (`--bursts-keep-mb` does not change it): the closed captures written longest ago go first, then any not written to for `--bursts-keep-days` (7 by default), each aged by its last packet; one being written is never expired. A capture does not resume across a node restart: the file is kept and listed, closed, and the `packets` job is listed `stopped` with the note `packet capture does not resume: attach again`; attach again to go on.",
        ),
        "packets_delete" => Some(
            "Delete a packet capture and its file. One being written is refused (`capture <id> is being written by job <packets job>: stop the packets job first`); an unknown id is `unknown` with the ids the store holds. Deleting is the operator's decision: through `ask` the run needs `allow.delete_captures` (`zeta ask --allow-delete`; on the page, `allow deletes on`), else the call is refused as `not_allowed`: report that, do not retry. Never delete a capture the request did not name.",
        ),
        "job_retune" => Some(
            "`replace`: as on `job_start`, the holders that may be stopped to make room, stopped under the gate right before the request; a `conflict` offers `retry with replace: [<ids>]` and the page shows the same retry buttons. A demodulator keeps its `squelch`, `squelch_db`, `deviation_hz` and `sideband` through a retune; its adjacent-channel guard (`guard_hz`, `guard_db`, `channel_hz`, the channel node's `wide_hz`) is derived again from the channel plan at the new band, never carried, so a width preset changes what the filter passes and a move to a neighbouring channel takes that channel's geometry; a guard turned off with `graph_put` (`guard_hz: null`) comes back on a retune onto a plan channel. Every attached job (`job_attach`) reading the retuned job is relaunched under its own id once the input runs again (one that cannot relaunch is `failed` with the reason; the retune still succeeds). A job started beside another (`job_start`'s `beside`; its record's `group`) retunes with its group: `job_retune` of any member moves every member to the new band under one gate. A `conflict` names only holders outside the group, and `replace` may not name a member. A member whose relaunch fails is `failed` with the reason and the others stand; a relaunch never puts two members on one handler. Centre the band on the signal's measured centre (`measure`, `classify`), never on a raster: see Fitting a demod to a signal under Field craft. A one-shot `zeta job retune` cannot reach a job `zeta serve` runs (`job <id> is not running in this node`): use `--node`. On the CLI, `--replace ID[,ID]`. A `scan` its source sweeps for itself (`job_context.scan.method` `source`) is re-aimed rather than restarted: the same id and event stream, `tuning` and `job_context.scan` (`steps`, `step_hz`, `bin_hz`, `alternate`) following the source's new plan, the rows of the old span gone and each row's `sweeps` counting from 1 again; `bin_hz` (`--bin-hz`) asks a new bin width with it (`null` keeps the scan's), and is refused on any other job. A span outside the source's `capabilities.sweep` is refused naming its range, `replace` is refused (`replace applies to a stepped scan`), and a stepped scan stays `scan is not retunable`: stop it and start a new one.",
        ),
        "job_stop" => Some(
            "Stopping a job is the operator's decision: never `job_stop` a job the request did not name, except one you started in this run (`job_start`, `plan_start`, `job_attach`, `graph_start`, `bits_start`, or the `verdict.job` of your own `identify` once you have read it): stop your own `measure` and `identify` leftovers when done. Through `ask` any other stop needs the run to allow stops, else it is refused as `not_allowed`: report that, do not retry. A job a running controller started (its `owner`: a watch's identify run or its trial) is refused naming the owner, `job <id> is run by <kind> <owner>: stop that job`; it ends with its owner.",
        ),
        "jobs_stop" => Some(
            "One call however many jobs match; the page's elastic spectrum keeps running. `group` (with `plan`) narrows a plan to one of its `channel_plans` groups: \"stop the data channels\" is `plan: uhf_cb_au, group: data`. Stopping jobs is the operator's decision: use it only when the request asks for all of them, or names the source, kind, plan or group. Through `ask` the run must allow stops, else the call is refused as `not_allowed`: report that, do not retry. Report how many stopped and any `failed`. A mix stops only with no filter or `kind: mix`.",
        ),
        "mix_start" => Some(
            "Starts one `mix` job that plays the audio of every job on `sources` (`null`: every source) as one stream, so whatever is talking is heard. It holds no lease, tunes nothing and blocks no job; jobs started or stopped later join and leave by themselves. Its `event` stream publishes `active` (who is talking: `job`, `source`, `center_hz`) when it changes, with a hangover after a channel falls silent. It is idempotent on the same set of sources: a mix of that set already running answers its id (`null` matches only `null`). To mute a channel, `graph get` the mix, edit `muted` on the `mix` node (a list of `{source, center_hz}`), `graph put`, which restarts it under its id.",
        ),
        "plan_start" => Some(
            "Starts `kind` on the channels of a plan (`channels`, `null` for all of them, or a named `group` of them from `channel_plans` in place of `channels`) at each channel's `center_hz` with the plan's `bandwidth_hz`, under one lease decision for the band that bounds them: inside the current capture nothing retunes, otherwise the source is tuned when only elastic jobs hold it, and a scan or a rigid holder elsewhere is a `conflict` naming the holders with nothing started. Check `source_list` first. `kind` must run on the radio (not `transcribe`, `spectrum`, `scan`, `watch` or `identify`). `deviation_hz` and `sideband` are passed to every start as on `job_start` (`null` is the kind's default; give 2500 for `fm_demod` on 12.5 kHz channels). `attach` is `null` or `transcribe`, attached to every channel started or already running except the data channels (UHF CB ch22 and ch23, every ACARS and AIS channel: their demodulators start, untranscribed, and a warning names them), and needs a ready stage (a speech backend for `transcribe`), else the op is refused with the stage's reason: start without `attach`. `language` must be `null` without `attach`. `replace`: as on `job_start`. The result lists `started` (channel, `center_hz`, `job`, `attached`, `attach_error`), `existing` (channels an identical running job already holds, skipped), `refused` (channels whose own start failed, with the error; the others went on) and `warnings`: repeat the `warnings` to the operator. Run it again to complete a partial set: running channels are `existing`, so nothing doubles. Name a group, never a list: \"transcribe the lower 40\" is `group: lower40_voice` with `channels: null` and `attach: transcribe` (UHF CB's groups: `lower40_voice` 1-40 less the data channels 22 and 23, `upper40_voice` 41-80 less the reserved 61-63, `voice` both, `repeater_outputs` 1-8 and 41-48, `data` 22 and 23). `channels` and `group` together are refused naming the plan's groups, and an unknown group is `unknown` listing them.",
        ),
        "set_list" => Some(
            "Read it before naming a set: set ids, their roles (`radios`, each with a `note` saying what radio it needs) and members come from here, and it only reads. A set that does not load is `broken` and names the field: report it, never work round it. `running` says whether a set runs, on which sources, and each of its jobs' state (`ended` for one stopped or failed since).",
        ),
        "set_start" => Some(
            "One call starts a whole set; never start its members by hand. Map every role the chosen members use to a source from `source_list` that can capture the role's band (the role's `note` says what it needs), or to `null` to let the node pick the first free one, and report the picks from the result's `radios`. A watch member on a band another watch holds with another policy is `other` naming that watch: report it, never stop or `graph_put` that watch unasked. A `conflict` names the role and the source with the holders, and nothing started anywhere (a `null` role the node could not fill too, `options[0]` `radios.<role>: no free source covers ...`): report it; the operator decides, and `members` narrows the next request to what is free. `replace`: as on `job_start`, never a job the request did not name. A `null` role keeps the source it picked while the set runs. Read the four lists: `started`, `existing` (already running, skipped), `refused` (a member this node cannot run, `unavailable` or `unknown`, such as a decoder whose program is missing, or a start that failed; the others went on) and `warnings` (repeat them). A set's watch member decodes every channel of a span: its `job_context.watch` (`cap`, `known`, `cooled`) and `decoding` events say how, see \"Decoding every channel of a band\" in the field craft. Run it again to complete a partial set: running members are `existing`.",
        ),
        "set_stop" => Some(
            "Stops only the jobs the set's instance started; jobs started by hand on the same radios keep running. Stopping a set is the operator's decision: use it only when the request names the set, or you started it whole in this run (a `set_start` that answered `existing` members leaves `set_stop` to the operator). Through `ask` the run must allow stops, else the call is refused as `not_allowed`: report that, do not retry. Report how many stopped, those that had already `ended`, and any `failed`; a failed stop keeps the instance, so a second `set_stop` finishes it.",
        ),
        "job_list" => Some(
            "Stopped and failed jobs are kept as history, newest 50 only (older ones are forgotten with their files); a re-run (the same kind on the same source and band) takes its predecessor's place rather than a slot of its own; a running job is never dropped. Through `ask` (the agent transport) each record is returned without its `graph`; `graph_get` fetches one.",
        ),
        "knowledge_lookup" => Some(
            "Three providers answer: the built-in seed signatures; `catalogue`, the 594-signal iNTERCEPT catalogue (Apache-2.0; one signature per frequency range of each signal overlapping the band, `bandwidth_hz` its widest bandwidth, `modulation` the first it lists); and `learned`, the operator's accepted signatures (`signature_accept`) whose band overlaps, with `tier: learned`. Every signature also carries `order`, `tone_spacing_hz`, `burst` (`duty`, `burst_ms`, `period_ms`), `sync` (the framing's sync word, hex), `encoding`, `crc`, `confidence` and `tier`, each `null` for the seed and the catalogue (and for what a learned record lacks: a features-only one has no `sync`, `encoding`, `crc` or `decoder`). A learned signature is the operator's own description of a signal heard before, not a decode: `classify` and `identify` label a later match `learned <name>` (layer 2, confidence the record's times its score), which is consistent with that signal, never proof of it. A signature's `decoder` is the id of the mode (`modes_list`) that proves it, the `kind` to start on the band to decode it; `null` means no mode row proves it yet. `classify` runs only the modes with a quick attempt (APRS, POCSAG, ACARS, RDS, AIS, and frame rows with `identify.quick: frame`) as decode-to-identify. The built-in three have no allocations. With `--knowledge-acma` a fourth provider, `acma`, answers: the register's plan services and assignments overlapping the band, each with licensee, site, distance and bearing from the station, power, role, the decoded emission designator and `reach` (whether a signal from that site can plausibly arrive here). It needs the station (`station set`): without one it gives no distances. One register lookup gives the rows, the reach and the answer's `status` and `as_of` (the register data's date): `status` is `ok` (every register query answered, so no assignment means nothing is licensed there), `partial` (`failed` names each register query that failed and why; with `candidates` among them the licences could not be listed, which is not \"nothing licensed\") or `unreachable` with its `reason` (no allocations at all, which says nothing about the band); `status` and `as_of` are `null` when no provider describes the band.",
        ),
        "origin_lookup" => Some(
            "Which licensed transmitters a signal may come from. Two forms, never both: `job` alone, or `source`, `center_hz` and `bandwidth_hz` together (anything else is refused `origin_lookup: give job, or source, center_hz and bandwidth_hz`). By job: the band is the job's (an attached job's is its input's; a `bits` job's is its capture's, from the capture's source), and `modulation: null` is read from the job: a decoder row's `modulation` labels (`modes_list`), a demodulator's kind (`fm_demod` reads `fm`, `am_demod` `am`, `ssb_demod` `ssb`, `wfm_demod` `wfm`), a `bits` chain's demodulator (`fsk`, `psk`, `ook`), else none. The answer is also published as one `origin` event on the job's event stream (so on its source's feed and in `events.log`) and kept as `job_context.origins` (the last answer only); a `bits` job's `origin` event stays on the job's own stream, not on a radio's feed or in `events.log`. A `mix`, `spectrum` or `scan` job has no band and is refused naming the kind. By band: a band of no width is refused (`origin_lookup: a band of no width`), the answer is returned only, nothing is published or kept, and `modulation: null` reads as unknown; the op never measures, so run `classify` on the band to score against a measurement. The answer: `origins`, at most 50 lit sites best first, each `{allocation, score, agree, terms, confirmed, heard}`: `allocation` the register row as `knowledge_lookup` gives it; `score` 0..1, the sum of `terms` (`width` 0.4 when the row's width is 0.5-1.5 times the band's, 0.2 within 0.25-4, 0.1 for a row with no width, each scaled by how far inside the band the row's centre sits: in full on the band's centre, falling to 0 for a row that only overlaps the band's edge, so the on-channel site scores above its neighbours; `modulation` 0.4 when the designator fits a label, 0 when it disagrees, 0.2 unknown; `reach` 0.2 at the station falling linearly to 0.1 at the reach limit, so the nearer of two otherwise equal sites scores higher, 0 beyond, 0.1 with no distance), 1 once `confirmed` by a decoded identity; `agree` `fits`, `disagrees` or `unknown` (no designator, or no label). `from` is where the query was made from: `{lat, lon, kind: station}` for a local radio or a recording, and for a remote source (a KiwiSDR) with a `position` (`source_list`) `{lat, lon, kind: source, source}`: the rows are cut to the reach around that receiver and the distances and bearings are from it, not the station. `from` is `null` with `from_note` saying why (`no station set`, or `remote source without a position`: no reach cut, the rows nearest in frequency across the region). `providers` names the providers asked, `register_rows` the rows the register returned (`null` with no register attached), `receivers` and `plans` the rows seen and not scored, and `register_status` the status of the register answer the origins were scored from (`{state, reason, failed}`: `ok`, `partial` naming the failed register queries, or `unreachable` with the reason; `null` when no provider describes the band, and in an answer kept before it was carried). The `origin` event carries the answer's fields with `job` and a `summary`: `<n> sites on <MHz> MHz: <licensee>, <site> <d> km <agree> <score>; ...`, or, with no site lit, `register unreachable: <reason>`, `register could not list licences: <reason>; partial answer: <query>, ... failed` (its `candidates` query failed), `no licensed transmitter on <MHz> MHz`, `register gave no rows`, `no register provider`. A job whose mode row declares `subject.identity` (`aprs_decode`: `src` against the `callsign`, the SSID stripped) confirms a lit site itself: when one of its events carries a callsign equal to a kept origin's (trimmed, case ignored), that origin gets `confirmed` `{field, value}` and score 1, `job_context.origins` is updated and the `origin` event is published again with `summary` `confirmed <value>: <licensee>, <site>`; a callsign matching no origin changes nothing (many stations share a channel), and only origins already kept are matched, so look the job up first. `heard` is `null` or `{last_ns, grade}` and is set only for a job whose mode row declares `subject.station` (`vdl2_decode`: a ground station's own frames, named by `icao`, and any other frame's `dst` names the station it is sent to; `acars_decode`: an uplink is a station's own frame, anonymous `*`). `grade` is `from` when any station's own frame was heard on the channel, else `to` (an aircraft heard here is talking to a station of that address). A heard site means the register's transmitter is on air on this channel, or an aircraft heard here is talking to a station of that address; the site is consistent; which co-sited transmitter it is stays unknown; it is still not `confirmed`, and the score is never changed by it. `from` is stronger evidence than `to`: the station itself was received, not only an aircraft addressing it. The answer and the `origin` event also carry `stations`, the job's tally `{key: {from, to, first_ns, last_ns}}` (frames the station sent, frames addressed to it; `last_ns` is stream time, the wall time on a live radio), and the `summary` then reads like `2 ground stations on <MHz> MHz: <key> heard <n> frames, <key> addressed by <n> frames; <sites>`; the anonymous station `*` (a row whose uplinks carry no station identity, ACARS) reads `uplinks heard <n> frames`, and a count of one reads `1 frame`. The tally is flushed at the run's end and seeded at launch, so a restart continues the counts. The same tally is kept as `job_context.stations`, written at once on a new station or a station's first own frame and otherwise about once a minute, whether or not a lookup has run, and a fresh `origin_lookup` with `job` answers `heard` and `stations` at once; the band form has no job and reports neither. A lit site is a hypothesis, not an identification: report it as \"consistent with Example Water Authority at Example Hill\", never \"it is\", unless `confirmed`.",
        ),
        "register_site" => Some(
            "The answer holds `provider`, `site_id`, `channels` by frequency (each `channel` (`center_hz`, `bandwidth_hz`, `role` `tx` or `rx`, `emission` as the register stores it), `licence_no` and `licensee`, `null` where the register holds none) and `as_of` (the register's data date). It reads the register live, every call, so it costs a register lookup and needs the register attached (`--knowledge-acma`, provider `acma`). `site_id` is an allocation's `site_id` (from `knowledge_lookup`, `origin_lookup`, or a register answer's candidate `site.id`); never invent one. An empty `channels` means the register holds nothing at that id, not that the register failed. A provider the node does not have is `unknown` (field `provider`, `known` listing the registers it has); an id that is not a register id (1-64 letters, digits, `/`, `_`, `.` or `-`) is `other` beginning `site_id:`, refused before the register is asked; a register that fails or does not answer is `other` beginning `register_site: <provider>:` with the reason: report it as the register being unreachable, never as an empty site.",
        ),
        "channel_plans" => Some(
            "`channel_plans` lists the channel plans the node knows (`uhf_cb_au`: Australian UHF CB, 80 channels, two rasters 12.5 kHz apart; `acars_vhf`: the seven VHF ACARS channels, 130.025-131.825 MHz, 10 kHz each; `ais_vhf`: the two AIS channels, 161.975 and 162.025 MHz, 25 kHz each). Name a channel by its number to the user (\"ch20\") and start jobs at its `center_hz` with the plan's `bandwidth_hz`.",
        ),
        "modes_list" => Some(
            "The decoder modes the node knows, one row each: `id` (the `kind` a decoder job starts with), `name`, `family`, `band` (`bandwidth_hz` the decoder expects, well-known `centers_hz`, `min_hz` the narrowest band it accepts or `null`, `range_hz` where identify and classify try it, one span `[lo, hi]` or a list of spans `[[lo, hi], ...]` (a row of one span is always written `[lo, hi]`), or `null` for anywhere), `modulation` (the layer-1 labels it presents as), `decoder_kind` (`graph`, run in-process, `program`, an external decoder, or `frame`, an in-process frame chain; `null` for a row with no decoder of its own, which only a handler file's entry runs), `file` (`modes/<id>.json` for a built-in row, the path for an operator row), `program`, `checked` (UNIX seconds at which `zeta modes check` last passed the row on this host with every handler found; `null` if never, or for a graph or frame row with no handlers), `subject` (the event field naming what events are about, its table columns and how the page folds it; `null` for none), and `state` ready|broken|unavailable with `reason` (`null` when ready). A broken row's `file` is the file to fix and its `reason` names the field (`<field>: <error>`); an unavailable one names the program it lacks. A ready row may list `missing`: the optional programs it runs without, each `<program>: missing (<hint>; <url>) — <what is lost>` (`voice silent` for a vocoder), `[]` when it lacks none. `outputs` lists the stream kinds a job of the row emits (`event`, `audio`, `packet`; a row listing `audio` gets play and Transcribe on the page, and `job_attach` may attach `transcribe` to its job; a row listing `packet` publishes each frame it decodes as a packet beside its event (an AX.25 frame for `aprs_decode`, the frame's bytes for a frame row), the frames a `job_attach` of `packets` captures; a graph row's are its terminal stages', a program row's `event`, a frame row's `event` and `packet`; empty for a broken row). `handlers` lists every handler of the mode, the row's own first (`builtin` for a graph or frame row, named `built-in`; a program row's first program's file name, `dump1090`; no own handler for a row with no decoder of its own), then each handler file's (`--handlers-dir`) by id: each has `id` (what `job_start`'s `handler` and `handler_policy_set` take), `name`, `in_process`, `outputs`, `state` with `reason`, `missing`, `program`, `checked` (a handler file's from `zeta modes check --handler <id>`), `file` and `extra` (the event fields that handler adds beside the row's). `handler` is the one the policy would pick now (`null` when none is ready). With more than one handler the mode's own fields summarise them: `state` is `ready` when any handler is, `program`, `reason` and `missing` describe the handler `handler` names (the row's own when none is ready), `outputs` is the union over the ready handlers (a capture is possible on the mode, not on every job of it: a job's outputs are its own handler's), and `checked` is the row's own. A row with no decoder of its own (`decoder_kind` `null`) is `ready` while one of its handler files' handlers is, and is otherwise `unavailable` with `reason` `no decoder of its own, and no handler for it is ready` followed by `; <id>: <reason>` for each of its handlers (the missing program, say); its `outputs` are its ready handlers' (`[]` with none), and a start of it always runs, and records, one of them. On the CLI, `zeta handlers list` prints one line per handler of each mode, `<mode> <id> <state> <outputs>`, then `checked=<UNIX seconds>` when a check passed it: this machine's rows and handler files, or with `--node` the serving node's (its programs and checks). `voices` lists the voices a job of the row has beside its main audio stream, each on an audio stream of its own (a DMR row's `slot1` and `slot2`): what `job_attach`'s `stream` may name, and the rows `job_start`'s `audio_policy` applies to; `[]` for a row with one voice. `access` is `'restricted'` on a row whose events carry content a later access control may gate (pager text); such events carry `access: 'restricted'`. `content` is `'network_traffic'` on a row that carries data-network traffic and `null` on the rest: it says what the row carries, nothing reads it yet, and it is not put on events; a job of such a row has `content` in its `job_context` from launch, and a packet capture of its job records it. `family` (and the `family` filter) is one of `tv`, `broadcast`, `aviation`, `maritime`, `satellite`, `two_way`, `cellular_rail`, `pagers`, `iot`, `weather_science`, `amateur_digital`, `amateur_voice`, `hf_utility`, `general`. On the CLI, `--family` and `--state` narrow the list. `zeta modes check [ID] [--strict] [--handler H]` (local only, not with `--node`) runs each row's test on a node over a synthesised capture and prints `<id>: ok`, `<id>: fail: <reason>` or `<id>: skipped: <reason>` (no test, or the program missing: `--strict` makes that a fail); it exits 0 when no row failed. With `--handler H` it runs the test of each mode that has handler `H` (of `ID` only, when given) by a job of that handler, a handler file's printed and recorded as `<id>@<H>`. A row with no decoder of its own is checked by the handler the policy picks, printed and recorded the same way, and is `<id>: skipped: no decoder of its own and no handler ready` when none is ready.\n\nFrame rows: a row with `decoder.kind: frame` decodes framed bursts in-process, with no program. Its `decoder` names the demodulator, `demod: {modulation, symbol_rate_baud, order, tone_spacing_hz}` (`modulation` `fsk`, `psk` or `ook`; `order` 2 when left out, 2 or 4 for fsk, 2, 4 or 8 for psk, 2 for ook; `tone_spacing_hz` `null` for the demodulator's own estimate), the `framing` (the `frame_decode` stage's params, under Stage kinds: sync, length, CRC, line code, whitening, bit order) and the `parser` (`opaque`, the default: the frame's bytes as they are). A job of the row runs `channel → resample → <demod> → clock_recover → slicer → frame_decode` on a band no narrower than `band.min_hz` (else `band.bandwidth_hz`) and publishes one event per cut frame (`sync`, `length_bytes`, `bytes` in hex, `crc_ok`, `payload`) under the row's `protocol` and `summary`; `clock_recover`'s `timing` events go on a separate output. With `identify: {proof: field, field: crc_ok, quick: frame}` a CRC-clean frame proves the row: `classify` runs the chain over the span (evidence `frame <n> B, <crc> ok (<ok>/<frames>)`; a framing without a `crc` never proves) and `identify` runs the row as a child job. `quick: frame` is refused on any other row, and a frame row's `quick` is `frame` or `null`. A bad framing leaves the row broken naming `decoder.framing.<field>`. `zeta modes check` checks a frame row over the `framing` synthesiser family (`test: {kind: synth, family: framing, modulation, symbol_rate_baud, deviation_hz, frames, body_bytes, framing, repeat, expect: {crc_ok: true}, within_ms}`).",
        ),
        "handler_policy_set" => Some(
            "The ranking is the operator's choice, as a job is: set it only when the request asks for it, read the ids from `modes_list`'s `handlers` first, and report the policy it leaves. It changes which handler a later start with `handler: null` runs (a set's, a watch's and an identify's decoders included), never a running job's. The node-wide ranking does not prefer a handler that lacks an output the row's own decoder gives (a handler with events but no audio, on a voice mode): it orders that handler as if the ranking did not name it. Rank such a handler for the mode, or name it in `job_start`'s `handler`, to run it; nothing is excluded, and it still runs when it is the only one ready. Nothing says a node-wide ranking was passed over: the result is the policy as stored, so report what a plain start runs from `modes_list`'s `handler`, not from the ranking. A capture needs no ranking: start the job with `needs: [\"packet\"]`.",
        ),
        "handler_policy_get" => Some(
            "What `modes_list`'s `handler` is chosen from. A mode with no ranking of its own and no node-wide ranking runs an in-process handler before a program, the row's own before a handler file's.",
        ),
        "choice_policy_set" => Some(
            "The operator's choice, as a job is: set it only on the operator's word, never to get a deeper trial for yourself. Give `lease` alone for the common change: it sets only step 3's lease and keeps every other value. Give `policy` alone to replace the whole value, so read `choice_policy_get` first and send it back with the one change; both, or neither, is refused. Per surface (`menu` the page's box menu, `watch`, `agent` you, `client` the CLI, HTTP and MCP) `default` is the step taken when none is asked for and `cap` the deepest step it may ask for, 1 to 3 with `default` at most `cap`; a request past the cap runs to the cap and says `not tried: capped at step <n> for <surface>`. `lease` `share` lets step 3 use only a running capture that covers the band; `widen` also lets it grow a held capture without moving any holder's band. `budget`: `trial_s` seconds in all, `concurrent` trials at once, `slice_s` seconds of the ring one trial replays. `confidence_stop` is `null` or 0 to 1. A value that fails is refused naming the surface or field, and nothing is written.",
        ),
        "choice_policy_get" => Some(
            "The policy a choice reads: the ceilings, the lease setting and the trial budget. Read it before telling the operator why a choice stopped at a step.",
        ),
        "themes_list" => Some(
            "The themes the page can use, one row each: `id`, `name`, `description`, `color_scheme` (`dark` or `light`; `null` for a broken theme), `builtin`, `replaces_builtin` (an operator theme with a built-in's id), `file` (`themes/<id>` for a built-in, the folder's path for an operator theme) and `state` ready|broken with `reason` (`null` when ready; a broken theme's `reason` names the field, `<field>: <error>`). A theme is a folder holding `theme.json`, an optional `style.css` and the fonts and images it names. On the CLI, `zeta themes list`.",
        ),
        "modes_show" => Some(
            "One mode's full row as loaded (`row`: band, decoder graph or program block (an audio program row adds its `demod` and `input.container`; `decoder` is `null` for a row with no decoder of its own, whose handler files' entries decode it), event schema and `summary` template (`{field:.N}` and `{field:+.N}` format numbers), `identify` rule, catalogue ids, `decoder.args` placeholders (`$port`, `$rate_hz`, `$center_hz`, `$center_khz` the centre in kHz, an integer when it divides, else up to three decimals, `$fifo`, `$packet_port` the port of a `packets` block's `tcp` output, `$bandwidth_hz` the job's band width in Hz, a whole number), `decoder.output.fields` (each event field <- the JSON pointer(s) into the program's JSON line it is read from; unmapped rows emit the line as is) and `subject.merge` (the event fields whose equal values mean the same subject across rows sharing `subject.name`, so the page draws one table: `[]` for none) and `subject.merge_skip` (field = value pairs naming the subjects that keep a row of their own, a VDL2 ground station: `{}` for none) and `subject.station` (`{when, key, addressed}`: an event matching every `when` pair is a data link's fixed station's own frame, named by its `key` field (`null`: the anonymous `*`); any other event's `addressed` field names the station it is sent to, so it should be a station identity, never a per-aircraft field, since every distinct value becomes a key of the tally; `null` for no stations; `origin_lookup` reads it), `position` (the event fields of a fix, added to each event as `position: {lat, lon, alt_m}`; `alt_unit` `ft` converts a feet field), `derive` (a list of `{when, from, extract, into}`: the event fields `into` filled from the text of field `from` by the named extractor, only on events matching every `when` pair, and skipped whole when any `into` field already holds a value, so a decoder's structured fix always wins; `acars_position` is the one extractor, giving `lat`, `lon`, `alt_ft`; `[]` for none), `subject` (`key`, `name`, `waterfall` `summary` or `fold`, `columns` of `field`, `label` and `unit`, `heading`, `label`, `identity`: how a subject matches the licence register, `{field, register, strip}`, the event `field` whose value, after `strip` (`ssid` removes an AX.25 `-N`; `null` for nothing), confirms a kept origin whose `register` field (only `callsign`) equals it; `null` for none) and `time` (the event field whose ISO 8601 UTC time becomes `t_ns`), each `null` for none) with its `state`, `reason`, `file`, `program`, `checked`, `missing` (the optional programs a ready row runs without; `[]` for none), `voices`, `outputs`, `handlers` and `handler`, each as in `modes_list`. An unknown id is an `unknown` error (`field: \"id\"`) listing the known ids in `known`; take ids from `modes list`, never invent them.",
        ),
        "classify" => Some(concat!(
            "Layer 1 labels the measured band (`wfm`, `fm`, `am`, `ssb`, `fsk`, `psk`, `ook`, `carrier`, `noise` or `unknown`; an `ssb` label's evidence gives the dial frequency). Layer 2 first sweeps the decoder modes (`modes_list`; the demodulator rows, `role: demodulator`, are never in it) as decode-to-identify: each ready or unavailable row is scored against the layer-1 measurement (0.4 for the measured centre within `band.bandwidth_hz` of one of its `band.centers_hz`, or, for a row that lists no centres, inside one of its `band.range_hz` spans, or always when it has neither; a row with a `range_hz` is not tried at a centre outside every span and away from its centres; 0.3 for a measured bandwidth within 0.5-1.5 times its `band.bandwidth_hz`; 0.3 for the layer-1 label in its `modulation`; plus its best catalogue score, below) and the best 5 are considered in score order (ties by `identify.priority`, highest first, then `family`, then `id`); a graph row with no quick attempt (`m17_decode`) is not considered at all, since only `identify` can try it. A row whose `identify.when_named` is true (`dvbs_ip_decode`) is passed over by this sweep, by `identify`'s own choice of rows and by a `watch` with default modes, unless the caller names it (a `watch`'s `modes`, a set or plan member, or the `candidates` a watch hands its identify run); a job started by hand is unchanged. The result is `{labels, tried, allocations, origins, register, features, choice}`; `origins` is `origin_lookup`'s answer for the band, scored on the measured layer-1 label (`[]` when none), and `register` the register's whole answer for the band (`null` when no provider describes it), all three read from one register lookup per run. ",
            register_note!(),
            " `tried` lists those 5 as `{mode, score, outcome}`, `outcome` one of `proved` (its quick decode passed its CRC/BCH check), `no_frame` (tried, nothing decoded), `skipped_span` (the span is shorter than the row's `identify.min_span_ms`, 1000 ms for the built-in rows), `skipped_quick` (the row has no quick attempt: a program row such as `adsb_decode` is tried only by identify, never by `classify`; use `adsb_decode`), `skipped_band` (the selection is narrower than the row's `band.min_hz`, else its `band.bandwidth_hz`: `rds_decode` needs 150000 Hz, `adsb_decode` 2000000; on a whole block the measured occupied band counts, read as 150000 Hz once it reaches 100000) or `unavailable` (its program is missing). A proof adds a layer-2 label named by the row's `label` (`APRS`, `POCSAG`, `ACARS`, `RDS`, `AIS`) at confidence 1.0, evidence `decode <mode>: ...`; a protocol is labelled only then. A `no_frame` does not prove the protocol absent: the span may have held no frame. Layer 2 also adds up to three signal names from the catalogue (evidence `catalogue <id>: ...`), scored 0.4 for the measured centre lying in one of the signal's frequency ranges (required), 0.3 for a matching bandwidth (required for signals wider than 50 kHz) and 0.3 for a matching layer-1 modulation. Layer 3 is an empty slot. `features` is layer 1's measurement as fields (`bandwidth_hz`, `center_hz`, `snr_db`, the burst shape, `symbol_rate_baud`, and when measured `power99_hz`, the width holding 99 % of the power, `audio_rms_hz`, the audio's rms deviation, and `ssb: {sideband, dial_hz}`); `null` when layer 1 could not measure the band. `choice` is the one job the band answers, `{job, row, step, settled, evidence, runner_up, could_prove, not_tried, reason}`: `job` is `{kind, center_hz, bandwidth_hz, params}`, what to `job_start` (`kind`, centre and width as given; `params` holds `deviation_hz`, `sideband` and the row's other defaults), `row` the mode row it is of, `step` 1, and `settled` one of `proved`, `best_guess` or `cannot_tell`. A row a quick decode proved wins (`settled: proved`; the row's own width, on the measured centre). Otherwise the demodulator row whose `modulation` holds the label is the job (`settled: best_guess`): `fm_demod` for `fm` and for an `fsk` no wider than 25000 Hz, `wfm_demod`, `am_demod`, `ssb_demod`, each sized and centred by its `band.width_rule` and given its `decoder.params` (`modes_show <id>`). With no demodulator for the label (`ook`, `psk`, a wider `fsk`), the best ready decoder row that fits is the job, still `best_guess`: say it is not proved. Otherwise it cannot tell: `job: null`, and `reason` says why (`the band holds noise only`, `no row serves <label>`, `layer 1 did not measure the band`). The centre is the measured one (the dial for `ssb_demod`), never a channel raster's; a channel plan of the station's region at the centre gives `fm_demod` its deviation, and the channel's width when the measured width is under the row's floor and not trusted. `evidence` holds `<source>: <line>` lines: `layer 1: <label> <width> Hz`, `plan: <id> deviation <d>`, `decode <row>: <summary>`, `catalogue: <name>` for a catalogue entry the row lists (a catalogue name only ranks: it never proves a row nor overturns the label). `could_prove` lists, in score order, the ready decoder rows that fit the label, the band and the width and could still prove it (start one, or `identify`, to try); `runner_up` (`{job, row, evidence}`, or `null`) is the first of them when it scores within the weights table's margin of the demodulator, else the next demodulator row; `not_tried` lists `{row, reason}`, a fitting row that is not ready as `not tried: <row> is not ready: <reason>`. Report a `best_guess` as what to listen with, never as an identification. `capture_classify` answers `choice: null`. On the CLI, `zeta classify` prints `choice: <kind> <center> Hz <width> Hz (<settled>, step 1)` or `choice: cannot tell: <reason>` on stderr, before the JSON answer. A span older than the source's ring fails `ring` with `detail.cause` `too_old` and `detail.earliest_ns`, the ring's first retained sample (`source_list` gives its `ring.seconds`): re-issue on a span at or after it, never guess. A span that crosses a gap in the capture fails `ring` with `detail.cause` `gap` and `detail.gap_start_ns`, `detail.gap_end_ns`: re-issue on one side of it.",
        )),
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
        ("set_start" | "set_stop", "set") => Some("airband".into()),
        ("set_start", "radios") => Some(serde_json::json!({"wide": "<id>", "second": "<id>"})),
        // The job form: the band fields stay `null`.
        ("origin_lookup", "job") => Some("<id>".into()),
        ("register_site", "provider") => Some("acma".into()),
        // A ranking set, not cleared.
        ("handler_policy_set", "mode") => Some("adsb_decode".into()),
        ("capture_decode", "kind") => Some("aprs_decode".into()),
        ("handler_policy_set", "order") => Some(serde_json::json!(["dump1090"])),
        _ => None,
    }
}

/// Where an operation's field value comes from, when the type alone does not say.
fn hint(op: &str, field: &str) -> Option<&'static str> {
    if op.starts_with("signature_") && field == "id" {
        return Some("an id from `signature list`");
    }
    match field {
        "dwell_ms" if op == "job_start" => Some("`scan` only, in ms (1-5000)"),
        "average" if op == "job_start" => Some("`scan` only, sweeps averaged (1-64)"),
        "bits" if op == "job_start" => Some(
            "`scan` only, bits per sample: one of `capabilities.depths[].bits` in `source list`",
        ),
        "method" if op == "job_start" => {
            Some("`scan` only, `stepped` or `source`; `null` lets the node choose")
        }
        "bin_hz" if op == "job_start" => Some(
            "`scan` only, the bin width in Hz asked of a source that sweeps for itself (1-1000000)",
        ),
        "dwell_scale" if op == "job_start" => {
            Some("`identify` only: each tried row's dwell times this (1-6); `null` is 1")
        }
        "audio_policy" if op == "job_start" => Some(
            "a mode whose row lists `voices` only: `first_active`, `follow` or `all`; `null` is `first_active`",
        ),
        "handler" if op == "job_start" => Some(
            "a mode row's kind only: a handler id of the mode (`modes list`'s `handlers[].id`); `null` for the handler policy's choice",
        ),
        "choose" if op == "job_start" => Some(
            "`choose` only: `{up_to, surface, start, transient}`; `null` for any other kind, and for a `choose` asking its surface's default",
        ),
        "beside" if op == "job_start" => Some(
            "a running job of a mode row (`job_list`): start a sibling on its band by another `handler` (required); `kind`, `source`, `center_hz` and `bandwidth_hz` must equal that job's; `null` for an ordinary start",
        ),
        "needs" if op == "job_start" => Some(
            "a mode row's kind only: the stream kinds the job must output, among `event`, `packet` and `audio` (`[\"packet\"]` for a job to capture); `[]` for none",
        ),
        "mode" if op == "handler_policy_set" => {
            Some("a mode id from `modes list`; `null` for the node-wide ranking")
        }
        "order" if op == "handler_policy_set" => Some(
            "handler ids of the mode (`modes list`'s `handlers[].id`), first preferred; `[]` clears the ranking",
        ),
        "stream" if op == "job_attach" => Some(
            "a voice of the job, one of its row's `voices` in `modes_list`; `null` for its main audio stream",
        ),
        "capture" if op == "capture_decode" => {
            Some("an id from `capture list`: its whole set is replayed, from the first capture")
        }
        "kind" if op == "capture_decode" => {
            Some("a mode row's id (`modes list`), not a built-in job kind")
        }
        "handler" if op == "capture_decode" => Some(
            "a handler id of the mode (`modes list`'s `handlers[].id`); `null` for the handler policy's choice",
        ),
        "packets" if op == "capture_decode" => Some(
            "`true` writes the handler's packets to a capture file from the recording's first sample (it must output `packet`); `false` for none",
        ),
        "source" => Some("an id from `source list`"),
        "plan" => Some("an id from `channel_plans`"),
        "set" => Some("an id from `set_list`"),
        "radios" => Some("each role of the set (from `set_list`) to an id from `source list`"),
        "members" => Some("member ids of the set (strings), `null` for every member"),
        "sources" => Some("ids from `source list` (strings), `null` for every source"),
        "channels" => {
            Some("channel numbers of the plan (integers), `null` for all (or for `group`'s)")
        }
        "group" if op == "jobs_stop" => Some(
            "a group id of `plan` from `channel_plans`: only its channels' jobs; `null` for the whole plan",
        ),
        "group" => Some(
            "a group id of the plan from `channel_plans` (`lower40_voice`), in place of `channels`; `null` for none",
        ),
        "job" => Some("an id from `job list`"),
        "stream" => Some("an id from `stream list`"),
        "replace" => Some("ids from `job list` (strings), `[]` for none"),
        "widen" => Some(
            "`true` moves a held capture and restarts every job holding the source on it; `false` never retunes a held source",
        ),
        "provider" if op == "register_site" => {
            Some("a register's provider name (`acma` with `--knowledge-acma`)")
        }
        "site_id" if op == "register_site" => {
            Some("an allocation's `site_id` (from `knowledge_lookup` or `origin_lookup`)")
        }
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

/// What a stage param is read for, when the type alone does not say: the `fft`
/// node's look is read by a `scan` job from its stored node and ignored by the stage.
fn param_note(kind: &str, param: &str) -> Option<&'static str> {
    match (kind, param) {
        ("fft", "dwell_ms") => Some("`scan` only, in ms (1-5000)"),
        ("fft", "average") => Some("`scan` only, sweeps averaged (1-64)"),
        ("fft", "bits") => Some("`scan` only, bits per sample, one of the source's depths"),
        ("fft", "method") => {
            Some("`scan` only, `stepped` or `source`; `null` lets the node choose")
        }
        ("fft", "bin_hz") => {
            Some("`scan` only, the bin width asked of a source that sweeps for itself")
        }
        _ => None,
    }
}

/// The `## Mode rows` section: one line per built-in decoder row that has `notes`, in
/// file order. Named so it does not collide with the field craft's `### Decoding modes`.
fn mode_rows() -> String {
    let mut out = String::from(
        "\n## Mode rows\n\nThe built-in decoder rows with notes (`modes_show` has every row in full; an operator row's notes reach `modes_show` only):\n",
    );
    for (file, text) in zeta_core::modes::ModeRegistry::embedded() {
        let row = zeta_core::modes::parse(text).unwrap_or_else(|e| panic!("{file}: {e}"));
        if let Some(notes) = row.notes {
            out.push_str(&format!("- `{}`: {notes}\n", row.id.0));
        }
    }
    out
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
            let h = hint(name, f).map(|h| format!("; {h}")).unwrap_or_default();
            let t = type_of(s, defs);
            // A nullable `Hz` field inlines its type and loses the name: say the unit.
            let u = unit(f)
                .or_else(|| (f.ends_with("_hz") && !t.contains("(Hz)")).then_some("Hz"))
                .map(|u| format!(" ({u})"))
                .unwrap_or_default();
            out.push_str(&format!("- {f}: {t}{u}{h}\n"));
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
        let kind = st["kind"].as_str().unwrap_or("?");
        let note = |p: &str| {
            param_note(kind, p)
                .map(|n| format!("; {n}"))
                .unwrap_or_default()
        };
        let req = required(ps);
        for p in &req {
            out.push_str(&format!(
                "- params.{p}: {}{}\n",
                type_of(&ps["properties"][p], &ps["$defs"]),
                note(p)
            ));
        }
        // A param with a schema default may be left out (older graphs lack it).
        for (p, s) in ps["properties"].as_object().into_iter().flatten() {
            if let (false, Some(d)) = (req.contains(&p.as_str()), s.get("default")) {
                out.push_str(&format!(
                    "- params.{p}: {} (optional, default {d}){}\n",
                    type_of(s, &ps["$defs"]),
                    note(p)
                ));
            }
        }
    }
    out.push_str(&mode_rows());
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
    if op == "source_fit" {
        return "zeta source fit --source <id>".into();
    }
    if op == "source_set_capture" {
        return "zeta source capture --source <id> --policy fit".into();
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
    if op == "handler_policy_set" {
        return "zeta handlers policy set --mode adsb_decode dump1090 [HANDLER]... (no --mode: the node-wide ranking; no handlers: clears it)".into();
    }
    if op == "handler_policy_get" {
        return "zeta handlers policy get".into();
    }
    if op == "choice_policy_set" {
        return "zeta choice policy set --lease share|widen (the lease alone), or --file policy.json (the whole policy)".into();
    }
    if op == "choice_policy_get" {
        return "zeta choice policy get".into();
    }
    if op == "set_list" {
        return "zeta sets".into();
    }
    if op == "set_start" {
        return "zeta set start --set airband [--radio wide=<id>,second=<id>] [--members acars,adsb] [--replace ID,ID] (no --radio: every role null, the node picks)".into();
    }
    if op == "jobs_stop" {
        return "zeta jobs stop [--source ID] [--kind KIND] [--plan ID [--group ID]]".into();
    }
    if op == "capture_list" {
        return "zeta capture list [--center-hz C --bandwidth-hz B]".into();
    }
    if op == "signature_list" {
        return "zeta signature list [--state S]".into();
    }
    if op == "signature_propose" {
        return "zeta signature propose --capture <id> [--name NAME] [--framing-file FILE]".into();
    }
    if op == "signature_accept" {
        return "zeta signature accept --id <id> [--name NAME]".into();
    }
    if op == "station_set" {
        return "zeta station set --lat -27.38 --lon 153.12 --alt-m 4 [--region AU] [--antenna KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]]...".into();
    }
    if op == "bits_start" {
        return "zeta bits start --capture <id> [--modulation fsk|psk|ook] [--symbol-rate-baud N] [--order N] [--then-tail MAX_FRAMES,TIMEOUT_MS] [--keep]".into();
    }
    if op == "capture_decode" {
        return "zeta capture decode --capture <id> --kind aprs_decode [--handler ID] [--packets]"
            .into();
    }
    if op == "origin_lookup" {
        return "zeta origin lookup --job ID | --source S --center-hz N --bandwidth-hz N [--modulation fm,fsk]".into();
    }
    if op == "register_site" {
        return "zeta register site <provider> <site_id>".into();
    }
    if op == "plan_start" {
        return "zeta plan start --plan uhf_cb_au --kind fm_demod --source <id> [--channels 1,2,5-8 | --group lower40_voice] [--attach transcribe] [--language de] [--replace ID,ID]".into();
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
