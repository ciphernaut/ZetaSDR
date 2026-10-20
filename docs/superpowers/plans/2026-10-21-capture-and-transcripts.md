# Capture and Transcripts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A restart never costs more than a fresh start (resumed channels get the shared bank, and the node says which path a channel took and which stage kinds lag); no capture the node leases yields a chain the resampler refuses (a UHD source asks for exact divisors of its master clock, and the resampler snaps rather than refuses); every `transcript` event is on disk (`transcripts.log`, `zeta transcripts tail`); a source can be pinned to a band (`source_pin` / `source_unpin`, persisted, restored first on resume, a rigid holder that is not a job); the receive gain is set live (`source_set_gain`, shown in `source_list`, persisted); optionally a `ppm=` correction and a squelch in the NBFM and AM demodulators.

**Architecture:** In `zeta-core`, `NodeConfig` carries the bank factory so `Node::open`'s resume sees it; `StageContext` gains a `note` callback into the job's context; `JsonLog` (today's `OpsLog`) gains a second file, `transcripts.log`, fed by a per-job tee on `transcribe` jobs' event streams; `LeaseManager` gains a pin flag under the holder id `pin:<source>`; a per-source record `sources/<id>.json` holds the pin and the gain; `Port` gains `gain`/`set_gain`. In `zeta-dsp`, `Resampler::new` snaps above a factor of 2 048, and the lag line names the stage kind. In `zeta-source-soapy`, `pick_rate` takes a UHD ladder, `tune` guards the actual rate, and the device seam gains the gain calls (mock and driver). In the page, pins are holders and outlines, and `source pin|unpin|gain` are commands.

**Tech Stack:** Rust 2024 (`zeta-core`, `zeta-dsp`, `zeta-source-soapy`, `zeta-source-soapy-driver` (out of the workspace), `zeta`, `zeta-ws`), vanilla JS (`crates/zeta-ui/web`), Playwright scenarios in `crates/zeta-ui/tests/e2e/ui.py` driven by `crates/zeta/tests/ui.rs`.

**Spec:** `docs/superpowers/specs/2026-10-21-capture-and-transcripts-design.md` (3.1 rates, 3.2 transcript log, 3.3 pins, 3.4 gain, 3.5 ppm, 3.6 source records, 3.7 squelch, 3.8 resumed jobs and the bank; 4 page; 5 CLI; 6 constraints; 8 open questions; 10 calls made).

## Global Constraints

- `zeta-core` gains no dependency. `JobRecord` is unchanged. The new `<state_dir>/sources/<id>.json` is format 1 (`{"format": 1, "pin": Tuning | null, "gain": {"db": number | null} | null}`), written atomically as job records are (`persist.rs:22-29`), every later field `#[serde(default)]`.
- The guide's rule holds: every field of an op is required and unknown fields are rejected. New ops: `source_pin { source, center_hz, bandwidth_hz, replace }`, `source_unpin { source }`, `source_set_gain { source, gain_db: number | null }`. `source_list` items gain `pin: Tuning | null`, `gain: GainInfo | null`, `bank: {"bin_hz", "consumers"} | null`. The optional Task 9 adds `squelch_db: integer | null` to `job_start`; every in-repo caller then sends it. Every task that changes an op, result, flag or note regenerates `AGENTS.md` (`cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`--skill`) and runs `cargo test -p zeta --test agents_md --test transports`.
- A pin is a rigid lease under `JobId("pin:<source id>")` with `pin: true`; it is never in `holders` (op result or conflict), `replace` naming it is `unknown_job`, and a conflict on a pinned source carries the option `unpin source <id> (source_unpin)` before `retry with replace: [...]`. `source_unpin` on an unpinned source is `other` `source <id> is not pinned`.
- Gain: `GainInfo { db: number | null, min_db, max_db, automatic: bool, automatic_available: bool }`; `null` turns automatic gain on, else `other` `source <id> has no automatic gain`; a value outside the range is `other` `gain_db <db> out of range <min>..=<max> dB for source <id>`; a port without adjustable gain answers `other` `source has no adjustable gain`. `_db` is a unit suffix (decibels) in the guide's rules and `agents_md::unit`.
- Resampler: exact when the reduced ratio's larger term is at most `SNAP_FACTOR` (2 048); above it, the nearest `in'` (searching `in + 1`, `in - 1`, `in + 2`, ...) within `SNAP_MAX_PPM` (1 000) whose ratio to `out` reduces under the factor; past that the existing error text. One stderr line per snap: `zeta-dsp: resampler <in> -> <out> Hz snapped to <in'> (<ppm> ppm)` (`ppm` as `{:.0}`).
- UHD ladder: `mcr` = the probed range's maximum, `min` its minimum, rungs `mcr / N` for `N` in `1..=min(512, mcr / min)` with `mcr % N == 0`; the smallest rung at least `bandwidth_hz`, `None` above `mcr`. Actual-rate guard on every device: an actual rate below `bandwidth_hz` is re-asked once at the next rung (or 100 kHz quantum) above the actual, then `PortError::Unsupported("soapy: device gives <actual> Hz for <requested> Hz, below the <bandwidth_hz> Hz band")`.
- Transcript log line: `{"t_ns", "job", "input", "source", "center_hz", "bandwidth_hz", "start_ns", "end_ns", "text", "language", "dropped", "error"}` exactly, in `<state_dir>/transcripts.log`, rotated at `8 * 1024 * 1024` bytes to `transcripts.log.1`; a tee lag is one `persist_errors` entry `transcripts.log: job <id>: <n> events skipped`; a write failure one entry `transcripts.log: <e>`. `zeta transcripts tail [--lines N] [--job ID]` (default 50); with `--node`: stderr `transcripts tail reads the state directory: not supported with --node`, exit 1.
- Lag lines read `zeta-dsp: <kind> stage lagged, <n> frames skipped`; `zeta_dsp::lagged_frames()` is unchanged and `zeta_dsp::lagged_frames_by_kind() -> Vec<(&'static str, u64)>` is new.
- Gates per task: `cargo fmt -p <touched crates> -- --check` (the `--all` form fails in a nested worktree), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the crate's tests (`--features test-fixtures` / `test-mock` where gated); UI tasks run `cargo test -p zeta --test ui` with no `ui e2e skipped` line within its 300 s deadline (plan 11 left 234 s used: at most two new scenarios, each under 10 s). Final gate: `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no skip line), `agents-md --check` and `--skill --check`. The driver crate is built on its own (`cargo check -p zeta-source-soapy-driver` in its directory, `cfg(soapy)` host) when its seam changes. Temp files under the job's tmp directory, never `/tmp`; Python via `python3 -I` except the Playwright runner.

## Review Focus

1. Resume with the bank factory in `NodeConfig`: every resumed `channel` is bank-served, no lag line, the same consumer count as a fresh start; pinned in Task 1 (`resumed_channels_take_the_bank_like_fresh_ones`). This is the confirmed live regression; it ships first.
2. A pinned source with a persisted running `scan`: the pin is restored first, the scan fails with the `conflict` reason carrying `unpin source`; nothing spins. Pinned in Task 5 (`pin_resumes_before_the_records_and_a_scan_fails_naming_it`).
3. The UHD ladder when the mock rounds the rate down below the band: one retry a rung up, then the exact `Unsupported` text; the lease never holds a capture narrower than the job's band. Pinned in Task 3 (`actual_rate_below_the_band_retries_once_then_refuses`).
4. The transcript tee never outlives its job and never ends it: a stopped `transcribe` job leaves no task, 80 tees share one lock, a lagging tee records once and continues. Pinned in Task 4 (`tee_ends_with_the_job_and_a_lag_is_recorded_once`).
5. `set_gain` through the reader thread while a retune is queued: both apply in order, the ack carries the new `GainInfo`; on a faulted reader the call goes straight to the device. Pinned in Task 6 (`gain_and_retune_keep_their_order_on_the_reader`).

---

## File map

- Modify `crates/zeta-core/src/node/mod.rs` (`NodeConfig.bank_factory`, `SourceRecord` load/apply, pins at open, `source_pin`/`source_unpin`/`source_set_gain` node methods, `job_context_merge` reuse for the note), `src/node/launch.rs` (note callback in `StageContext`, the transcript tee, `feed_bank` unchanged), `src/node/opslog.rs` → `jsonlog.rs` (`JsonLog`), new `src/node/sources.rs` (the per-source record), `src/stage.rs` (`StageContext.note`), `src/lease.rs` (`Lease.pin`, `pin_locked`, `unpin`, `pinned`, `conflict_options`), `src/ops.rs` (`Op::SourcePin`, `SourceUnpin`, `SourceSetGain`, `OpResult::Gain`, `SourceInfo.pin/.gain/.bank`, `GainInfo`), `src/port.rs` (`gain`, `set_gain`, `GainInfo`), `src/bank.rs` (unchanged API).
- Modify `crates/zeta-dsp/src/resample.rs` (snap), `src/lib.rs` (`run_map` kind, `lagged_frames_by_kind`), `src/channel.rs` (kind, context note, `run_bin` kind), every `run_map` caller (kind argument), optionally `src/nbfm.rs`, `src/am.rs` (squelch).
- Modify `crates/zeta-source-soapy/src/caps.rs` (`RateModel`, `uhd_ladder`, `pick_rate`), `src/source.rs` (`Cmd` enum, actual-rate guard, `gain`/`set_gain`, `ppm`), `src/device.rs` (gain calls), `src/mock.rs` (`with_uhd_decimation`, gain state), `src/lib.rs` (`split_zeta_args`); `crates/zeta-source-soapy-driver/src/lib.rs` (gain calls, `open` through `set_gain`).
- Modify `crates/zeta/src/node_setup.rs` (factory into `NodeConfig`), `src/cli.rs` (`SourceCmd::Pin/Unpin/Gain`, `Command::Transcripts`, optional `--squelch-db`), `src/main.rs` (`transcripts tail`, shared `tail_log`), `src/agents_md.rs` (notes, `_db` unit, running paragraph, field craft), `crates/zeta-ws/src/http.rs` (nothing but the new ops flowing through `apply_from`).
- Create tests `crates/zeta/tests/bank_resume.rs`, `crates/zeta-core/tests/pin.rs`, `crates/zeta-core/tests/transcripts_log.rs`, `crates/zeta-core/tests/gain.rs`; extend `crates/zeta-dsp/src/resample.rs` tests, `crates/zeta-source-soapy/src/caps.rs` tests, `crates/zeta-source-soapy/tests/soapy.rs`, `crates/zeta-core/tests/ops.rs`, `crates/zeta/tests/cli.rs`, `crates/zeta-ws/tests/http.rs`.
- Modify `crates/zeta-ui/web/app.js` (source commands, conflict unpin action, tabs), `overlays.js` (pin outline), `waterfall.js` and `coarse.js` (pin counts as a holder), `agent.js` (optional squelch tag), `style.css`; scenarios in `crates/zeta-ui/tests/e2e/ui.py`.
- Modify `README.md`, `docs/agents/field-craft.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 12, "Plan 12 status"), `AGENTS.md`, `skills/zetasdr/SKILL.md`.

---

### Task 1: Resumed jobs take the channel bank; the path and the lag are observable

**Files:**
- Modify: `crates/zeta-core/src/node/mod.rs:60-73` (`NodeConfig`), `:379-496` (`open`), `src/stage.rs:63-73` (`StageContext`), `src/node/launch.rs:170-173` (the context), `src/ops.rs:864-877` (`SourceInfo.bank`), `:1470-1491` (`SourceList`), `crates/zeta-dsp/src/lib.rs:99-160` (`run_map`, counters), `src/channel.rs:255-280, 368-419` (note, kind), every `run_map` caller in `zeta-dsp` and `zeta-speech`/`zeta-ext` if any, `crates/zeta/src/node_setup.rs:262-271`, every `NodeConfig { .. }` literal in tests (`grep -rn "NodeConfig {" crates`)
- Test: `crates/zeta/tests/bank_resume.rs` (new; real registry and `PfbBankFactory`, as `crates/zeta/tests/resume.rs` opens a node in process), `crates/zeta-core/tests/ops.rs` (schema), `crates/zeta-dsp/src/lib.rs` (unit)

**Interfaces:**
- Produces: `NodeConfig { ..., pub bank_factory: Option<Arc<dyn BankFactory>> }` (installed into `Inner.bank_factory` before any port is added, so `add_port`'s `sync_bank` and every resumed launch see it; `set_bank_factory` unchanged); `StageContext { source, bank, pub note: Option<Arc<dyn Fn(serde_json::Value) + Send + Sync>> }` with `StageContext::new(source)` giving `None` and a `with_note` builder; the node builds the note per launch as `job_context_merge(&id, value)` (`mod.rs:1015-1038`, a shallow merge, so `{"channel": {...}}` replaces the `channel` key); `SourceInfo { ..., pub bank: Option<BankInfo> }`, `BankInfo { bin_hz: u64, consumers: usize }` from `Node::bank(&id)`; `zeta_dsp::run_map(kind: &'static str, inputs, bus, cancel, f)` and `run_bin(kind, ...)` print `zeta-dsp: {kind} stage lagged, {n} frames skipped` and count into a `static LAGGED_BY_KIND: Mutex<Vec<(&'static str, u64)>>`, read by `pub fn lagged_frames_by_kind() -> Vec<(&'static str, u64)>`; the channel stage notes `{"channel": {"path": "bank", "bin_hz": N}}` right after a successful `bank.subscribe`, else `{"channel": {"path": "direct", "reason": R}}` with `R` one of `no bank`, `band <bw> Hz wider than a bin (<bin_hz> Hz)`, `bin rate <r> Hz not a multiple of out_rate_hz <o>`, or the `BankError`'s `to_string()`.
- Consumes: `ChannelBank::bin_hz`/`consumers` (`crates/zeta-core/src/bank.rs:29-41`); `PfbBankFactory` (`crates/zeta-dsp/src/bank.rs`).

- [ ] **Step 1: Write the failing tests** in `crates/zeta/tests/bank_resume.rs` (a looping file fixture of 1 s at 2 048 000 Hz, 16 tones 25 kHz apart from the centre, written with `zeta_dsp::fixtures::write_fixture`; `FileOpts { realtime: true, repeat: true, .. }`; a `NodeConfig` with `bank_factory: Some(Arc::new(PfbBankFactory))`):

```rust
#[tokio::test]
async fn fresh_channels_take_the_bank() {
    // 16 fm_demod jobs at 12 500 Hz on the grid: after 1 s each job_context has
    // {"channel": {"path": "bank", "bin_hz": 256000}}; source_list[0].bank ==
    // Some(BankInfo { bin_hz: 256_000, consumers: 16 }); lagged_frames() == 0 after 3 s.
}
#[tokio::test]
async fn resumed_channels_take_the_bank_like_fresh_ones() {
    // Start the 16 as above, drop the node, reopen with resume and the factory in
    // NodeConfig: within 2 s every job is Running with path "bank", consumers == 16,
    // and lagged_frames_by_kind() has no entry with a non-zero count after 3 s.
}
#[tokio::test]
async fn without_a_factory_the_path_is_direct_and_says_so() {
    // NodeConfig { bank_factory: None }: one fm_demod's context reads
    // {"channel": {"path": "direct", "reason": "no bank"}}; source_list[0].bank == None.
}
```
In `crates/zeta-dsp/src/lib.rs`: `lag_line_names_the_kind` (a `run_map("passthrough", ...)` fed past its capacity → `lagged_frames_by_kind()` contains `("passthrough", n)` with `n > 0`). In `tests/ops.rs`: `source_list` items have `bank` nullable with `bin_hz` and `consumers` required.

- [ ] **Step 2: Run to fail**: `cargo test -p zeta --test bank_resume`, `cargo test -p zeta-dsp lag_line`, `cargo test -p zeta-core --test ops` → compile errors on `bank_factory`, `note`, `lagged_frames_by_kind`.
- [ ] **Step 3: Implement**: `NodeConfig.bank_factory` and its install at the top of `open`; `node_setup` passes `Some(Arc::new(PfbBankFactory))` and drops the comment and the `set_bank_factory` call; `StageContext.note` built in `launch_inner` from `self.clone()` and the job id; the channel stage's notes; `run_map`/`run_bin` kind and counters; `SourceInfo.bank`; the schema.
- [ ] **Step 4: Run to pass**: the three targets, then `cargo test -p zeta-core -p zeta-dsp -p zeta`.
- [ ] **Step 5: Gate and commit**: `git commit -m "Resumed channels take the bank: the factory reaches Node::open; channel path in job_context, bank in source_list, lag lines by kind"`.

---

### Task 2: The resampler snaps instead of refusing

**Files:**
- Modify: `crates/zeta-dsp/src/resample.rs:19, 52-94` (`SNAP_FACTOR`, `SNAP_MAX_PPM`, `new`, `input_rate_hz`)
- Test: `crates/zeta-dsp/src/resample.rs` (unit)

**Interfaces:**
- Produces: `pub const SNAP_FACTOR: u64 = 2_048`, `pub const SNAP_MAX_PPM: u64 = 1_000`; `Resampler::new(in, out)` (same signature; exact below the factor, snapped above it, `Err` with the existing text `resampler {in} -> {out} Hz reduces to {l}/{m}, beyond the {MAX_FACTOR} limit` only when no `in'` within `in × SNAP_MAX_PPM / 1_000_000` qualifies); `pub fn input_rate_hz(&self) -> u64` (the rate the ratio was built for, `in` when exact); `ratio()` reports the snapped ratio. `MAX_FACTOR` stays as the ceiling the error names.
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests**:

```rust
#[test]
fn exact_ratios_below_the_factor_are_unchanged() {
    // (96_000, 48_000) -> (1, 2), input_rate_hz 96_000; (200_000, 48_000) -> (6, 25);
    // (2_048_000, 2_400_000) -> (75, 64); (204_800, 48_000) -> (15, 64).
}
#[test]
fn the_live_rates_snap_within_the_bound() {
    // (398_961, 48_000): input_rate_hz 398_976, ratio (125, 1039);
    // (260_339, 48_000): input_rate_hz 260_352, ratio (125, 678);
    // (398_961, 240_000): input_rate_hz 398_976, ratio (625, 1039);
    // each |in' - in| * 1e6 / in < 300.
}
#[test]
fn a_snapped_resampler_streams_like_the_exact_one_at_the_snapped_rate() {
    // Resampler::new(398_961, 48_000) and Resampler::new(398_976, 48_000) give identical
    // output for the same 1 s tone input (the first is the second by construction).
}
#[test]
fn zero_rates_still_error() { /* (0, 48_000) and (48_000, 0): "resampler rates must be >= 1" */ }
```

- [ ] **Step 2: Run to fail**: `cargo test -p zeta-dsp resample`.
- [ ] **Step 3: Implement**: compute `(l, m)` exactly; if `l.max(m) > SNAP_FACTOR`, loop `k` in `1..=in * SNAP_MAX_PPM / 1_000_000` trying `in + k` then `in - k` (skipping 0) for the first whose reduced larger term is at most `SNAP_FACTOR`; `eprintln!` the snap line; store `input_rate_hz`; design the prototype at the snapped rate.
- [ ] **Step 4: Run to pass**: `cargo test -p zeta-dsp` (all; the demod tests must still pass unchanged).
- [ ] **Step 5: Gate and commit**: `git commit -m "Resampler: snap the input rate within 1000 ppm above a factor of 2048 instead of refusing"`.

---

### Task 3: The UHD rate ladder and the actual-rate guard

**Files:**
- Modify: `crates/zeta-source-soapy/src/caps.rs:7, 129-159` (`RateModel`, `uhd_ladder`, `pick_rate`), `src/source.rs:91-104, 117-150, 228-277, 310-343, 403-428` (`SoapySource.rates: RateModel`, the guard in `open_stream` and `apply`), `src/mock.rs` (`with_uhd_decimation`)
- Test: `crates/zeta-source-soapy/src/caps.rs` (unit), `crates/zeta-source-soapy/tests/soapy.rs`

**Interfaces:**
- Produces: `pub enum RateModel { Ranges(Vec<RangeHz>), Ladder { mcr_hz: u64, rungs: Vec<u64> } }`; `pub fn uhd_ladder(rates: &[RangeHz]) -> RateModel` (`mcr` = `max_rate(rates).round()`, `min` = the smallest `min_hz` rounded, rungs `mcr / N` for `N` in `1..=min(512, mcr / min)` with `mcr % N == 0`, ascending); `pub fn pick_rate(model: &RateModel, bandwidth_hz: u64) -> Option<u64>` (ranges as today; ladder: the smallest rung `>= bandwidth_hz`); `pub fn next_rate_above(model: &RateModel, actual_hz: u64) -> Option<u64>` (the smallest rung, or 100 kHz quantum, strictly above `actual_hz`); `SoapySource::with_device_args` builds `RateModel::Ladder` when `is_uhd`, `Ranges` otherwise; in `open_stream` and the reader's `apply`, after reading the actual rate: `if actual < bandwidth_hz` then once `set_sample_rate(next_rate_above(actual))` and re-read, then `Err(PortError::Unsupported(format!("soapy: device gives {actual} Hz for {rate} Hz, below the {bw} Hz band")))` (the `Retune` command carries `bandwidth_hz` for this); `MockDevice::with_uhd_decimation(mcr_hz: f64) -> Self` (`set_sample_rate(r)` stores `mcr / (mcr / r).round().max(1.0)`; the range reads `[mcr / 512, mcr]`, step 0; `with_driver("b200")` implied by the test).
- Consumes: `is_uhd`, `probe_rate_ranges` (unchanged).

- [ ] **Step 1: Write the failing tests** in `caps.rs`:

```rust
#[test]
fn uhd_ladder_picks_exact_divisors_of_the_master_clock() {
    // rates [120_000 .. 61_440_000]: 12_500 -> 120_000; 200_000 -> 204_800; 1_000_000 ->
    // 1_024_000; 2_400_000 -> 2_457_600; 4_000_000 -> 4_096_000; 10_240_000 -> 10_240_000;
    // 16_000_000 -> 20_480_000; 61_440_000 -> 61_440_000; 61_440_001 -> None.
    // Every rung r: gcd-reduced r/48_000 has a larger term <= 1_280.
}
#[test]
fn the_mock_reproduces_the_live_rates() {
    // with_uhd_decimation(61.44e6): set 400_000 -> sample_rate 398_961; 1_040_000 ->
    // 1_041_356; 260_339 is 1_041_356 / 4 (channel_rate_hz, not the device); 4_000_000 ->
    // 4_096_000.
}
#[test]
fn next_rate_above_steps_one_rung_or_quantum() { /* ladder: 398_961 -> 409_600 (N 150); ranges: 398_961 -> 400_000 */ }
```
In `tests/soapy.rs`: `uhd_tune_asks_only_for_ladder_rates` (a 200 000 Hz band on the `b200` mock with `with_uhd_decimation(61.44e6)` → `calls()` contains `set_sample_rate(204800)` and the capture's `bandwidth_hz` is 204 800); `actual_rate_below_the_band_retries_once_then_refuses` (a mock whose `sample_rate()` answers 10 % under every request: the tune of a 200 000 Hz band calls `set_sample_rate` twice and fails with the exact `Unsupported` text naming the second request; `current()` is `None`; a job on it fails with that reason); `non_uhd_rates_keep_the_quantum` (hackrf-like, 12 500 → 2 000 000 as today).

- [ ] **Step 2: Run to fail.** **Step 3: Implement** (`RateModel` first, the mock, then the guard). **Step 4: Run to pass**: `cargo test -p zeta-source-soapy --features test-mock`.
- [ ] **Step 5: Gate and commit**: `git commit -m "Soapy: UHD sources ask for exact divisors of the master clock; an actual rate below the band is retried once, then refused"`.

---

### Task 4: The transcript log and `zeta transcripts tail`

**Files:**
- Modify: `crates/zeta-core/src/node/opslog.rs` → `jsonlog.rs` (`JsonLog`; `OpsLog` gone), `src/node/mod.rs:148-176` (`Inner.transcripts_log`), `:699-714` (`log_op` generalised to `log_line(which, line)`), `src/node/launch.rs:196-248` (the tee), `crates/zeta/src/cli.rs:162-214` (`Command::Transcripts(TranscriptsCmd::Tail { lines, job })`), `src/main.rs:56-58, 250-264` (`tail_log`), `src/agents_md.rs` (`RUNNING`), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/src/node/jsonlog.rs` (unit, the renamed existing tests), `crates/zeta-core/tests/transcripts_log.rs` (new), `crates/zeta/tests/cli.rs`, `crates/zeta-ws/tests/http.rs`

**Interfaces:**
- Produces: `pub(super) struct JsonLog` with `new(path: PathBuf)` (8 MiB) and `with_max_bytes`, `append` as today; `Inner { ops_log: JsonLog, transcripts_log: JsonLog, log_failed: [AtomicBool; 2] }`; `Inner::tee_transcripts(self: &Arc<Self>, rec: &JobRecord, outputs: &[(StreamKind, StreamId)], token: &CancellationToken)` (called in `launch_inner` after `e.outputs = job_outputs` when `rec.kind == JobKind::Transcribe.name()`: one `tokio::spawn` per `Event` output, `select!` on the token and `rx.recv()`; a frame whose `Payload::Event(v)` has `v["protocol"] == "transcript"` is written as the Global Constraints line with `t_ns = now()`, `job = rec.id`, `input = rec.input`, `source = rec.source`, `center_hz`/`bandwidth_hz` from `rec.tuning`, `start_ns`/`end_ns` from the frame's provenance span, `text`/`language`/`dropped`/`error` from the event (`null`/`false` when absent); `Lagged(n)` → the `persist_errors` entry once per task; `Closed` ends the task); CLI `zeta transcripts tail [--lines N] [--job ID]` printing the last N lines of `transcripts.log.1` + `transcripts.log`, filtered by `job` or `input` equal to `ID`, exit 0; `main.rs` `fn tail_log(state_dir, stem: &str, lines, filter: impl Fn(&str) -> bool)` shared with `ops tail`.
- Consumes: `transcribe.rs` events (`:290-298`, `:324-335`, `:339-347`); `StreamBus::subscribe` (`stream.rs:129`).

- [ ] **Step 1: Write the failing tests** in `crates/zeta-core/tests/transcripts_log.rs` (a registry with `PassthroughStage` plus an inline `transcribe`-named stage that publishes three events on its event output as soon as it has an input frame: a text event, a `dropped` event and an `error` event, then idles; a file source; `job_start` passthrough then `job_attach` `transcribe`):

```rust
#[tokio::test]
async fn every_transcript_event_is_one_line_with_the_jobs_tuning() {
    // transcripts.log has exactly 3 lines within 2 s; each has the 12 keys and no other;
    // line 1: text "hello", language "en", dropped false, error null; line 2: text null,
    // dropped true; line 3: text null, error "backend: boom"; every line: job == the
    // attached id, input == the passthrough id, source == "tone", center_hz/bandwidth_hz ==
    // the passthrough's tuning, start_ns/end_ns == the event's.
}
#[tokio::test]
async fn non_transcript_events_and_other_kinds_are_not_logged() {
    // the same stage registered under kind "measure" started by job_start (not attached):
    // no transcripts.log is created.
}
#[tokio::test]
async fn tee_ends_with_the_job_and_a_lag_is_recorded_once() {
    // job_stop the attached job: the tee's JoinHandle (exposed as
    // Node::debug_tee_count(&self) -> usize under cfg(any(test, feature = "test-fixtures")))
    // reaches 0 within 1 s. A stage that publishes 600 transcript events in one burst on a
    // 256-capacity bus: persist_errors() has exactly one entry starting
    // "transcripts.log: job " and ending " events skipped".
}
#[tokio::test]
async fn unwritable_transcripts_log_does_not_fail_the_job() {
    // transcripts.log is a directory: the job runs, persist_errors() has one entry
    // starting "transcripts.log: ".
}
```
`jsonlog.rs`: the three existing tests renamed onto `JsonLog` plus `two_logs_rotate_independently`. `crates/zeta/tests/cli.rs`: `transcripts_tail_prints_the_last_lines_filtered_by_job` (two hand-written lines with different `job` values in `<state>/transcripts.log`; `zeta --state-dir D transcripts tail --lines 1` prints the last; `--job <first>` prints the first; `--job <first's input>` prints it too; `zeta --node http://127.0.0.1:1 transcripts tail` exits 1 with the usage text). `crates/zeta-ws/tests/http.rs`: `ops_log_still_records_the_transport_after_the_rename` (plan 11's `ops_log_records_the_callers_transport` passes unchanged; no new test if it does).

- [ ] **Step 2: Run to fail.** **Step 3: Implement**: `JsonLog`; the tee; the CLI; the running-paragraph sentence: "Every `transcript` event of a `transcribe` job is appended to `transcripts.log` in the state directory (rotated at 8 MiB) with the job, its input, the source and the channel's centre; `zeta transcripts tail --lines N [--job ID]` prints the last N (local only)."; regenerate the guide.
- [ ] **Step 4: Run to pass**: `cargo test -p zeta-core`, `-p zeta --test cli --test agents_md --test transports`, `-p zeta-ws --test http`.
- [ ] **Step 5: Gate and commit**: `git commit -m "Transcript log: every transcript event on disk with its job and channel; zeta transcripts tail"`.

---

### Task 5: Source records and pinned captures

**Files:**
- Create: `crates/zeta-core/src/node/sources.rs` (`SourceRecord`, `load_sources`, `persist_source`)
- Modify: `crates/zeta-core/src/lease.rs:63-84, 113-147, 229-290, 436-483` (`Lease.pin`, `pin_holder`, `pin_locked`, `unpin`, `pinned`, `rigid` and `holders` exclude pins, `conflict_options` option), `src/node/mod.rs:379-544` (`open`: load records, pin before `resume_captures`, skip pinned sources there; `Node::source_pin`, `source_unpin`, `pinned`), `src/ops.rs:34-152` (`Op::SourcePin`, `Op::SourceUnpin`), `:864-877` (`SourceInfo.pin`), `:976-1012` (`name`, `read_only`), `:1470-1524` (apply), `crates/zeta/src/cli.rs:280-290` (`SourceCmd::Pin`, `Unpin`), `src/main.rs`, `src/agents_md.rs` (notes), every in-repo `source_list` consumer that destructures `SourceInfo`, `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/pin.rs` (new), `tests/ops.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `SourceRecord { #[serde(default = current_format)] format: u32, #[serde(default)] pin: Option<Tuning>, #[serde(default)] gain: Option<GainSetting> }`, `GainSetting { db: Option<f64> }` (Task 6 fills it), `<state_dir>/sources/<id>.json`, `load_sources(dir) -> (HashMap<SourceId, SourceRecord>, Vec<String>)` (errors by file name as `persist::load`), `persist_source(dir, id, &rec)` atomic; `pub fn pin_holder(source: &SourceId) -> JobId` (`JobId(format!("pin:{}", source.0))`); `LeaseManager::pin_locked(&self, port, want, guard) -> Result<(Tuning, bool), LeaseError>` (removes an existing pin lease on the source, then `ensure_locked` with `pin_holder`, `elastic: false`, marking the lease `pin: true`); `pin(&self, port, want) -> Result<(Tuning, bool), LeaseError>` (gate-taking); `unpin(&self, source) -> Option<Tuning>`; `pinned(&self, source) -> Option<Tuning>`; `rigid()` and `holders()` skip `pin` leases; `conflict_options(port, want, cur, shared, holders, pinned: Option<&SourceId>)` pushes `format!("unpin source {} (source_unpin)", id.0)` before the replace line; `Node::source_pin(&self, source, want, replace) -> Result<Tuning, NodeError>` (gate once: `replace_holders`, `pin_locked`, persist; on `Io` the pin is released; after the gate `relaunch_elastic` when it tuned); `Node::source_unpin(&self, source) -> Result<(), NodeError>` (`NodeError::Other(format!("source {} is not pinned", id.0))` when none; release, persist `pin: None`, `idle_if_unheld`); `Op::SourcePin { source, center_hz, bandwidth_hz, replace }` → `OpResult::Tuning`, `Op::SourceUnpin { source }` → `OpResult::Ok`, neither `read_only`; `SourceInfo { ..., pub pin: Option<Tuning> }` (nullable schema like `current`); `open`: after `add_port`s, for each loaded record with `pin: Some(t)` whose port is present, `leases.pin(port, t)`; `Err(e)` → `load_errors.push(format!("source {}: pin {}/{} Hz: {e}", id, t.center_hz.0, t.bandwidth_hz.0))`; `resume_captures` skips a source with `pinned(source).is_some()`; CLI `zeta source pin --source S --center-hz N --bandwidth-hz N [--replace ...]`, `zeta source unpin --source S`.
- Consumes: Task 4's `JsonLog` is unrelated; plan 11's `replace_holders`, `gate_for_replace`, `relaunch_elastic`.

- [ ] **Step 1: Write the failing tests** in `crates/zeta-core/tests/pin.rs` (helpers from `tests/elastic.rs`: a file source with a tune range, `Passthrough` and the inline spectrum stand-in):

```rust
#[tokio::test]
async fn pin_tunes_an_unheld_source_and_is_listed() {
    // source_pin at 100.000 MHz / 48 000 → Tuning == current; source_list: pin == Some(that
    // band), holders == [], elastic == []; sources/<id>.json has "pin" with the requested band.
}
#[tokio::test]
async fn pin_is_a_rigid_holder_without_an_id() {
    // rigid passthrough outside the pin → Conflict { holders: [], options } with options
    // containing "unpin source <id> (source_unpin)" and no "retry with replace"; a rigid
    // passthrough inside the pin → Ok, no tune (current unchanged); source_ensure outside →
    // the same Conflict; job_start replace: ["pin:<id>"] → UnknownJob.
}
#[tokio::test]
async fn elastic_jobs_join_the_pin_and_follow_a_repin() {
    // elastic spectrum → tuning == pin; source_pin again at another band with no job holders
    // → Ok, tuned, the elastic job relaunched (same id, new streams, tuning == new pin).
}
#[tokio::test]
async fn repin_over_job_holders_outside_their_capture_is_a_conflict_and_keeps_the_old_pin() {
    // pin A, rigid passthrough inside A; source_pin at B outside A → Conflict naming the
    // passthrough; pinned() == A; with replace: [passthrough] → Ok, pinned() == B.
}
#[tokio::test]
async fn unpin_releases_and_idles() {
    // source_unpin → Ok; source_list pin == None; sources/<id>.json "pin": null; the port's
    // idle count rose (the fake port of tests/scan.rs counts idles); a second source_unpin →
    // Other "source <id> is not pinned".
}
#[tokio::test]
async fn pin_resumes_before_the_records_and_a_scan_fails_naming_it() {
    // sources/<id>.json with a pin; persisted running: a passthrough inside the pin, a
    // passthrough outside it, a scan. Reopen with resume: pin held (source_list.pin), the
    // inside passthrough Running, the outside one Failed with a reason containing "unpin
    // source", the scan Failed with a reason containing "unpin source"; resume_captures
    // did not tune (the port's tune count is 1: the pin's).
}
#[tokio::test]
async fn a_pin_whose_tune_fails_is_a_load_error_and_stays_on_disk() {
    // a with_fail_tune port: load_errors() has one entry starting "source <id>: pin "; the
    // record file is unchanged.
}
```
`tests/ops.rs`: `source_pin` requires `source`, `center_hz`, `bandwidth_hz`, `replace`; `source_unpin` requires `source`; `source_list` items have `pin` nullable. `cli.rs`: `source_pin_and_unpin_round_trip` (one-shot against a file source with a tune range: `source pin` prints `{"result":"tuning",...}`, `source list` shows `pin`, `source unpin` prints `{"result":"ok"}`, `source unpin` again exits 2 with `"error":"other"` and the text).

- [ ] **Step 2: Run to fail.** **Step 3: Implement** (lease first, then the record, the node methods, the ops, the CLI, the guide notes: `source_pin`: "Hold the source at a band without a job: a rigid holder that is not a job, so `replace` cannot name it and a `conflict` offers `unpin source <id> (source_unpin)`. Persisted; restored before anything else on a restart. Put `center_hz` between two channels of the grid you listen to so the receiver's DC spike falls in a guard band (the UHF CB plan's midpoint 476918750 Hz with 1000000 Hz holds all 80 channels)."; `source_unpin`: "Release the pin; the source idles if nothing else holds it."). **Step 4: Run to pass**: `cargo test -p zeta-core`, `-p zeta --test cli --test agents_md --test transports`, every `SourceInfo` consumer crate.
- [ ] **Step 5: Gate and commit**: `git commit -m "Pinned captures: source_pin and source_unpin hold a source at a band, persisted in sources/<id>.json and restored first on resume"`.

---

### Task 6: Live receive gain

**Files:**
- Modify: `crates/zeta-core/src/port.rs:72-88` (`gain`, `set_gain`, `GainInfo`), `src/ops.rs` (`Op::SourceSetGain`, `OpResult::Gain`, `SourceInfo.gain`, apply), `src/node/mod.rs` (`Node::source_set_gain` persisting into the source record; `open` re-applies `gain`), `src/node/sources.rs` (`gain` filled), `crates/zeta-source-soapy/src/device.rs:31-42` (gain calls), `src/mock.rs` (gain state and calls), `src/source.rs:43-56, 60-64, 184-225, 403-428` (`Status.gain`, `Cmd { Retune, Gain }`, `gain`/`set_gain`), `src/lib.rs:22-40` (`split_gain` unchanged); `crates/zeta-source-soapy-driver/src/lib.rs:36-63, 102-154` (the six calls; `open` applies `gain=` through `SoapySource::set_gain`); `crates/zeta/src/cli.rs` (`SourceCmd::Gain { source, db: Option<f64>, auto: bool }`), `src/main.rs`, `src/agents_md.rs:97` (`_db`), the Rules line, `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/gain.rs` (new), `tests/ops.rs`, `crates/zeta-source-soapy/tests/soapy.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `pub struct GainInfo { pub db: Option<f64>, pub min_db: f64, pub max_db: f64, pub automatic: bool, pub automatic_available: bool }` (Serialize/Deserialize/JsonSchema, `db` nullable required); `Port::gain(&self) -> Option<GainInfo>` default `None`; `async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError>` default `Err(PortError::Unsupported("source has no adjustable gain".into()))`; `SoapyDevice { fn gain_range(&self) -> Result<(f64, f64), String>; fn gain(&self) -> Result<f64, String>; fn set_gain(&self, db: f64) -> Result<(), String>; fn has_gain_mode(&self) -> Result<bool, String>; fn gain_mode(&self) -> Result<bool, String>; fn set_gain_mode(&self, automatic: bool) -> Result<(), String> }`; `MockDevice` records `set_gain(<db>)`, `set_gain_mode(<bool>)` in `calls()` and answers range `(0.0, 76.0)` with `has_gain_mode` configurable by `with_gain_mode(bool)`; `SoapySource::gain()` from `Status.gain` (read at open), `set_gain(db)`: `None` → `Unsupported("source <id> has no automatic gain")` unless `automatic_available`, else `set_gain_mode(true)`; `Some(d)` out of range → `PortError::Unsupported(format!("gain_db {d} out of range {min}..={max} dB for source {id}"))`, else `set_gain_mode(false)` where available then `set_gain(d)`; through `Cmd::Gain { db, ack }` on a live reader, directly otherwise; `Node::source_set_gain(&self, source, db) -> Result<GainInfo, NodeError>` (port, `set_gain`, persist `gain: Some(GainSetting { db })`); `Op::SourceSetGain { source, gain_db: Option<f64> }` (required nullable number) → `OpResult::Gain(GainInfo)`; `port_error(PortError::Unsupported(m))` stays `OpError::Other { message: m }` so the texts above reach the caller verbatim; `SourceInfo { ..., pub gain: Option<GainInfo> }`; `open`: a record's `gain: Some(g)` → `port.set_gain(g.db).await`, `Err(e)` → `load_errors.push(format!("source {}: gain: {e}", id))`; CLI `zeta source gain --source S --db N | --auto` (neither or both: clap usage error, exit 1).
- Consumes: Task 5's record; `split_gain` and the driver's open.

- [ ] **Step 1: Write the failing tests** in `crates/zeta-core/tests/gain.rs` (a fake port with gain, range 0..=76, automatic available, counting calls):

```rust
#[tokio::test]
async fn set_gain_round_trips_and_is_listed_and_persisted() {
    // source_set_gain 40 → Gain { db: Some(40.0), min_db: 0.0, max_db: 76.0, automatic: false,
    // automatic_available: true }; source_list gain == that; sources/<id>.json "gain": {"db": 40.0}.
}
#[tokio::test]
async fn null_is_automatic_where_available_and_an_error_elsewhere() {
    // → Gain { db: None, automatic: true, .. }, record {"db": null}; a port without automatic
    // → Other "source <id> has no automatic gain".
}
#[tokio::test]
async fn out_of_range_and_unsupported_are_other_with_the_text() {
    // 90 → Other "gain_db 90 out of range 0..=76 dB for source <id>"; a file source → Other
    // "source has no adjustable gain"; source_list gain == None for the file source.
}
#[tokio::test]
async fn a_persisted_gain_is_reapplied_at_open() {
    // reopen: the fake port's set_gain was called once with Some(40.0) before any job
    // launched; a failing port → load_errors() entry "source <id>: gain: <reason>".
}
```
`tests/soapy.rs`: `gain_and_retune_keep_their_order_on_the_reader` (tune, then `set_gain(Some(30.0))` and a retune issued back to back: `calls()` shows `set_gain_mode(false)`, `set_gain(30)` before the second `set_frequency`; `gain().db == Some(30.0)`); `gain_on_a_faulted_reader_goes_straight_to_the_device` (`fail_on("read", ..)` style fault: `set_gain` succeeds, `calls()` has it); `the_gain_arg_at_open_uses_the_same_path` (`split_gain` + `set_gain(Some(40.0))` → `calls()` `set_gain_mode(false)`, `set_gain(40)`). `tests/ops.rs`: `source_set_gain` requires `source`, `gain_db` (number|null); `GainInfo` fields required, `db` nullable; `unit_fields_carry_their_unit` accepts `_db`. `cli.rs`: `source_gain_cli_requires_db_or_auto` (neither → exit 1; `--auto` on the file source → exit 2 with `"error":"other"` and `source has no adjustable gain`).

- [ ] **Step 2: Run to fail.** **Step 3: Implement** (port trait, Soapy seam and mock, source, node and record, op and CLI, the driver on a `cfg(soapy)` host, the guide: Rules gain "gains are decibels in fields ending `_db`"; `source_set_gain` note: "Set the source's overall receive gain live (`gain_db` in dB within `source list`'s `gain.min_db..=max_db`), or `null` for the driver's automatic gain where `gain.automatic_available`. Persisted: it is re-applied when the node restarts and wins over `gain=` in the device args. A source without adjustable gain (a recording, a KiwiSDR, another node) answers `source has no adjustable gain`."). **Step 4: Run to pass**: `cargo test -p zeta-core`, `-p zeta-source-soapy --features test-mock`, `-p zeta --test cli --test agents_md --test transports`; `cargo check` of the driver crate on a host with libsoapysdr.
- [ ] **Step 5: Gate and commit**: `git commit -m "Live receive gain: source_set_gain through the port, shown in source_list, persisted with the pin"`.

---

### Task 7: The page: pins are holders, source commands, the unpin offer

**Files:**
- Modify: `crates/zeta-ui/web/app.js:175-260` (tabs: `.tag.pin`, title with the band and the gain), `:362-377` (`runCommand`: `source pin|unpin|gain`), `:314-318` and `conflictActions` (the unpin action), `overlays.js:224-236` (`ov.sync(jobs, source)` outlines the pin, key `pin`, label `pin`; no capture outline while pinned), `waterfall.js` (the "rigid holder exists" check includes `pin`), `coarse.js` (the `auto` scanner skips a pinned source), `style.css`
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Produces: `app.js` `runSourceCommand(state, words) -> { text, error }` for `source pin`, `source pin <center_hz> <bandwidth_hz>`, `source unpin`, `source gain <db>|auto` (matched by `/^source\s+(pin|unpin|gain)\b/`; anything else starting `source ` goes to `/ask`); replies `pinned <src> at <center_hz> Hz, <bandwidth_hz> Hz`, `unpinned <src>`, `gain <src>: <db> dB (<min>..=<max>)` / `gain <src>: automatic`, errors the op's error text; `source pin` with no `current` → error `<src> has no capture to pin`; `conflictActions(err, retry)` adds `unpin <src> and retry` when `err.holders` is empty and `state.sources` shows `pin` on `err.source`; `renderTabs` adds `<span class="tag pin">pin</span>` to a pinned source's tab and `title` `pin <lo>–<hi> MHz; gain <db> dB` (or `gain automatic`; omitted without `gain`); `ov.sync(jobs, src)` outlines `src.pin` under key `pin` with text `pin` and skips the capture outline while it exists; `wf` and `coarse` read `src.pin` as a rigid holder.
- Consumes: Tasks 5 and 6's ops and `source_list` fields.

- [ ] **Step 1: Write the failing scenario** `scenario_source_pin_unpin_and_gain_commands` (on `retune`, the file source tunable over 200-300 MHz, `ui.rs:244-247`): select `retune`; `source pin` → transcript `pinned retune at 250000000 Hz, <bw> Hz`; the tab has `.tag.pin`; `.outline[data-key="pin"]` is visible; `source_info("retune").pin` is non-null; `job_start` (API) a `passthrough` on `retune` at 260 MHz → the transcript line contains `held by nobody` and `unpin source retune (source_unpin)`, and a button `unpin retune and retry` exists; click → within 3 s the job is running and `source_info("retune").pin` is null and the tab has no `.tag.pin`; `source gain 20` → an error line containing `source has no adjustable gain`; `source pin 999` → error line `source pin: expected <center_hz> <bandwidth_hz>`; `source of the noise` → one `.line.assistant` (went to `/ask`). Under 10 s.
- [ ] **Step 2: Run to fail**: `ZETA_UI_ONLY=source_pin_unpin_and_gain_commands cargo test -p zeta --test ui`.
- [ ] **Step 3: Implement.** **Step 4: Run to pass**: the scenario, then the whole suite (note the total time in the commit body; it must stay under 300 s).
- [ ] **Step 5: Gate and commit**: `git commit -m "UI: pins outlined and counted as holders, source pin|unpin|gain commands, conflicts offer unpin"`.

---

### Task 8 (optional, open question 8.4): `ppm=` frequency correction

**Files:**
- Modify: `crates/zeta-source-soapy/src/lib.rs:22-40` (`split_zeta_args(args) -> Result<(String, ZetaArgs { gain: Option<f64>, ppm: Option<f64> }), String>`; `split_gain` kept as a wrapper or removed with its callers), `src/source.rs:310-343` (`tune`: `hw = (center as f64 * (1.0 + ppm / 1e6)).round() as u64` to the device; `current()`, blocks and `band` keep the requested centre), `crates/zeta-source-soapy-driver/src/lib.rs:49-63`, `crates/zeta/src/agents_md.rs` (the `--source-soapy` flag text), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-source-soapy/src/lib.rs` (unit), `tests/soapy.rs`

- [ ] **Step 1: Write the failing tests**: `ppm_is_taken_out_of_the_device_args` (`"driver=uhd,ppm=3.4,gain=40"` → rest `"driver=uhd"`, gain 40, ppm 3.4; `ppm=fast` is an error `ppm=fast: not a number`); `tune_applies_ppm_to_the_hardware_and_reports_the_requested_centre` (`with_ppm(3.4)`: tune 146 975 000 → `calls()` has `set_frequency(146975500)` (146 975 000 × 1.0000034 = 146 975 499.7, rounded), `current().center_hz == 146_975_000`, the first block's `center_hz == 146_975_000`).
- [ ] **Step 2: Run to fail.** **Step 3: Implement**; flag text: "`ppm=N` corrects the device's frequency error: the hardware is tuned `N` parts per million higher than every frequency zeta reports, so a device that reads high by `N` ppm (a carrier on 146.975 MHz seen at 146.9754) reads true." **Step 4: Run to pass**: `cargo test -p zeta-source-soapy --features test-mock`, `-p zeta --test agents_md`.
- [ ] **Step 5: Gate and commit**: `git commit -m "Soapy: ppm=N corrects the device's frequency error in tune"`.

---

### Task 9 (optional, open question 8.5): squelch in the NBFM and AM demodulators

**Files:**
- Create: `crates/zeta-dsp/src/squelch.rs` (`Squelch`)
- Modify: `crates/zeta-dsp/src/nbfm.rs:80-200`, `src/am.rs:139-258` (params, the gate, the `Event` output), `crates/zeta-core/src/ops.rs` (`Op::JobStart.squelch_db`, the `fm_demod`/`am_demod` graphs' params, the kind check), every in-repo `job_start` caller (`grep -rn '"job_start"' crates sdk docs/agents`), `crates/zeta/src/cli.rs` (`--squelch-db N`), `src/agents_md.rs`, `crates/zeta-ui/web/agent.js:317-333` (`.tag.squelch`), `overlays.js` (counters ignore `protocol: squelch`), `menu.js` (`squelch_db: null` on every `job_start`), `coarse.js`, `waterfall.js` (same), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-dsp/src/squelch.rs` (unit), `src/nbfm.rs` (stage), `crates/zeta-core/tests/ops.rs`, `crates/zeta/tests/cli.rs`, `ui.py`

**Interfaces:**
- Produces: `pub struct Squelch { .. }` with `Squelch::new(threshold_dbfs: Option<f64>)` (`None` automatic), `pub fn feed(&mut self, iq: &[Complex<f32>], rate_hz: u64) -> Option<bool>` (`Some(open)` on a transition and on the first block, `None` otherwise), `pub fn is_open(&self) -> bool`, `pub fn power_db(&self) -> f32`, `pub fn floor_db(&self) -> f32`; constants `SQUELCH_OPEN_DB: f32 = 6.0` (above the floor, automatic), `SQUELCH_HYSTERESIS_DB: f32 = 3.0`, `SQUELCH_HANG_MS: u64 = 300`, `SQUELCH_FLOOR_PERCENTILE: usize = 15`, `SQUELCH_FLOOR_BLOCKS: usize = 100`; `NbfmParams { deviation_hz, audio_rate_hz, #[serde(default)] squelch_db: Option<i64>, #[serde(default = "true")] squelch: bool }`, `AmParams` likewise; both stages' `outputs: [Audio, Event]`; while closed no audio frame; on each `Some(open)` an event `{"protocol": "squelch", "open": open, "power_db": P, "floor_db": F}` on the event output; `Op::JobStart { ..., squelch_db: Option<i64> }` (required nullable integer; a value with a kind other than `fm_demod`/`am_demod` is `other` `squelch_db applies only to kinds fm_demod and am_demod; pass null (kind is <kind>)`; outside -120..=0 is `other` `squelch_db <v> out of range -120..=0`); CLI `--squelch-db N`; the page's row tag `squelch` while the last `squelch` event has `open: false`.
- Consumes: `channel_rate_hz` output rates; `node_outputs` (`launch.rs:624-646`) for the second output; `job_attach`'s audio-stream rule (`mod.rs:253-266`).

- [ ] **Step 1: Write the failing tests**: `squelch.rs`: `opens_six_db_over_the_floor_and_hangs_300_ms` (2 s of noise at -60 dBFS then a -50 dBFS block: `feed` → `Some(true)` on it; back to noise: `Some(false)` only after 300 ms of blocks), `fixed_threshold_ignores_the_floor` (`Some(-40)`: -50 dBFS never opens, -30 opens at once), `first_block_reports_the_state`. `nbfm.rs`: `closed_squelch_publishes_no_audio_and_a_squelch_event` (a stage fed 1 s of noise at -60 dBFS with `squelch_db: 0`: zero audio frames, one event with `open: false`; with `squelch: false`: audio frames as today). `tests/ops.rs`: `job_start.squelch_db` required nullable integer. `cli.rs`: `job_start_accepts_squelch_db_flag`. `ui.py`: `scenario_squelched_row_is_tagged` (`job_start` `fm_demod` on `tone` with `squelch_db: 0` via the API → the row shows `.tag.squelch` within 3 s and `stream tail` of its audio stream for 1 s returns 0 frames; under 6 s).
- [ ] **Step 2: Run to fail.** **Step 3: Implement.** Guide note on `job_start`: "`squelch_db` (`fm_demod` and `am_demod` only, every other kind passes `null`): `null` opens the squelch 6 dB over the channel's tracked noise floor, an integer is a fixed threshold in dBFS (-120..=0). While closed the job publishes no audio and the attached transcriber idles; each change publishes a `squelch` event (`open`, `power_db`, `floor_db`) on the job's event stream." **Step 4: Run to pass**: `cargo test -p zeta-dsp --features test-fixtures`, `-p zeta-core`, `-p zeta --test cli --test agents_md --test transports --test ui`, the caller crates.
- [ ] **Step 5: Gate and commit**: `git commit -m "Squelch in nbfm_demod and am_demod: no audio while closed, a squelch event per transition; job_start --squelch-db"`.

---

### Task 10: Docs, plan series, final gate

**Files:**
- Modify: `README.md:60-80` (routes: `source pin|unpin|gain`, `transcripts tail`, the rate rules, `--squelch-db` and `ppm=` if built), `docs/agents/field-craft.md` (new "Holding a band": pin it, centre between channels, the UHF CB example, `unpin source` in conflicts; "Reading transcripts": `transcripts tail --job`; a line under "Transcribing a channel" on the squelch if built), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 12 and "Plan 12 status" with the gate counts and the UI suite time), `crates/zeta/src/agents_md.rs` if field-craft is embedded, `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: Write the docs.** **Step 2: Final gate**: `cargo fmt -p <each member> -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no `ui e2e skipped`; record the UI suite's time), `cargo run -p zeta -- agents-md --check` and `--skill --check`; the driver crate checked on a `cfg(soapy)` host. **Step 3: Commit**: `git commit -m "Plan 12: docs, field-craft, status"`.

---

## Live acceptance (controller, after the final review)

On the field host (the `zeta.service` unit): rebuild from a bundle of the branch and restart with the 161 CB jobs persisted (not re-created): every job resumes Running, `zeta job context --job <an fm_demod>` reads `"channel": {"path": "bank", "bin_hz": 512000}`, `zeta source list` shows `bank.consumers` 80 on the B210, CPU settles near the fresh-start figure (about 555 %) and `journalctl -u zeta.service --since -2min | grep -c 'stage lagged'` is 0. Then `zeta transcripts tail --lines 5` shows lines with `center_hz` of CB channels. Pin the band:
`zeta source pin --source b210 --center-hz 476918750 --bandwidth-hz 1000000 --replace <the passthrough hold>` → `bandwidth_hz` 1 024 000 in the result, `source list` shows `pin`, the 80 demodulators keep running (they fit), a `job start --kind wfm_demod` at 100 MHz is a `conflict` whose options name `unpin source b210`. `zeta source gain --source b210 --db 45` changes the noise floor on the page at once; restart the service: the pin and the gain are back (`source list`), the jobs resume inside the pin. A narrow start alone (`job start --kind fm_demod --source b210 --center-hz 147050000 --bandwidth-hz 12500` after unpinning and stopping everything) leases a 120 000 Hz capture and runs with audio. If Task 9 shipped: an idle CB channel's row shows `squelch`, its audio stream is silent, and the 80-tuner CPU drops; if Task 8 shipped: with `ppm=3.4` (or the value measured) the 146.975 MHz repeater's carrier sits at 146.975 on the waterfall.
