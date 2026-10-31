# Soapy Worker Process Implementation Plan (plan 20)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The SoapySDR driver runs in a child process the node spawns and supervises, so a driver crash or a USB drop ends the worker and never the node; and a faulted SoapySDR source heals: the worker is respawned, the device reopened when it reappears, the pin and gain restored and every job that failed with the source's fault relaunched under its own id, the `watch` and its decoders included.

**Architecture:** A new workspace crate `zeta-source-worker` holds a generic "Port over stdio" protocol: `serve(port)` exposes any `Arc<dyn Port>` on stdin/stdout (JSON control lines in, framed control/IQ messages out) and `WorkerPort` is a `Port` that spawns a command, speaks that protocol and supervises the child (hello timeout, exit-code faults, bounded backoff, a `healed` counter). `zeta soapy-worker` (in `crates/zeta`, `cfg(soapy)`) opens the device as today and hands the unchanged `SoapySource` to `serve`, exiting 5 on any fault so the leaked handle dies with the process. In `zeta-core`, `Port` gains `healed()` and `worker()`, every port fault that fails a job carries the `source <id>: ` prefix, an attached job whose input died of a source fault ends `Failed` instead of `Stopped`, the watch skips its decoder teardown when its port is faulted at exit, and `recover_source` (the startup resume for one source) runs when a port heals. `source_list` gains `worker`.

**Tech Stack:** Rust 2024, tokio (process, io, sync), serde/serde_json, the existing `MockDevice` (`zeta-source-soapy`, feature `test-mock`), `FileSource` fixtures (`zeta-source-file`, feature `test-fixtures`), `assert_cmd` for CLI tests, the `soapysdr` driver crate behind `--cfg soapy` (compile check only, no hardware in tests).

**Spec:** `docs/superpowers/specs/2026-10-31-soapy-worker-design.md` (read it first; section numbers below refer to it).

## Global Constraints

- The workspace builds and tests without `libsoapysdr-dev`: `zeta-source-soapy-driver` stays excluded; everything that links it stays behind `cfg(soapy)` in `crates/zeta`. `cargo test --workspace --all-features` is the gate and must reach every new test, so test-only binaries are enabled by feature unification (a `test-bin` feature enabled by the crate's own dev-dependency on itself, as `zeta-source-soapy` does with `test-mock`).
- No new third-party dependency beyond what the workspace already uses (`serde`, `serde_json`, `tokio`, `tokio-util`, `thiserror`, `tempfile`, `assert_cmd`, `predicates`). `libc` is already a transitive dependency; it may be added directly for `prctl`.
- Constants (spec §4.5, §3): hello timeout `HELLO_TIMEOUT = 30 s`; backoff `BACKOFF_FIRST = 1 s`, doubling to `BACKOFF_MAX = 5 s`, reset after `ALIVE_RESET = 60 s` alive; a device-not-found exit waits `BACKOFF_MAX` at once; shutdown grace `SHUTDOWN_GRACE = 2 s` then `SIGKILL`; the worker's status tick `STATUS_TICK = 1000 ms`; the worker's exit grace after a fault `FAULT_EXIT_GRACE = 200 ms`; a tune's round-trip timeout `RETUNE_ACK + 2 s`; a "waiting" log line once per episode and a dying-respawn line at most once per `60 s`.
- Exit codes of a worker (spec §4.2): 0 clean, 3 `device not found`, 4 `open failed`, 5 `faulted`; a signal or any other code is `died`.
- Fault texts (verbatim, spec §4.5): `device not found: <args>`, `soapy open failed: <last stderr line>`, `soapy worker for <id> died: signal 11 (SIGSEGV)`, `soapy worker for <id> died: exit code N`, `soapy worker for <id>: no hello in 30 s`. Job reasons keep `forward_iq`'s shape `source <id>: <fault>`.
- Secrets never reach the worker: the spawned environment is the parent's minus `ZETA_SECRET`, every `ZETA_NODE_SECRET_*`, every `ZETA_KIWI_PASSWORD_*` and `ZETA_STT_NODE_SECRET`.
- Every task that changes an op result, a flag or field craft regenerates `AGENTS.md` (`cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`cargo run -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and runs `cargo test -p zeta --test agents_md --test transports`.
- Gates per task: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the touched crates' tests. Final gate: `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no `ui e2e skipped`), `agents-md --check`, `agents-md --skill --check`, and `RUSTFLAGS="--cfg soapy" cargo clippy -p zeta --all-targets -- -D warnings` on a host with `libsoapysdr-dev` (the field host or this desktop). Temp files under the job's tmp directory, never `/tmp`; Python via `python3 -I`.
- Commits on branch `plan-20` from `main` (`bd3e186`), one per task, messages in the repo's style (`Worker: ...`, `Jobs: ...`, `Soapy: ...`, `Docs: ...`).

## Review Focus

1. **A dead worker must not stall `tune`.** `source_ensure`/`job_start` on a faulted worker source answer at once with `Io(<fault>)`, never wait the round-trip timeout. Pinned in Task 3 (`tune_during_a_respawn_answers_the_fault_at_once`).
2. **A stopped job stays stopped through a heal.** The operator's `job_stop` before the fault, and a job that failed for its own reason, are untouched by recovery. Pinned in Task 6 (`recovery_leaves_stopped_and_self_failed_jobs_alone`).
3. **A heal in an earlier episode does not reach forward.** A job that failed with the fault, was not relaunched (node restarted with `resume: false`, say) and whose source healed and faulted again is not relaunched by the second heal. Pinned in Task 6 (`a_record_failed_in_an_earlier_episode_is_not_recovered`).
4. **The worker's stdout must never carry a stray line.** A driver that prints to stdout (UHD prints to stderr, but a plugin may not) would corrupt the frame stream; the worker's `serve` owns stdout and the device is opened with stdout already redirected to stderr at the fd level. Pinned in Task 2 (`a_print_to_stdout_in_the_worker_lands_on_stderr`).
5. **Parent exit kills the worker.** A one-shot command or a `zeta serve` ending (SIGINT) leaves no orphan holding the radio. Pinned in Task 3 (`dropping_the_port_ends_the_child_within_the_grace`) and Task 7 (`a_one_shot_source_list_leaves_no_worker_behind`).

---

## File map

- Create `crates/zeta-source-worker/` (`Cargo.toml`, `src/lib.rs`, `src/wire.rs` (frames, control messages, `PortError` wire form), `src/serve.rs` (`serve`), `src/port.rs` (`WorkerPort`, snapshot), `src/supervisor.rs` (spawn/reap/backoff loop), `src/env.rs` (secret scrub), `src/bin/zeta-fake-worker.rs` (test binary behind feature `test-bin`), `tests/wire.rs`, `tests/serve.rs`, `tests/supervisor.rs`, `tests/node.rs`).
- Modify `crates/zeta-core/src/port.rs` (`Port::healed`, `Port::worker`, `WorkerInfo`, `PortError` serde), `src/ops.rs` (`SourceInfo.worker`), `src/node/sources.rs` (`SourceRecord.capabilities`, `pub fn load_source`), `src/node/mod.rs` (`Inner.episodes`, `add_port` subscribes `healed`, `recover_source`, resume refactor, `prune_history` keep rule, launch-failure prefix), `src/node/launch.rs` (`supervise`: attached job `Failed` on an input's source fault), `src/node/watch.rs` (`watch_loop` teardown keyed on `r.port.fault()`), `src/stream_in.rs` (doc only), tests `crates/zeta-core/tests/{resume,attach,watch,ops,recover}.rs`.
- Modify `crates/zeta/Cargo.toml` (dep `zeta-source-worker`), `src/cli.rs` (`Command::SoapyWorker`, `--soapy-in-process`), `src/main.rs` (dispatch), `src/node_setup.rs` (`open_soapy` → `WorkerPort`, startup-absent rule), create `src/soapy_worker.rs` (the subcommand body), tests `crates/zeta/tests/cli.rs`.
- Modify `crates/zeta/src/agents_md.rs`, `README.md`, `docs/agents/field-craft.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md`, `TODO.md`, `AGENTS.md`, `skills/zetasdr/SKILL.md`, `scratch/serve-hackrf-child.sh` (retired: replaced by a comment pointing at the single-node form).

---

### Task 1: The wire format (Sonnet)

**Files:**
- Create: `crates/zeta-source-worker/Cargo.toml`, `src/lib.rs`, `src/wire.rs`
- Modify: `crates/zeta-core/src/port.rs:74-90` (`PortError` derives)
- Modify: `Cargo.toml` (nothing: `members = ["crates/*"]` picks the crate up)
- Test: `crates/zeta-source-worker/tests/wire.rs`

**Interfaces:**
- Produces, in `zeta_source_worker::wire`:
  - `pub const PROTOCOL: u32 = 1;`
  - `pub enum Cmd { Tune { center_hz: u64, bandwidth_hz: u64 }, SetGain { gain_db: Option<f64> }, Shutdown }` and `pub struct Request { pub id: u64, #[serde(flatten)] pub cmd: Cmd }` serialised as spec §4.3 (`{"id": n, "cmd": "tune", "center_hz": .., "bandwidth_hz": ..}`; `serde(tag = "cmd", rename_all = "snake_case")`).
  - `pub struct Hello { pub protocol: u32, pub capabilities: Capabilities, pub gain: Option<GainInfo>, pub driver: String }`
  - `pub struct Status { pub fault: Option<String>, pub current: Option<Tuning>, pub gain: Option<GainInfo>, pub stats: Option<PortStats> }`
  - `pub enum Reply { Tuning(Tuning), Gain(GainInfo) }`
  - `pub enum Control { Hello(Hello), Reply { reply: u64, result: Result<Reply, PortError> }, Status(Status) }` serialised as `{"hello": ..}`, `{"reply": n, "ok": ..}` / `{"reply": n, "err": ..}`, `{"status": ..}` (a custom `Serialize`/`Deserialize` or an untagged enum over three structs; the builder picks).
  - `pub struct BlockHeader { pub t0_ns: u64, pub sample_rate_hz: u64, pub center_hz: u64, pub n: u32 }` (28 bytes LE).
  - `pub enum Frame { Control(Control), Block(IqBlock) }`
  - `pub fn write_frame(w: &mut impl std::io::Write, f: &Frame) -> std::io::Result<()>` (kind `u8` 0/1, `len: u32 LE`, payload) and `pub fn read_frame(r: &mut impl std::io::Read) -> std::io::Result<Option<Frame>>` (`Ok(None)` at a clean EOF, an error for a truncated frame or an unknown kind).
  - `pub fn write_request(w: &mut impl std::io::Write, r: &Request)` (one JSON line) and `pub fn parse_request(line: &str) -> Result<Request, String>`.
- `PortError` gains `Serialize, Deserialize` (`#[serde(tag = "kind", rename_all = "snake_case")]`: `out_of_range {field, value, min, max, suggested}`, `unsupported {message}`, `io {message}`; `field: &'static str` is the one obstacle: change it to `Cow<'static, str>` or deserialise through a leaked/interned string. The builder picks; `Display` output is unchanged, pinned by the test).

- [ ] **Step 1: Write the failing tests** in `tests/wire.rs`:

```rust
#[test]
fn a_block_round_trips_byte_for_byte() {
    // 3 samples, t0 1_760_000_000_000_000_000, rate 2_400_000, centre 1_090_000_000.
    // write_frame → read_frame → the same t0, rate, centre and samples (bit-exact f32).
}
#[test]
fn control_messages_round_trip_in_the_spec_shape() {
    // Hello/Reply(ok Tuning)/Reply(err OutOfRange)/Status: serde_json::to_value matches the
    // literal shapes of spec §4.3, e.g. json!({"reply": 7, "err": {"kind": "out_of_range",
    // "field": "center_hz", "value": 1, "min": 2, "max": 3, "suggested": null}}).
}
#[test]
fn port_error_display_is_unchanged_after_a_round_trip() {
    // for each variant: to_string() before == to_string() after serde round trip; the
    // OutOfRange text is "center_hz 1 out of range 2..=3".
}
#[test]
fn a_truncated_frame_is_an_error_and_a_clean_eof_is_none() {}
#[test]
fn requests_parse_from_one_json_line() {
    // {"id":1,"cmd":"tune","center_hz":144800000,"bandwidth_hz":12500} → Cmd::Tune;
    // {"id":2,"cmd":"set_gain","gain_db":null} → SetGain{None}; an unknown field is an Err.
}
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-source-worker --test wire` → compile errors (no crate / no `wire`).
- [ ] **Step 3: Create the crate** (`Cargo.toml`: deps `serde`, `serde_json`, `thiserror`, `num-complex`, `zeta-core`; `[dev-dependencies]` `tempfile`), `src/lib.rs` (`pub mod wire;`), `src/wire.rs` with the items above; add the derives to `PortError` in `zeta-core`.
- [ ] **Step 4: Run**: `cargo test -p zeta-source-worker --test wire`, `cargo test -p zeta-core` (the `PortError` change touches nothing else), clippy, fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "Worker: the port-over-stdio wire format (JSON requests, framed control and IQ messages); PortError serialises"`.

---

### Task 2: `serve`: a Port over stdio, and the fake worker (Opus)

**Files:**
- Create: `crates/zeta-source-worker/src/serve.rs`, `src/bin/zeta-fake-worker.rs`
- Modify: `crates/zeta-source-worker/Cargo.toml`: optional deps `zeta-source-soapy` and `zeta-source-file`; feature `test-bin = ["dep:zeta-source-soapy", "zeta-source-soapy/test-mock", "dep:zeta-source-file", "zeta-source-file/test-fixtures"]`; `[[bin]] name = "zeta-fake-worker" required-features = ["test-bin"]`; `[dev-dependencies] zeta-source-worker = { path = ".", features = ["test-bin"] }` so `cargo test` builds it. The fake worker parses `std::env::args` by hand (no clap).
- Test: `crates/zeta-source-worker/tests/serve.rs`

**Interfaces:**
- Produces `pub async fn serve(port: Arc<dyn Port>, driver: &str, stdin: impl AsyncRead, stdout: impl Write + Send) -> ExitCode` in `zeta_source_worker::serve`, plus `pub const EXIT_NOT_FOUND: u8 = 3; EXIT_OPEN_FAILED: u8 = 4; EXIT_FAULTED: u8 = 5;` and `pub fn redirect_stdout_to_stderr() -> std::io::Result<RawFd>` (dup2 of fd 1 onto fd 2 after taking a dup of the original fd 1 for the frame writer: spec Review Focus 4).
- `serve` behaviour: writes `Hello { protocol: PROTOCOL, capabilities: port.capabilities().clone(), gain: port.gain(), driver }` first; reads requests line by line; `Tune` → `port.tune(Tuning{..})` → `Reply::Tuning`/`err`; `SetGain` → `port.set_gain` → `Reply::Gain`/`err`; `Shutdown` → return `ExitCode::SUCCESS`; stdin EOF → `SUCCESS`. A block pump task subscribes `port.subscribe()` once and writes `Frame::Block` for every block (a `Lagged` is counted and logged at most every 10 s as `zeta-source-worker: {n} blocks lost`). A status task writes `Status` every `STATUS_TICK` and after every reply; when `port.fault()` is `Some` it writes that status, sleeps `FAULT_EXIT_GRACE`, flushes and returns `ExitCode::from(EXIT_FAULTED)`. All writes to stdout go through one `Mutex<BufWriter>` and are flushed per frame.
- Fake worker (`zeta-fake-worker --id ID --script SCRIPT [--record PATH]`), scripts (spec §5): `stream` (a `SoapySource` over `MockDevice::hackrf_like().with_reads(vec![MockRead::Samples(16_384); usize::MAX/…]` → use the mock's repeat/infinite facility if it has one, else 1_000_000 reads), `gone-after-ms N` (`Samples` for N ms then `MockRead::Gone("unplugged".into())`), `not-found` (exit 3 after the stderr line `device not found: fake`), `open-fails` (exit 4, last stderr line `fake open: no such device`), `crash-after-ms N` (serve normally, a timer calls `std::process::abort()`), `hang-hello` (sleep forever, no hello), `file PATH[,crash-after-ms N]` (a `FileSource` with `FileOpts { realtime: true, block_samples: 16_384, repeat: true, tune_range: Some((centre-2 MHz, centre+2 MHz)) }`, started once served). `--record PATH` appends one line per `set_gain` call (`gain <db|auto>`) and per `tune` (`tune <c> <bw>`); the file is how tests see what the node re-applied.
- Hello of the fake worker reports `driver: "fake"`. Two environment hooks for the supervisor tests: `ZETA_FAKE_CRASH_ONCE=1` makes a `crash-after-ms` script crash only on the first spawn (a marker file beside `--record`'s path says it has crashed); `ZETA_FAKE_PRINT=1` makes it `println!("noise")` after the stdout redirect (Review Focus 4). With `--record`, the fake also writes one `env <NAME>` line per environment variable name at startup.

- [ ] **Step 1: Write the failing tests** in `tests/serve.rs` (spawn `env!("CARGO_BIN_EXE_zeta-fake-worker")` with piped stdio; a helper `fn spawn(script: &str) -> (Child, ChildStdin, ChildStdout)`; a helper `fn read_control(stdout) -> Control` that skips blocks):

```rust
#[test] fn hello_is_the_first_frame_and_carries_the_capabilities() { /* script stream: driver "fake", protocol 1, capabilities.max_bandwidth_hz > 0 */ }
#[test] fn a_tune_is_answered_and_blocks_follow_with_rising_t0() { /* tune 100 MHz / 2 MHz → Reply::Tuning; ≥3 blocks, t0 strictly rising, sample_rate_hz == tuning.bandwidth_hz */ }
#[test] fn a_bad_tune_is_answered_with_the_same_text_as_in_process() { /* centre 10 GHz on hackrf_like → err Display "center_hz 10000000000 out of range ..."; the process stays alive */ }
#[test] fn a_fault_is_published_then_the_worker_exits_5() { /* gone-after-ms 300 → a Status with fault Some(..."unplugged"...) then exit status code 5 within 3 s */ }
#[test] fn stdin_eof_ends_the_worker_cleanly() { /* drop stdin → exit 0 within 2 s */ }
#[test] fn shutdown_ends_the_worker_cleanly() { /* {"id":1,"cmd":"shutdown"} → exit 0 */ }
#[test] fn not_found_and_open_failed_exit_3_and_4_with_a_last_stderr_line() {}
#[test] fn a_print_to_stdout_in_the_worker_lands_on_stderr() { /* script `stream` with env ZETA_FAKE_PRINT=1: the fake calls println!("noise") after redirect; stdout frames still parse, stderr contains "noise" */ }
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-source-worker --test serve` → the binary does not exist.
- [ ] **Step 3: Implement `serve`, the exit codes, `redirect_stdout_to_stderr` and the fake worker.** Tokio runtime inside the bin (`#[tokio::main(flavor = "multi_thread", worker_threads = 2)]`); stdout writes on a `spawn_blocking`-fed channel or a std thread so a slow pipe never blocks the status task's fault check.
- [ ] **Step 4: Run**: `cargo test -p zeta-source-worker --test serve`, clippy (`--all-features` builds the bin), fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "Worker: serve any Port over stdio (hello, replies, status, IQ frames; exit 5 on a fault); the zeta-fake-worker test binary"`.

---

### Task 3: `WorkerPort` and its supervisor (Opus)

**Files:**
- Create: `crates/zeta-source-worker/src/port.rs`, `src/supervisor.rs`, `src/env.rs`
- Modify: `crates/zeta-core/src/port.rs` (`Port::healed`, `Port::worker`, `WorkerInfo`)
- Test: `crates/zeta-source-worker/tests/supervisor.rs`

**Interfaces:**
- In `zeta-core::port`: `pub struct WorkerInfo { pub pid: Option<u32>, pub restarts: u64, pub since_ns: Option<u64> }` (Serialize, Deserialize, JsonSchema, `deny_unknown_fields`; `pid` and `since_ns` nullable-required like `PortStats.measured_rate_hz`); on `Port`: `fn healed(&self) -> Option<tokio::sync::watch::Receiver<u64>> { None }` and `fn worker(&self) -> Option<WorkerInfo> { None }`.
- `pub struct WorkerConfig { pub id: SourceId, pub program: PathBuf, pub args: Vec<String>, pub timings: Timings }` with `pub struct Timings { hello: Duration, backoff_first: Duration, backoff_max: Duration, alive_reset: Duration, shutdown_grace: Duration }` and `Timings::default()` = the Global Constraints values; `impl Default`.
- `pub enum Attach { Hello, NotFound { args: String }, Refused(String) }`: what the first spawn said.
- `pub async fn WorkerPort::open(cfg: WorkerConfig, cached: Option<Capabilities>) -> Result<(Arc<WorkerPort>, Attach), String>`: spawns once; on hello returns `Attach::Hello`; on exit 3 with `cached` `Some` returns `Attach::NotFound` and a port whose capabilities are `cached`, faulted `device not found: <args>` and already polling; on exit 3 with no cache, exit 4, death or no hello returns `Err(<fault text>)` after reaping the child (so a refused flag leaves no process).
- `WorkerPort: Port` per spec §4.4: `tune` validates centre and bandwidth against capabilities first (`out_of_range` texts as `SoapySource`: `center_hz <v> out of range <min>..=<max>`, `bandwidth_hz <v> out of range 1..=<max>`), then if no live worker → `Err(Io(<fault>))` at once, else the round trip under a `tokio::sync::Mutex` gate with timeout `RETUNE_ACK + 2 s` (`zeta_source_soapy` is not a dependency: copy the 5 s value as `pub const TUNE_TIMEOUT: Duration = Duration::from_secs(7)` with a comment); `fault()`, `current()`, `gain()`, `stats()` from the snapshot; `healed()` returns a clone of the watch receiver; `worker()` returns `WorkerInfo`.
- `pub fn env::scrubbed() -> Vec<(OsString, OsString)>`: the parent's environment minus the Global Constraints' secret names.
- Supervisor (spec §4.5): one task; states `Starting`, `Live { child, pid, since }`, `Waiting { fault, next_at }`; `restarts` increments on each spawn after the first; the `healed` watch is bumped on a hello that follows a fault; log lines `zeta: soapy worker for <id>: <fault>; waiting for the device` (once per episode) and `zeta: soapy worker for <id>: back after <n> attempts, <s> s`; worker stderr lines are forwarded to the parent's stderr prefixed `zeta-soapy-worker[<id>]: `. `impl Drop for WorkerPort`: cancel the supervisor; the supervisor's drop path sends `Shutdown`, waits `shutdown_grace`, then `kill()` and reaps.

- [ ] **Step 1: Write the failing tests** in `tests/supervisor.rs` (helper `fn cfg(script: &str) -> WorkerConfig` pointing `program` at the fake worker, `timings` shortened: hello 3 s, backoff 100 ms / 300 ms, alive reset 2 s, grace 500 ms):

```rust
#[tokio::test] async fn hello_attaches_and_a_tune_streams_blocks() { /* Attach::Hello; tune; subscribe → blocks; worker().pid Some, restarts 0 */ }
#[tokio::test] async fn a_crash_faults_names_the_signal_and_a_respawn_heals_once() { /* crash-after-ms 500 with ZETA_FAKE_CRASH_ONCE=1: fault() == Some("soapy worker for f died: signal 6 (SIGABRT)") within 1 s; then the respawn says hello: healed() goes 0→1 exactly once, fault() None, worker().restarts == 1 */ }
#[tokio::test] async fn not_found_with_a_cache_attaches_faulted_and_polls_at_the_cap() { /* Attach::NotFound; fault "device not found: ..."; spawns ≈ every backoff_max (count via --record lines over 1 s) */ }
#[tokio::test] async fn not_found_without_a_cache_is_refused_and_leaves_no_child() {}
#[tokio::test] async fn a_hang_before_hello_is_killed_at_the_timeout() { /* fault "soapy worker for f: no hello in 3 s" (the text uses the configured seconds) */ }
#[tokio::test] async fn tune_during_a_respawn_answers_the_fault_at_once() { /* after the crash, tune() returns Err(Io(fault)) in < 100 ms */ }
#[tokio::test] async fn backoff_doubles_to_the_cap_and_resets_after_alive() { /* observe spawn times from --record: gaps ≈ 100, 200, 300, 300 ms */ }
#[tokio::test] async fn the_child_environment_has_no_secrets() { /* set ZETA_SECRET, ZETA_NODE_SECRET_X, ZETA_KIWI_PASSWORD_Y, ZETA_STT_NODE_SECRET, HOME; fake with --record dumps env names; none of the four present, HOME present */ }
#[tokio::test] async fn dropping_the_port_ends_the_child_within_the_grace() { /* pid gone within grace + 200 ms */ }
#[tokio::test] async fn a_bad_band_is_refused_without_a_round_trip() { /* centre out of range → OutOfRange even while the worker is down */ }
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-source-worker --test supervisor` → compile errors.
- [ ] **Step 3: Implement** `WorkerInfo` and the two `Port` methods in `zeta-core`, then `env.rs`, `supervisor.rs`, `port.rs`. Reading stdout: a `std::thread` running `read_frame` in a loop, forwarding to the tokio side through an `mpsc` (blocks) and updating the snapshot (`Arc<Mutex<Status>>`) directly. Reaping: `tokio::process::Child::wait`. Signal names: `libc::strsignal`-free mapping of the few that matter (`SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGKILL`, `SIGTERM`), others as `signal N`.
- [ ] **Step 4: Run**: `cargo test -p zeta-source-worker`, `cargo test -p zeta-core --test ops` (schema unchanged so far), clippy, fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "Worker: WorkerPort spawns and supervises a port-over-stdio child (hello timeout, exit-code faults, bounded backoff, healed counter); Port::healed and Port::worker"`.

---

### Task 4: `source_list.worker` and the cached capability record (Sonnet)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs:1076-1104` (`SourceInfo.worker`), the `source_list` builder (`ops.rs` near `:1928`, `node/mod.rs` near `:1646`), `crates/zeta-core/src/node/sources.rs` (`SourceRecord.capabilities`, `pub fn load_source(dir, id) -> Option<SourceRecord>`), `crates/zeta-core/src/node/mod.rs:902-918` (`add_port` writes the record's `capabilities` for a port with `worker().is_some()`)
- Test: `crates/zeta-core/tests/ops.rs` (schema), `crates/zeta-core/tests/gain.rs` or a new `tests/sources_record.rs`

**Interfaces:**
- `SourceInfo.worker: Option<WorkerInfo>` with `#[schemars(required, schema_with = "crate::types::nullable::<WorkerInfo>")]`, doc: "The child process the source runs in (`Port::worker`): its pid, how many times it was respawned, when the live one said hello; `null` for a source that runs in the node."
- `SourceRecord.capabilities: Option<Capabilities>` (`#[serde(default)]`, format stays 1); `pub fn load_source(dir: &Path, id: &SourceId) -> Option<SourceRecord>` (one file; `None` when missing or unreadable).
- The node writes `capabilities` into the record in `add_port` when `port.worker().is_some()`, and again on every `healed` bump (Task 6 calls the same `fn note_capabilities(&self, port: &dyn Port)`).

- [ ] **Step 1: Write the failing tests**: in `tests/ops.rs` the schema test that pins `SourceInfo` required fields gains `worker` (nullable `WorkerInfo` with `pid`, `restarts`, `since_ns`); in a new `tests/sources_record.rs`: `a_worker_port_writes_its_capabilities_to_the_source_record` (a fixture port whose `worker()` is `Some(WorkerInfo{pid: Some(1), restarts: 0, since_ns: None})` → after `Node::open`, `load_source(&state/sources, &id).unwrap().capabilities == Some(port.capabilities().clone())`), `a_plain_port_writes_no_capabilities` (a `FileSource` → `capabilities` `None`, the record otherwise as before), `source_list_shows_worker_for_a_worker_port_and_null_otherwise`.
- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-core --features test-fixtures --test ops --test sources_record`.
- [ ] **Step 3: Implement** the field, the record field, `load_source`, `note_capabilities`.
- [ ] **Step 4: Regenerate** `AGENTS.md` and the skill (the `source_list` paragraph names `worker`), run `cargo test -p zeta --test agents_md --test transports`, `cargo test -p zeta-core --features test-fixtures`, clippy, fmt → green. The UI reads `source_list` as JSON and ignores unknown fields; run `cargo test -p zeta --test ui` once to confirm.
- [ ] **Step 5: Commit**: `git commit -m "Sources: source_list.worker (pid, restarts, since_ns) and the source record caches a worker port's capabilities"`.

---

### Task 5: Faults carry the source's name everywhere; attached jobs and the watch (Opus)

**Files:**
- Modify: `crates/zeta-core/src/node/mod.rs:722-727` (the launch-failure reason), `crates/zeta-core/src/node/launch.rs:97-` (`launch_inner`'s `ensure_tuned` error mapping, the `job_start` path), `:870-915` (`supervise`: an attached job's end), `crates/zeta-core/src/node/watch.rs:976-990` (`teardown`), `crates/zeta-core/src/stream_in.rs:34-37` (doc)
- Test: `crates/zeta-core/tests/resume.rs`, `tests/attach.rs`, `tests/watch.rs`

**Interfaces:**
- Produces `pub(crate) fn source_fault_reason(source: &SourceId, e: &dyn Display) -> String` = `format!("source {}: {e}", source.0)` in `node/mod.rs`, used by: the resume launch failure (`mod.rs:722-727`, only when the error is `NodeError::Lease(LeaseError::Port(_))`; `UnknownSource` keeps `source <id> not present`), `forward_iq` (already this shape; switch it to the helper), the scan loop and the watch loop's `faulted` closures.
- `supervise`: when the ending job is attached (`rec.input.is_some()`), the run's failure is `None` (its `stream_in` ended `Ok`) **and** the input job's record is now `Failed { reason }` with `reason.starts_with("source ")` and the input's source port reports `fault().is_some()`, the attached job is recorded `Failed { reason: NodeError::InputNotRunning(input).to_string() }` instead of `Stopped`. Every other attached end stays `Stopped`.
- `watch_loop`: `let port_faulted = r.port.fault().is_some(); let teardown = !shutdown && !inner.restarting(&r.id) && !port_faulted;` and the doc comment lists the fourth case: a port fault leaves the decoders to fail on their own, for recovery to relaunch and the next run to re-adopt.

- [ ] **Step 1: Write the failing tests**:

```rust
// tests/resume.rs
#[tokio::test] async fn a_record_resumed_against_a_faulted_port_fails_naming_the_source() {
    // a fixture port whose tune() returns Err(Io("device not found: x")) → the resumed
    // record is Failed { reason: "source s: io: device not found: x" }
}
// tests/attach.rs
#[tokio::test] async fn an_attached_job_whose_input_died_of_a_source_fault_fails_input_not_running() {
    // fm_demod on a faultable fixture port + transcribe attached (the fake speech backend as
    // the existing attach tests do); set the fault → the demod Failed "source s: <fault>",
    // the attached job Failed "input job <id> not running" (NodeError::InputNotRunning text)
}
#[tokio::test] async fn an_attached_job_whose_input_was_stopped_stays_stopped() { /* job_stop the input → attached Stopped (today's behaviour) */ }
// tests/watch.rs
#[tokio::test] async fn a_watch_whose_port_faults_leaves_its_decoders_to_fail_on_their_own() {
    // the watch harness with a faultable wrapper port, a proved decoder in `decoding`;
    // set the fault → watch Failed "source s: <fault>", decoder Failed with the same prefix
    // (not Stopped), no `lost` event published, job_context.watch.decoding still lists it
}
```
  The faultable wrapper: `tests/watch.rs` already wraps a `FileSource` in a port whose `fault()` delegates to an inner `Mutex<Option<String>>`; move it to a shared `tests/common/faultable.rs` (or `zeta_core::test_fixtures` behind `test-fixtures`) as `pub struct FaultablePort { inner: Arc<FileSource>, fault: Mutex<Option<String>>, healed: watch::Sender<u64> }` with `set_fault(Option<String>)` and `heal()` (clears the fault, bumps `healed`) so Task 6 reuses it; its `tune()` returns `Err(Io(fault))` while faulted.
- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-core --features test-fixtures --test resume --test attach --test watch <names>`.
- [ ] **Step 3: Implement** the helper, the three call sites, the attached-job rule in `supervise`, the watch teardown condition.
- [ ] **Step 4: Run** `cargo test -p zeta-core --features test-fixtures`, `cargo test -p zeta --test attach --test resume`, clippy, fmt → green (existing `resume` tests that pin the bare reason text, if any, are updated to the prefixed one).
- [ ] **Step 5: Commit**: `git commit -m "Jobs: every port fault fails a job as 'source <id>: ...'; an attached job whose input died of one fails 'input job ... not running'; a faulted watch leaves its decoders to fail on their own"`.

---

### Task 6: `recover_source`: the resume for one source, run when its port heals (Opus, higher effort)

**Files:**
- Modify: `crates/zeta-core/src/node/mod.rs` (`Inner.episodes: Mutex<HashMap<SourceId, Episode>>`, `add_port` spawns `watch_healed`, `Node::open`'s resume refactored into `resume_records(records, sources: Option<&SourceId>)`, `recover_source`, `prune_history`), `crates/zeta-core/src/node/launch.rs` (nothing new; `launch` is reused)
- Test: `crates/zeta-core/tests/recover.rs` (new)

**Interfaces:**
- `struct Episode { since_ns: u64 }`: present while a source is known faulted. Opened by `note_fault(source)` (called from `supervise`'s `finish` when the reason has the `source <id>: ` prefix, from `source_list` when `port.fault()` is `Some`, and from `job_start`/`source_ensure` when the lease error is a port fault); closed by `recover_source`.
- `fn recoverable(&self, rec: &JobRecord, ep: &Episode) -> bool`: `rec.input.is_none() && matches!(rec.state, Failed{reason} if reason.starts_with(&format!("source {}: ", rec.source.0))) && entry.ended_ns.0 >= ep.since_ns` (the `JobEntry.ended_ns` of the record's entry), plus attached records `Failed { reason == InputNotRunning(input) }` whose `input` is recoverable or running.
- `pub(crate) async fn recover_source(self: &Arc<Inner>, id: &SourceId)`: under the source's gate: (1) `note_capabilities(port)`; (2) gain from `sources/<id>.json` → `port.set_gain` (a refusal logged, not fatal); (3) pin from the record → `leases.pin(port, t)` (a failure logged; the pin stays in the record); (4) the recoverable records of this source in resume order (`input.is_some()`, `elastic`, `kind != scan`, `Reverse(bandwidth)`, `created_ns`), `resume_captures` for this one source, `launch(rec)` each (a launch error leaves the record `Failed` with the new reason), release the capture hold; (5) attached recoverable records; (6) close the episode; (7) `eprintln!("zeta: source {id}: recovered: pin {c}/{bw} Hz, gain {g} dB, {n} jobs relaunched")` (omit the pin/gain parts when there are none).
- `add_port`: if `port.healed()` is `Some(rx)`, `tokio::spawn(watch_healed(inner, id, rx))`: on each change with value `> 0`, `recover_source`.
- `prune_history`: a terminal record that is recoverable in an open episode is excluded from the prune candidates.
- `Node::open`: the per-record launch loop and the `resume_captures` call move into `resume_records(&self, records: Vec<JobRecord>, only: Option<&SourceId>) -> Vec<(JobId, Arc<dyn Port>)>` so `recover_source` and `open` share one body (the `resumable` check stays `open`'s; `recover_source` filters with `recoverable`).

- [ ] **Step 1: Write the failing tests** in `tests/recover.rs` with `FaultablePort` (Task 5) over the `pocsag`-bearing fixture or a plain tone:

```rust
#[tokio::test] async fn a_heal_restores_pin_gain_and_relaunches_the_failed_jobs_under_their_ids() {
    // pin 1.024 MHz, set gain 20 (FaultablePort reports a GainInfo and records set_gain),
    // job_start passthrough A and measure B; set_fault("no samples for 1000 ms (device
    // unplugged?)") → A, B Failed "source s: no samples ..."; source_list.fault Some;
    // heal() → within 2 s: A, B Running (same ids), source_list.pin Some(the band),
    // the port saw set_gain(Some(20.0)) again, fault None, debug_stage_count() == pre-fault.
}
#[tokio::test] async fn recovery_leaves_stopped_and_self_failed_jobs_alone() {
    // C stopped by job_stop before the fault; D failed for its own reason (a graph with a
    // stage that fails: the "watch: test failure" trick or a bad param) → both unchanged.
}
#[tokio::test] async fn a_record_failed_in_an_earlier_episode_is_not_recovered() {
    // Open a node with resume: false over a state dir holding a record A that is Failed
    // "source s: no samples ..." from before (written by a first node that faulted and was
    // dropped): no episode is open for it. Then set_fault + heal() on the live port → A stays
    // Failed; a job B started and failed inside this episode is relaunched.
}
#[tokio::test] async fn history_pruning_keeps_recoverable_records() { /* 25 terminal records, 2 of them recoverable in an open episode → both still listed */ }
#[tokio::test] async fn an_attached_job_comes_back_with_its_input() { /* fm_demod + transcribe (fake speech) → fault → both Failed → heal → both Running */ }
#[tokio::test] async fn a_watch_and_its_decoder_come_back_and_nothing_is_lost() {
    // watch harness (tests/watch.rs fixtures over FaultablePort), decoding a proved decoder;
    // fault → both Failed; heal → within 10 s both Running, job_context.watch.decoding names
    // the decoder's id, no `lost` event on the watch's stream since the heal.
}
#[tokio::test] async fn source_list_shows_the_episode_fault_then_clears() {}
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-core --features test-fixtures --test recover`.
- [ ] **Step 3: Implement** `Episode`, `note_fault`, `recoverable`, the `resume_records` refactor, `recover_source`, `watch_healed`, the prune rule. If the watch test shows `ADOPT_WAIT` (5 s) is too short because the decoders launch after the watch, `recover_source` launches a recovered watch's `decoding` jobs (read from its persisted `context.watch.decoding`) before the watch itself; record which in the commit message.
- [ ] **Step 4: Run** `cargo test -p zeta-core --features test-fixtures` (all; `resume.rs` must stay green after the refactor), `cargo test -p zeta --test resume --test bank_resume`, clippy, fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "Jobs: a source that heals is recovered (gain, pin, then every job that failed with its fault in this episode, attached jobs and the watch's decoders included); recoverable records survive history pruning"`.

---

### Task 7: `zeta soapy-worker`, the node attaches Soapy sources through a worker, startup with the device absent (Opus)

**Files:**
- Create: `crates/zeta/src/soapy_worker.rs`
- Modify: `crates/zeta/Cargo.toml` (dep `zeta-source-worker`; `libc` if `prctl` is done here), `src/cli.rs:215-226` (`Command::SoapyWorker(SoapyWorkerArgs)` with `#[command(hide = true)]`; global `--soapy-in-process: bool`), `src/main.rs:40-48` (dispatch before `transport::dispatch`, like `Soapy(Find)`), `src/node_setup.rs:173-185, 379-381` (`open_soapy` → async, builds a `WorkerPort` unless `--soapy-in-process`), `src/agents_md.rs` (the `--source-soapy` paragraph, Task 8 finishes the prose)
- Test: `crates/zeta/tests/cli.rs`, `crates/zeta-source-worker/tests/node.rs`

**Interfaces:**
- `pub struct SoapyWorkerArgs { #[arg(long)] id: String, #[arg(long)] args: String }`; `pub async fn soapy_worker::run(args: SoapyWorkerArgs) -> ExitCode`: `cfg(soapy)`: `redirect_stdout_to_stderr()`, `prctl(PR_SET_PDEATHSIG, SIGTERM)` (Linux), SIGTERM → the same path as `Shutdown`; enumerate with the driver keys of `args` (`split_zeta_args` then `find(&rest)`); empty → stderr `device not found: <args>`, exit 3; `zeta_source_soapy_driver::open` → `Err(e)` → stderr `e`, exit 4; `Ok(src)` → `serve(Arc::new(src), &driver_key, stdin, stdout)`. `cfg(not(soapy))`: stderr `zeta: soapy-worker: {NO_SOAPY}`, exit 1.
- `node_setup::open_soapy(id, args, in_process: bool, state_dir: &Path) -> Result<Arc<dyn Port>, String>`: `in_process` → today's path; else `WorkerPort::open(WorkerConfig { id, program: std::env::current_exe()?, args: ["soapy-worker", "--id", id, "--args", args], timings: default }, zeta_core::node::sources::load_source(&state_dir.join("sources"), &id).and_then(|r| r.capabilities))`: `Attach::Hello | Attach::NotFound` → the port; `Err(e)` → the flag error. The `NotFound`-without-cache text: `device not found: <args> (never seen on this host; plug it in once)`.
- Without `cfg(soapy)` the node still refuses `--source-soapy` with `NO_SOAPY` (the worker would exit 1 at once; check the cfg before spawning).

- [ ] **Step 1: Write the failing tests**:

```rust
// crates/zeta/tests/cli.rs
#[test] fn soapy_worker_without_the_cfg_exits_1_naming_the_rebuild() { /* `zeta soapy-worker --id x --args driver=hackrf` → exit 1, stderr contains "built without soapy" (this test runs in the workspace build, which has no cfg(soapy)) */ }
#[test] fn soapy_worker_is_hidden_from_help() { /* `zeta --help` stdout lacks "soapy-worker"; `zeta soapy-worker --help` works */ }
#[test] fn soapy_in_process_is_accepted_as_a_global_flag() { /* `zeta --soapy-in-process source list` with a file source → exit 0 */ }
// crates/zeta-source-worker/tests/node.rs (dev-deps: zeta-core test-fixtures, zeta-dsp, zeta-source-file test-fixtures)
#[tokio::test] async fn a_node_over_a_crashing_fake_worker_recovers_its_jobs() {
    // Node::open with WorkerPort over `file <pocsag fixture>,crash-after-ms 1500`,
    // ZETA_FAKE_CRASH_ONCE=1; pin + passthrough; the job fails "source f: soapy worker for f
    // died: signal 6 (SIGABRT)"; worker.restarts 1; the job Running again within 5 s; the
    // pin back; stages_running back to pre-crash.
}
#[tokio::test] async fn startup_with_the_device_absent_attaches_faulted_when_seen_before() {
    // sources/f.json with capabilities (written by a prior open over `stream`) + script
    // `not-found` → Attach::NotFound, source_list.fault "device not found: fake"
}
#[tokio::test] async fn a_one_shot_source_list_leaves_no_worker_behind() {
    // spawn `zeta --state-dir D --source-soapy ...`? no cfg(soapy) in tests: instead drop the
    // Node and assert the fake worker's pid is gone within the grace (the Drop path end to end)
}
```

- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement** the subcommand, the flag, the dispatch, `open_soapy`. `open_with` becomes able to call the async `WorkerPort::open` (it is already `async`).
- [ ] **Step 4: Run** `cargo test -p zeta --test cli`, `cargo test -p zeta-source-worker`, the agents-md regeneration (the flag list changed), clippy, fmt → green. On a host with `libsoapysdr-dev`: `RUSTFLAGS="--cfg soapy" cargo clippy -p zeta --all-targets -- -D warnings` and `RUSTFLAGS="--cfg soapy" cargo build -p zeta --release`, then with a HackRF attached and nothing else using it: `./target/release/zeta --state-dir <tmp> --source-soapy h=driver=hackrf source list` shows `worker.pid`; `kill -SEGV <that pid>` while `zeta serve` runs a `passthrough` on it → the job fails naming `signal 11 (SIGSEGV)` and is `Running` again within 10 s. Record the result in the commit message (or `skipped: no device` if none).
- [ ] **Step 5: Commit**: `git commit -m "Soapy: the driver runs in a 'zeta soapy-worker' child the node spawns and supervises; --soapy-in-process keeps the old path; a device absent at startup attaches faulted when seen before"`.

---

### Task 8: Field craft, docs, TODO and the plan series (Sonnet)

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` (the `--source-soapy` paragraph; "Reading a source's health" gains `worker`; "A radio in its own process" rewritten as "A radio that comes back" per spec §4.9; the `source_list` paragraph names `worker`), `README.md`, `docs/agents/field-craft.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 20 **Merged**/status, a "Plan 20 status" section with the acceptance checklist of spec §6 left as unchecked items for the live run), `TODO.md` (the sections "Bug: a USB drop of a SoapySDR device segfaults the node", "Bug: a node that loses its only device stays up faulted..." replaced by one line each pointing at the spec; the two-node mention under "Bug: stopped jobs..." left), `scratch/serve-hackrf-child.sh` (body replaced by a comment: retired by plan 20, run one node with `--source-soapy`), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `cargo test -p zeta --test agents_md --test transports`

- [ ] **Step 1: Write the prose.** The `--source-soapy` paragraph says: the device is opened in a child process `zeta soapy-worker` the node spawns (the same executable); a driver crash or an unplugged device faults the source (`source list` shows `fault`, jobs on it fail with `source <id>: <fault>`) and never ends the node; the node then respawns the worker, waits for the device, and when it is back restores the pin and the gain and relaunches every job that failed with that fault under its own id, the `watch` and its decoders and an attached `transcribe` included, logging `zeta: source <id>: recovered: ...`; `worker` in `source list` shows the child's pid and how many times it was respawned; a device absent at startup attaches faulted when the host has seen it before, else the flag is refused; `--soapy-in-process` is for diagnosing the worker only. Field craft: "A radio that comes back": what to expect on a cable pull, how to read `worker.restarts`, that nothing needs restarting, that the two-node form is federation and no longer the crash answer.
- [ ] **Step 2: Regenerate** `AGENTS.md` and the skill; `cargo run -p zeta -- agents-md --check`, `--skill --check`, `cargo test -p zeta --test agents_md --test transports` → green.
- [ ] **Step 3: Final gate**: `cargo test --workspace --all-features --no-fail-fast` → no `FAILED`, no `ui e2e skipped`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo fmt --all -- --check`.
- [ ] **Step 4: Commit**: `git commit -m "Docs: plan 20 field craft (a radio that comes back), README, TODO, plan series status; the two-node HackRF launcher retired"`.

---

## After the plan (not tasks: the session does these)

1. Whole-branch review, then merge `plan-20` into `main` (fast-forward).
2. The field host: sync `main` to `~/zetasdr/src20` under the `sdr` account, `RUSTFLAGS="--cfg soapy" cargo build -p zeta --release` there (the operator is making room for the target directory), copy to `~/zetasdr/plan20-zeta`, point the unit's `ExecStart` at it, `systemctl --user restart zeta`, then spec §6 items 1-2 (`kill -SEGV` of the worker during a running watch), item 3 only with the operator at the machine. Record the outcomes in "Plan 20 status".
3. Update the memory file for the HackRF field setup (the single-node form replaces the two-node launchers).
