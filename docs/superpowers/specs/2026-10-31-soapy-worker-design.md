# Plan 20: the SoapySDR driver in its own process, and a source that heals

Design spec, 2026-10-31. Brainstormed with the operator; a reviewer read it as the builder before
hand-over. Builds on plan 14 (`docs/superpowers/plans/2026-10-27-field-fix-wave.md`: device loss
ends in `abandon`, the clock guard, `source_list.stats`) and plan 17 (the `watch` job). Everything
here is written in the terms of `crates/zeta-source-soapy` (the port logic), `crates/zeta-source-
soapy-driver` (the libSoapySDR adapter, outside the workspace) and `crates/zeta-core/src/node` (the
lease manager, resume, `forward_iq`).

## 1. Intent and why

A SoapySDR driver runs inside the node today. Plan 14 made the node survive a USB drop in-process
(the reader gives the device up, `abandon` leaks the handle rather than touch it, the source faults
`no samples for 1000 ms (device unplugged?)`), but two things remain:

- A driver crash (a SIGSEGV in SoapyHackRF, libhackrf, libusb or UHD) still ends the process that
  loads it, which is the node, with every job, stream and client on it.
- A source that lost its device stays faulted until the node restarts: `tune` and `source gain`
  answer `device lost: ...; restart the node`. The field workaround is the two-node form (a child
  `zeta serve` owning the HackRF, the main node attaching it with `--source-node`), which has its
  own faults: a pulled cable leaves the child alive but dead, so a restart loop never fires; the
  link ships 250 kHz unless widened; two state directories, two secrets, two units.

The operator's standing case is the sonde `watch` on the field host: a job meant to run for days on a
B210 behind a USB cable. It must survive a cable wobble or a driver crash without anyone noticing,
and when the radio comes back, the pin, the gain and every job that was on the radio come back too.

Plan 20 therefore does two things, and the second is the point of the first:

1. The driver moves into a child process, `zeta soapy-worker`, that the node spawns and supervises.
   A crash or a lost device ends the worker, never the node.
2. A faulted SoapySDR source heals: the node respawns the worker, waits for the device to reappear,
   reopens it, restores the pin and the gain, and relaunches the jobs that failed with its fault,
   the watch and its decoders among them. "Restart the node" leaves the vocabulary.

## 2. Goals, non-goals, anti-goals

### Goals

- A SIGSEGV in the driver (simulated with `kill -SEGV` on the worker, and the real thing) fails the
  jobs on that source with a reason naming the worker's end, faults the source, and leaves the node,
  every other source and every other job untouched. `GET /health` keeps answering throughout.
- A device that goes away (cable pulled) faults the source with today's text and keeps the node up,
  as plan 14 does; nothing new has to be learned about the fault's appearance.
- A device that comes back (cable replugged, driver respawned after a crash) is reopened without an
  operator: the worker is respawned, the pin and the gain restored, and every job that failed with
  that source's fault relaunched under its own id, in the order a node restart would launch them.
  A `watch` that was decoding is decoding again, its kept decoders relaunched and re-adopted.
- Startup with the radio unplugged: a `--source-soapy` whose device is absent attaches faulted
  (`device not found: <args>`) when the node has seen that device before, and heals when it
  arrives; only an error other than absence (bad args, a gain out of range, an unknown driver)
  still refuses the flag as today.
- Observability: `source_list` says the source runs in a worker and how many times it has been
  respawned; the node's stderr says, once per episode, that the device went, that it is awaited,
  and that it is back with what was restored.
- Behaviour pinned: every existing Soapy test (`crates/zeta-source-soapy/tests/soapy.rs`) passes
  unchanged against the in-process source, which stays as it is. The worker is tested without
  hardware and without libSoapySDR.
- Deployed on the field host: the `sdr` unit runs the B210 behind a worker, and a `kill -SEGV` of the
  worker during a running watch ends with the watch decoding again, with nothing done by hand.

### Non-goals

- No change to the rate model, the clock guard, the stats, the gain path or the fault texts of
  `SoapySource`: the worker hosts it as it is.
- No worker for the other sources (file, Kiwi, VRT, remote node): they do not link a driver that
  crashes. The worker protocol is generic (it serves any `Port`) but only the Soapy kind ships.
- No shared memory for the IQ path. A pipe carries 16 Msps of cf32 (128 MB/s) with room to spare;
  if a wider device ever needs more, that is a change inside the worker crate, not to the node.
- No retirement of `--source-node` or the federation: the two-node form stops being the answer to
  a crash, that is all.
- No change to the `Port` contract's view of faults: jobs on a faulted source still fail with the
  fault as their reason, and a successful tune still clears it. Healing is a node behaviour built
  on top of that, not a new port state.

### Anti-goals

- A node that silently hides a fault. Every failure is still a `Failed` record with the reason, a
  `source_list.fault` while it lasts, and a stderr line; healing adds a second stderr line, it does
  not remove the first.
- A relaunch storm. A worker that dies at once on every respawn (a bad cable, a broken driver) is
  retried with a bounded backoff, never in a tight loop, and never more than once per 5 s.
- A relaunch of something the operator stopped. Only jobs that failed because of the source's fault
  during the current fault episode come back; a job the operator stopped, or one that failed for a
  reason of its own, stays as it is.
- A secret in the worker. The worker's environment carries no `ZETA_SECRET`, no
  `ZETA_NODE_SECRET_*`, no `ZETA_KIWI_PASSWORD_*`, no `ZETA_STT_NODE_SECRET`, no `ZETA_LLM_KEY`,
  and no other `ZETA_*` variable whose name contains `SECRET`, `PASSWORD` or `KEY`: the list
  names today's credentials, the pattern catches the ones added later.

## 3. Constraints

- The workspace must still build and test without `libsoapysdr-dev`. The driver crate stays
  excluded; the worker's generic parts (framing, the client port, the supervisor, the fake worker
  used by tests) live in the workspace; the Soapy worker's `main` lives in `crates/zeta` behind
  `cfg(soapy)` like `open_soapy` does today.
- One deployable binary. The worker is the `zeta` executable itself, invoked as `zeta soapy-worker`
  (the node spawns `/proc/self/exe` on Linux, `std::env::current_exe()` elsewhere), so the field host unit changes nothing but the binary.
- `Port` methods `fault`, `current`, `gain`, `stats`, `capabilities` are synchronous. The parent must
  answer them from state it already holds; no round trip to the worker inside them.
- Capabilities are taken once at attach (`Port::capabilities` returns a reference). The worker's
  capability record is read at its first hello and kept for the life of the port.
- UHD opens slowly (a B210 loads its FPGA image; the field host's journal shows 2-4 s from process start
  to the probe line, cold boot included), and two opens of one B210 cannot overlap. The parent must
  never spawn a second worker while one may still hold the device.
- Linux only for the parent-death safeguard (`prctl(PR_SET_PDEATHSIG)`); everywhere the worker also
  exits on stdin EOF, which is the portable guarantee.
- The e2e suite runs debug builds in real time and is near its 300 s budget; the worker tests use
  short captures and the fake worker, never a real device, and the UI suite is not extended.

## 4. The design

### 4.1 Shape

```
zeta serve ──spawns──> zeta soapy-worker --id b210 --args driver=uhd,serial=...,gain=65,ppm=2.7
   │                        │
   │  WorkerPort            │  SoapySource (unchanged) over the real Driver
   │  (implements Port)     │  + a stdio server: control in, frames out
   │                        │
   ├── stdin  ───JSON lines──────────>  tune / set_gain / shutdown
   ├── stdout <───framed messages────  hello, replies, status, IQ blocks
   └── stderr <───passes through────  the worker's own log lines, prefixed
```

Three pieces:

- `zeta-source-worker` (new crate, in the workspace): the wire format (§4.3), `serve(port, stdin,
  stdout)` which exposes any `Arc<dyn Port>` over stdio, and `WorkerPort` (§4.4), a `Port` that
  spawns a command, speaks the protocol to it and supervises it (§4.5). It depends on `zeta-core`
  only.
- `zeta soapy-worker` (subcommand in `crates/zeta`, `cfg(soapy)`): opens the device through
  `zeta_source_soapy_driver::open` exactly as the node does today and hands the `SoapySource` to
  `serve`. Without `cfg(soapy)` it exits 1 with the `NO_SOAPY` text.
- The node: `--source-soapy` now builds a `WorkerPort` (§4.6); a port that reports it has healed
  triggers per-source recovery (§4.7), which is the startup resume path applied to one source.

### 4.2 The worker process

`zeta soapy-worker --id ID --args ARGS` does, in order:

1. Enumerate with the driver-selecting keys of `ARGS` (every key except zeta's own `gain` and
   `ppm`). An empty result exits with code **3** (`device not found`), nothing else attempted, after
   one stderr line. This is the absence signal the parent polls on; it is cheap and does not touch
   the device.
2. Open through `zeta_source_soapy_driver::open(id, args)` (gain and ppm applied as today, the
   `zeta: gain for <id>` line on stderr as today). An open error exits with code **4** and the error
   text as the last stderr line.
3. Write `hello` (§4.3) with the capability record, the gain record and the driver key, then serve
   until stdin closes or a `shutdown` arrives. On either, the `SoapySource` is dropped (the reader
   deactivates its stream as today) and the process exits 0.

Platform safeguards: `PR_SET_PDEATHSIG(SIGTERM)` on Linux before the open; `SIGTERM` is handled as
`shutdown`. The worker never reads the state directory and never receives the node's secrets: the
parent spawns it with the parent's environment minus the variables named in §2 (UHD and SoapySDR
read `UHD_IMAGES_DIR`, `SOAPY_SDR_PLUGIN_PATH`, `HOME`, `PATH`; those pass through).

A fault inside the worker is plan 14's path, unchanged: the source faults (and on device loss sets
`lost` and abandons the stream). The worker watches `Port::fault()` on its own source (polled with
the 1000 ms status tick and checked after every reply); **any** fault makes it publish that status
(§4.3) and **exit with code 5** (`faulted`) after a 200 ms grace so the status reaches the parent.
That covers the `lost` faults (`no samples for 1000 ms (device unplugged?)`, `read failed`, the
overflow storm, a retune that lost the device) and the two that are not `lost` today (`soapy retune
timed out`, `samples arrive faster than ... Hz`): in-process those clear on the next operator tune,
but behind a pinned worker nothing would retune, so a reopen is the right answer for them too, at
the cost of one UHD open. Exiting is the simplest correct thing: the leaked handle dies with the
process, reopening is the parent's job, and `SoapySource` needs no new API (`fault()` is on `Port`).
A worker that is tuned and healthy streams for as long as it lives; the node's ring and bank are a
permanent subscriber today, so no idle-stop protocol is needed.

### 4.3 The wire format

Parent to worker, on stdin: one JSON object per line, each with an integer `id` and a `cmd`:

- `{"id": n, "cmd": "tune", "center_hz": u64, "bandwidth_hz": u64}`
- `{"id": n, "cmd": "set_gain", "gain_db": f64 | null}`
- `{"id": n, "cmd": "shutdown"}`

Worker to parent, on stdout: frames of `kind: u8, len: u32 LE, payload[len]`.

- kind 0, control, a JSON object:
  - `{"hello": {"protocol": 1, "capabilities": Capabilities, "gain": GainInfo | null, "driver": str}}`
    once, first.
  - `{"reply": n, "ok": Tuning}` or `{"reply": n, "ok": GainInfo}` or
    `{"reply": n, "err": {"kind": "out_of_range" | "unsupported" | "io", ...PortError fields}}`.
    `PortError` gains `Serialize`/`Deserialize` for this; the texts the node shows come out the same.
  - `{"status": {"fault": str | null, "current": Tuning | null, "gain": GainInfo | null,
    "stats": PortStats | null}}` on every change the worker observes and at least every 1000 ms.
- kind 1, an IQ block: `t0_ns: u64, sample_rate_hz: u64, center_hz: u64, n: u32`, then `n`
  little-endian cf32 pairs. The block is `IqBlock` byte for byte but the `Arc`.

Flow: a `tune` is answered when `SoapySource::tune` returns (its own `RETUNE_ACK` timeout bounds it).
Blocks are written by one task that subscribes to the source's broadcast; if the parent falls
behind, that task lags on the broadcast and blocks are lost as they would be in-process (`t0` jumps
forward, never back), and the loss is logged at most every 10 s like `zeta-source-node` does. The
parent reads stdout on a blocking thread and publishes blocks on its own broadcast.

### 4.4 `WorkerPort`

A `Port` whose state is a snapshot of the worker's last `status` plus its own supervision state.

- `capabilities()`: the hello's record, taken once (§3). A respawned worker whose record differs is
  logged (`zeta: soapy worker for <id>: capabilities changed; keeping the first`) and served with
  the first record.
- `tune(t)`: checks `t` against the capability record first (the same `out_of_range` texts as
  `SoapySource` gives, so a bad request never waits on a dead worker), then, under the port's own
  gate, sends `tune` and awaits the reply with a timeout of `RETUNE_ACK` plus 2 s. No live worker
  (dead, respawning, awaiting the device) answers at once with `Io("<the current fault>")`; the
  fault text is the one `fault()` returns, so a `source_ensure` on an unplugged radio reads the
  same as its `source_list`.
- `current()`, `gain()`, `stats()`, `fault()`: from the snapshot. `fault()` is the worker's reported
  fault when it is alive, else the supervision fault (§4.5). A successful `tune` clears both.
- `set_gain(db)`: like `tune`, a round trip under the gate.
- `subscribe()`: the parent's broadcast of the blocks read from stdout.
- `idle()`: a no-op, as for `SoapySource`.
- `healed()` (new on `Port`, default `None`): `Option<watch::Receiver<u64>>`, a counter the port
  bumps when a fresh worker has said hello after a fault. The node subscribes to it at attach
  (§4.7). This is the only addition to the `Port` trait, and the only new port-level concept.
- `worker()` (new on `Port`, default `None`): the `WorkerInfo` `source_list` shows (§4.8).

### 4.5 Supervision

One supervisor task per `WorkerPort`, owning the child. Its loop:

1. Spawn the worker; wait for hello with a 30 s timeout (UHD). On hello: record the capability and
   gain records, set `alive`, publish the status, and if this is a respawn after a fault, bump
   `healed`.
2. Run: read frames until stdout closes. Each status frame replaces the snapshot.
3. Ended: reap the child and set the supervision fault from the exit:
   - code 5 (faulted): the worker's last status fault stands (`no samples for 1000 ms (device
     unplugged?)`, `read failed: ...`, `overflow storm ...`, `soapy retune timed out`); the parent
     appends nothing.
   - code 3 (device not found): `device not found: <args>`.
   - code 4 (open failed): `soapy open failed: <last stderr line>`.
   - a signal or any other code: `soapy worker for <id> died: signal 11 (SIGSEGV)` / `exit code N`.
   - no hello within 30 s: `soapy worker for <id>: no hello in 30 s`, the child killed.
4. Back off and go to 1: 1 s after the first end, doubling to a 5 s cap, reset after a worker has
   lived 60 s. A device-not-found exit polls at the cap at once (it is the common long wait). The
   loop never gives up: a radio unplugged for an hour comes back after an hour.

2026-11-02: the hello no longer ends the fault at once. It moves to a held fault that `fault()` keeps reporting (a tune does not clear it) until the node calls `Port::heal_settled`, so a crash shorter than a job's poll of the fault still fails the job; `reap` clears it too.

Logging, once per episode, so a long unplug does not flood the log: `zeta: soapy worker for <id>:
<fault>; waiting for the device` on the first end, `zeta: soapy worker for <id>: back after <n>
attempts, <s> s` on the hello that heals. A respawn whose worker dies again logs at most once per
minute.

Shutdown of the parent (`zeta serve` exiting, a one-shot command ending, the `WorkerPort` dropped):
`shutdown` on stdin, 2 s for a clean exit, then `SIGKILL`. A one-shot command that attaches a Soapy
source therefore starts a worker (and a UHD open), as it opens the device in-process today.

Rule from §3: the supervisor spawns a new worker only after it has reaped the old one.

### 4.6 Attaching a Soapy source

`node_setup` builds a `WorkerPort` for each `--source-soapy ID=ARGS`, spawning `/proc/self/exe
soapy-worker --id ID --args ARGS` (`current_exe()` off Linux), so a respawn runs the running
image even after the binary on disk was replaced. The first hello decides whether the flag is accepted:

- hello: attached, as today.
- exit 3 (not found) and `sources/<id>.json` holds a capability record from an earlier open: attached
  faulted `device not found: <args>`, the supervisor polling. The record gains `capabilities:
  Option<Capabilities>` (format 1, defaulted), written on every hello. `zeta-core` exposes a reader
  for one source record (`load_sources` is private to the node today) so `node_setup` can consult
  it before the node opens.
- exit 3 and no record: refused with `source <id>: device not found: <args> (never seen on this
  host; plug it in once)`, exit 1.
- exit 4 or a death before hello: refused with the reason, exit 1, as a bad flag is today.

`--soapy-in-process` (global flag, default off) builds the in-process `SoapySource` instead, as the
node does today, for diagnosing the worker itself. `zeta soapy find` stays local and in-process.

### 4.7 Healing: per-source recovery in the node

Today's startup resume does, in order: `restore_gains`, `restore_pins`, `resume_captures` (hold the
capture the rigid records shared), launch the resumable records (a scan first, then widest first),
release the capture holds. Recovery is that sequence for one source, run when the source's port
reports `healed`:

1. The node subscribes to `port.healed()` when it attaches a port. On a bump it runs
   `recover_source(id)` under that source's lease gate so no `job_start` interleaves.
2. `recover_source` restores the gain from `sources/<id>.json`, re-takes the pin **from that record**
   (`leases.pin` at the recorded band: a pin that failed to tune at startup was never leased, so
   `leases.pinned()` is not the source of truth; for a live fault the pin lease is still held and
   this is a tune of the same band), then relaunches the **recoverable** records of that source with
   the resume machinery (`resume_captures` for this source only, then `launch` in resume order),
   then the attached records whose input now runs, and releases the hold.
   2026-11-02: when the port still holds its fault, `recover_source` opens the episode, waits (at most 10 s, logging `zeta: source <id>: recovery: job <ids> did not end in 10 s; left running`) for the running jobs that poll the fault as well as the failed ones, then calls `heal_settled` under the gate. A `job_start` in that hold is accepted, and its job may fail and be relaunched under its id. A bump before the port is attached is taken for a heal, which the hold relies on.
3. A record is recoverable when it is `Failed` with a reason starting `source <id>: ` **and** it
   failed during the current fault episode. Every way a port fault fails a job carries that prefix:
   `forward_iq`, the scan loop and the watch loop do today; the launch-time tune failure
   (`ensure_tuned` in `Node::open`'s resume and in `job_start`, which writes the lease error's text
   bare, `io: device not found: ...`) is changed to write `source <id>: <error>` too, so a record
   relaunched at startup against an absent device is recoverable. The episode begins when the node
   first sees the port faulted (a job failing with the prefix, or `source_list`/`job_start` reading
   the fault) and ends at `healed`; the node keeps it in memory per source and a record's
   `ended_ns` places it. A node restart during an episode starts a fresh one: the records relaunch
   as `Running` at startup, fail with the prefix inside the new episode, and recover with it.
   Attached jobs (`transcribe`): today an attached job whose input fails live ends **`Stopped`**
   (`stream_in` ends `Ok` when the input stream closes), which recovery must not touch. This plan
   changes that one case: an attached job whose input ended with a `source <id>: ` fault ends
   `Failed { reason: "input job <id> not running" }`, the text a node restart already gives, so the
   existing input rule (`resumable`) relaunches it once its input runs again. An input stopped by
   the operator or ended for its own reason still leaves the attached job `Stopped`.
4. Recoverable records are relaunched under their own ids, as at startup. `prune_history` never
   drops a recoverable record while its episode lasts (it still counts toward the 20).
5. The watch: when its source faults, the watch's own `forward_iq` usually fails first and cancels
   the job, so the watch loop returns `Cancelled`, not `Failed`; the job record still ends `Failed`
   with the prefix (the first failing stage's reason). The watch's exit teardown must therefore key
   on **`r.port.fault()` being set at exit**, whatever the loop returned: with a port fault present
   the teardown is skipped (its decoders fail on their own with the same fault, which keeps them
   recoverable; a `stop_gated` would mark them `Stopped` before their forwarders saw the fault). On
   recovery, resume order is widest first, so the watch launches before its decoders and re-adopts
   them by `wait_running` (`ADOPT_WAIT` is 5 s today); the builder either confirms the decoders are
   launched inside that window or launches a recovered watch's kept decoders first. Acceptance:
   after recovery `job_context.watch.decoding` names running decoders and the watch publishes no
   `lost` for them.

Recovery is not an op (nothing is appended to `ops.log` for it) but it is loud: one stderr line per
source listing what came back (`zeta: source <id>: recovered: pin 403025000/5950000 Hz, gain 65 dB,
3 jobs relaunched`), and each relaunched job's record returns to `Running`.

### 4.8 Observability

- `source_list`: a new required nullable field `worker` on `SourceInfo`: `{"pid": u32 | null,
  "restarts": u64, "since_ns": u64 | null}` for a source behind a worker, `null` for every other
  source. `restarts` counts spawns after the first; `since_ns` is the live worker's hello time.
- `GET /health` is unchanged; `stages_running` must return to its pre-fault value after recovery
  (the leak check the operator already uses).
- stderr lines as in §4.5 and §4.7; the worker's own stderr passes through the parent's with the
  prefix `zeta-soapy-worker[<id>]: ` so a driver's `[ERROR] [UHD]` line is attributable.

### 4.9 CLI and docs

- `zeta soapy-worker --id ID --args ARGS` (hidden from `--help`'s main list; documented in
  AGENTS.md's running section as what the node spawns).
- `--soapy-in-process` global flag.
- AGENTS.md / `agents_md.rs`: the `--source-soapy` paragraph says the device runs in a worker, what
  a fault looks like, that it heals and what comes back; the "A radio in its own process" field
  craft section is rewritten around this (the two-node form stays documented as federation, not as
  crash protection); "Reading a source's health" mentions `worker`. README, skill and docs follow.
- `TODO.md`: the three bug sections this plan closes ("a USB drop ... segfaults the node" (the
  remaining in-process clause), "a node that loses its only device stays up faulted", the two-node
  gotchas) are replaced by one line each pointing at this spec.
- Plan series: row 20 marked, a "Plan 20 status" section with the acceptance outcomes.

## 5. Testing

No test needs hardware or libSoapySDR.

- **Fake worker.** `zeta-source-worker` ships a test binary (a `[[bin]]` behind a `test-bin`
  feature that enables `zeta-source-soapy/test-mock` and `zeta-source-file`; the gate is `cargo test
  --workspace`, so the feature is reached by workspace feature unification the way
  `zeta-core/test-fixtures` is today) whose stdout speaks the real protocol and whose port is chosen
  by flag: `--script stream` (a `SoapySource` over a `MockDevice` giving samples forever), `--script
  gone-after-ms N` (a `MockRead::Gone` after N ms, so the fault exit 5 path runs), `--script
  not-found` (exit 3), `--script open-fails` (exit 4), `--script crash-after-ms N` (`abort()`,
  SIGABRT), `--script hang-hello` (never writes hello), and `--script file PATH[,crash-after-ms N]`
  (a `FileSource` over a SigMF recording, for tests that need a real signal: the `MockDevice`'s
  samples are a ramp, which no detector sees). The fake worker's `set_gain` calls are recorded in
  a file named by `--record PATH` so a test can assert the gain was re-applied.
- **Protocol tests** (`zeta-source-worker`): frame round trips, a block byte for byte, a reply for
  each `PortError` variant with the same `Display` text, a status snapshot, hello parse.
- **Supervisor tests** (`zeta-source-worker`): crash → `fault()` names the signal, `restarts` 1 after
  respawn, `healed` bumped once; not-found → fault text, polling at the cap, hello heals; hang-hello
  → killed at the timeout, fault text; backoff schedule (1, 2, 4, 5, 5 s; reset after 60 s alive)
  with a mocked clock or shortened constants; a tune during a respawn answers `Io(<fault>)` at
  once; the environment handed to the child lacks every secret variable and keeps `HOME`/`PATH`;
  drop of the port ends the child within 2 s (shutdown, then kill).
- **Node tests** (in `zeta-source-worker/tests`, which is the only package whose tests can find the
  fake binary through `CARGO_BIN_EXE_<name>`; dev-deps on `zeta-core` with `test-fixtures`,
  `zeta-dsp` and `zeta-source-file`): a
  `passthrough` and a pinned source over a worker that dies after 1 s: the job fails `source <id>:
  soapy worker for <id> died: ...`, `source_list` shows the fault and `worker.restarts` 0, then (the
  fake respawns healthy) the pin is back, the gain re-applied (the fake records `set_gain` calls),
  the job is `Running` again under the same id, `stages_running` equals its pre-fault value. A job
  the operator stopped before the fault stays `Stopped`; a job that failed for its own reason
  stays `Failed`; a job that failed with the fault in an earlier episode (healed in between) stays
  `Failed`. History pruning with 25 terminal records keeps the recoverable ones. A `transcribe`
  attached to an `fm_demod` on the source: both fail (the attached one `input job <id> not
  running`), both run again. A `watch` over the plan 17 watch harness's synthesised FSK fixture
  (`crates/zeta-core/tests/watch.rs`), served by `--script file ...,crash-after-ms N`: the watch and
  its decoder fail, then both run again and `watch.decoding` names the decoder with no `lost` event
  in between.
  Startup with `--script not-found` and a `sources/<id>.json` carrying capabilities attaches
  faulted; without the record the node refuses with the "plug it in once" text.
- **CLI tests** (`crates/zeta/tests`): `zeta soapy-worker` without `cfg(soapy)` exits 1 with
  `NO_SOAPY`; `--soapy-in-process` is accepted; `--help` does not list the worker among the groups.
- **Existing suites** unchanged and green: `zeta-source-soapy` with `test-mock`, the e2e UI suite
  within its budget, and `RUSTFLAGS="--cfg soapy" cargo clippy -p zeta-source-soapy-driver` and
  `-p zeta` clean.

## 6. Acceptance on the field host

Built on the field host under the `sdr` account (12 cores, 125 GB RAM, ~190 GB free on `/home`; the
operator is making room for build artefacts) with `RUSTFLAGS="--cfg soapy"`, installed as
`~/zetasdr/plan20-zeta`, the unit's `ExecStart` pointed at it, nothing else in the unit changed.

1. Restart the unit. `source list` shows `b210` with `worker.pid`, its pin and gain restored,
   `modes check --strict rs41_decode` still passes (local, with the unit stopped), the page loads.
2. With the sonde watch running (re-armed as the plan 17 handover says), `kill -SEGV <worker pid>`:
   within 1 s `source list` shows the fault naming the signal and the watch `Failed` with it; within
   30 s (UHD reopen) the worker is back, `worker.restarts` is 1, the pin and gain are restored, the
   watch is `Running` under its id, and `stages_running` is back to its pre-kill value.
3. If the operator is at the machine: pull the B210's cable, wait 10 s, replug. The same outcome
   through the device-lost path (`no samples for 1000 ms (device unplugged?)`, exit 5, polling,
   heal), and `journalctl` shows exactly one "waiting" and one "back" line.
4. The desktop HackRF form: `scratch/serve-hackrf-child.sh` and the two-node launcher are retired in
   favour of a single node with `--source-soapy hackrf=driver=hackrf,gain=32`; a cable pull and a
   `kill -SEGV` behave as in 2 and 3.

Record the outcomes in the plan series "Plan 20 status".

## 7. Left to the builder

- Crate layout inside `zeta-source-worker` (modules, where the fake worker's scripts live).
- The exact JSON shape of `PortError` on the wire, as long as `Display` is preserved.
- How the supervisor's clock is mocked in tests (shortened constants behind a `cfg(test)` or
  a `with_timings` builder), and the exact 30 s / 2 s / 60 s constants' names.
- Whether `recover_source` is a refactor of `Node::open`'s resume into a per-source function called
  from both places (preferred) or a parallel implementation.
- How `wait_running` and resume order interact for the watch (§4.7 item 5).
- Stdout read buffering, the broadcast capacity in the parent, and the per-block copy strategy.
- The exact hidden-subcommand mechanism in clap.

## 8. Calls made in this document (check these while reading)

- The worker exits on **any** source fault (code 5) instead of trying to reopen in place: the leaked
  handle dies with the process, which is the whole reason for the process boundary, and the two
  non-`lost` faults get a reopen instead of waiting for a tune nobody will send.
- An attached job whose input died of a source fault ends `Failed` (`input job <id> not running`),
  not `Stopped`, so transcribers come back with their demodulators; every other way an input ends
  still leaves it `Stopped`.
- The launch-time tune failure text gains the `source <id>: ` prefix so startup-against-an-absent-
  device records are recoverable; recovery reads the pin and gain from `sources/<id>.json`.
- The watch skips its decoder teardown whenever its port is faulted at exit, whatever its loop
  returned.
- Healing relaunches only records that failed with the source's fault **during the current
  episode**; anything older, stopped, or failed for its own reason is left alone.
- The watch's Failed-exit teardown is skipped on a source-fault exit so its decoders stay
  recoverable and are re-adopted rather than re-identified.
- Startup with the device absent attaches faulted only when the device has been seen before (the
  capability record cached in `sources/<id>.json`); a never-seen device still refuses the flag.
- `Port` gains two optional methods, `healed()` and `worker()`, and nothing else; recovery lives in
  the node, not in the port.
- The worker is the default for every `--source-soapy`; `--soapy-in-process` keeps the old path for
  diagnosis only.
- The worker inherits the parent's environment minus the secret variables, rather than the cleared
  environment plugins get: UHD and SoapySDR need their own variables and the worker is our code.
- Backoff 1 s doubling to 5 s, hello timeout 30 s, shutdown grace 2 s, "alive" reset at 60 s.
- Blocks cross the pipe as cf32 (the format `SoapySource` already produces), not the wire format.
- Nothing is appended to `ops.log` for a recovery; stderr and `source_list.worker` carry it.
