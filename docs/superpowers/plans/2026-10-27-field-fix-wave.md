# Field Fix Wave Implementation Plan (plan 14)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The four defects of the 2026-10-23 HackRF field session are fixed and pinned by tests: a SoapySDR source whose samples run ahead of the wall clock restarts its stream instead of stamping stale data into the future; a job's stopped run can no longer outlive its job (the CPU leak behind the "channel stage lagged" flood); `measure` reports a partial window instead of discarding it in silence on every gap; a vanished SoapySDR device faults the source without another native call on the dead handle, and an overflow storm faults instead of spinning. `source_list` and `GET /health` carry the counters that would have shown each of them live.

**Architecture:** All in existing crates, no new dependency. In `zeta-source-soapy`, the reader's `Clock` gains a wall-clock bound (`MAX_AHEAD_NS`) that closes and reopens the stream, a rate change reopens the stream instead of deactivating it in place (SoapyHackRF keeps a stale remainder buffer across `activateStream`), and device loss ends in `SoapyRx::abandon` (the stream is forgotten, never closed). In `zeta-core`, `JobEntry` gains a run generation so a supervisor only finishes, releases and records the run it was spawned for, `record_running` refuses to replace a live run, and a `stages` counter with an RAII guard counts live stage tasks. In `zeta-dsp`, `measure` emits partial windows of at least `MIN_PARTIAL_S` with `partial: true` and notes its discards in the job context. `Port` gains `stats()` (nullable `PortStats` on `SourceInfo`), `BankInfo` gains `dropped`, `Health` gains `stages_running`.

**Tech Stack:** Rust 2024 (`zeta-source-soapy`, `zeta-source-soapy-driver`, `zeta-core`, `zeta-dsp`, `zeta-ws`, `zeta`), the `soapysdr` 0.5.1 crate behind `--cfg soapy`, tokio, the `MockDevice` in `crates/zeta-source-soapy/src/mock.rs` (feature `test-mock`), the 2.048 MHz looping `FileSource` + `PfbBankFactory` harness in `crates/zeta/tests/bank_resume.rs`.

**Spec:** `TODO.md` sections "Bug: sample timestamps run ahead of wall-clock time (HackRF)", "Bug: stopped jobs leave work running", "Bug: `measure` jobs emit no events", "Bug: a USB drop of a SoapySDR device segfaults the node" (the field evidence), and the root-cause notes in "Findings" below (from the 2026-10-27 code investigation; nothing in them was run against hardware).

## Findings (the causes this plan argues from)

1. **Clock.** `crates/zeta-source-soapy/src/source.rs:671-689`: `t0 = anchor + count·1e9/rate`, `reanchor` sets `anchor = max(now, at(count))` (forward only). A rate change (`apply`, `:696-734`) deactivates and re-activates the stream in place. SoapyHackRF (`/opt/build/SoapyHackRF/HackRF_Streaming.cpp`) clears its partly read remainder buffer only in `closeStream`; after `activateStream` the first `releaseReadBuffer` decrements `buf_count` (u32) from 0, and the driver then hands back buffers as fast as the reader takes them. The clock stamps them at `rate`, so `t0` runs ahead at reader throughput over sample rate (the observed 3.4x), and the forward-only anchor keeps the lead for good (17-81 min). A legitimate stream is never ahead of the wall clock except by the anchor carry: buffered samples are stamped at capture time, which is earlier than their read time.
2. **Leak.** `crates/zeta-core/src/node/launch.rs`: `record_running` (`:530-572`) inserts a fresh `JobEntry` under the job id unless the entry is a relaunch/gated stop no longer running; `supervise` (`:755-800`) reads `relaunch`, calls `finish` (`:687`) and releases the lease by id only; `e.done = Some(h)` (`:262`, `:653`) lands on whatever entry has the id. Two overlapping `job_graph_set` calls (`node/mod.rs:1136`), or a relaunch landing between `record_running` and `done = Some`, orphan a run: its `CancellationToken` is dropped uncancelled, its stages (fft at full capture rate, or channel+demod) run until the node restarts, its streams stay registered. `stop_with` (`node/mod.rs:245-278`) returns at once for a job already `Stopped`, which is how the second caller gets past the first's stop.
3. **measure.** `crates/zeta-dsp/src/measure.rs:787-835`: emits once `buf.len() >= rate` (one second at the channel rate); on `Lagged`, a `t0` gap over one sample, or a rate/centre change it clears the window with no event, log or note, and ignores its `StageContext`. Every completed window emits (a `noise` label at low SNR), so zero events means no unbroken second arrived. The bank path breaks input in more places than the direct one (`bank.rs:277-280` input queue of 64, `launch.rs:925-934` `feed_bank` lag → `bank.reset()`, `channel.rs:492-495` bin lag); with orphaned stages saturating the workers (finding 2) those fire at over 1 Hz.
4. **Crash.** The zeta crates have no `unsafe`; the SIGSEGV is in SoapyHackRF/libhackrf/libusb. The field log (`scratch/serve.log`) shows no `[ERROR]` line, so the process died inside the ~1 s of 100 ms read timeouts before zeta touched the device again (`QUIET_READS`, `source.rs:824-831`), on the reader or libusb thread. zeta cannot stop that in-process; it can stop making native calls on a lost handle (today `run()` ends with `rx.deactivate()` on every exit, `:842`, and a later `tune` reuses the dead `Device`) and bound the overflow retry (`:832-835`, unbounded). The node survives a driver crash only when the driver runs in another process: today that is a child `zeta serve` attached with `--source-node` (its port faults `remote node <url>: stream closed` when the child dies); a `soapy-worker` child is a plan of its own (TODO.md "Architecture").

## Global Constraints

- No new crate dependency. Every new op-result field is additive and nullable where it can be absent: `SourceInfo.stats: PortStats | null`, `BankInfo.dropped: integer`, `Health.stages_running: integer`. Event fields are additive: a `measure` event gains `partial: bool` (false on a full window). Job-context notes are additive: `measure: {discarded, partial, last}`.
- `IqBlock` contract (`port.rs:89-96`) holds: `t0` never goes backward within a source; after a stream restart blocks are discarded until the wall clock has caught up with the last stamped end.
- Constants: `MAX_AHEAD_NS = 2_000_000_000`, `MAX_RESTARTS = 5` within `RESTART_WINDOW = 60 s` (then a fault), `MAX_OVERFLOW_RUN = 100` consecutive overflows with no samples (then a fault), `MIN_PARTIAL_S = 0.25` (a partial `measure` window is emitted when it holds at least `rate/4` samples and at least `SIZE` samples). Existing: `QUIET_READS = 10`, `READ_TIMEOUT_US = 100_000`, the 164 ms lead `t0_monotonic_across_reopen` relies on stays under `MAX_AHEAD_NS`.
- A `SoapySDR` test runs with `--features test-mock` and never needs hardware; the `--cfg soapy` driver builds and passes clippy (`RUSTFLAGS="--cfg soapy" cargo clippy -p zeta-source-soapy-driver --all-targets -- -D warnings`).
- Every task that changes an op result, a stage note or field craft regenerates `AGENTS.md` (`cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`--skill`) and runs `cargo test -p zeta --test agents_md --test transports`.
- Gates per task: `cargo fmt -p <touched crates> -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the touched crates' tests (`--features test-fixtures` / `test-mock` where gated). Final gate: `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no `ui e2e skipped`), `agents-md --check`, `--skill --check`. Temp files under the job's tmp directory, never `/tmp`; Python via `python3 -I`.
- Commits on branch `plan-14` from `main` (`bfa7e5b`), one per task, messages in the repo's style (`Soapy: ...`, `Jobs: ...`, `Measure: ...`).

## Review Focus

1. **A legitimate slow consumer must never trigger the clock guard.** A reader that falls behind stamps blocks behind the clock, not ahead; the guard fires only on stamps over `MAX_AHEAD_NS` ahead of `now`. Pinned in Task 1 (`slow_reads_never_restart_the_stream`: `Slow(n, ms)` reads slower than real time, zero restarts).
2. **A stop that races a relaunch must still stop.** After the generation change, `job_stop` during `relaunch_elastic_now` must leave no running run and a `Stopped` record (plan 11's `stop_during_a_relaunch_is_not_undone` stays green) and `debug_stage_count()` returns to baseline. Pinned in Task 2 (`stop_during_a_relaunch_leaves_no_stage`).
3. **Partial windows must not pretend to be whole.** A partial event's `start_ns`/`end_ns` span its true samples and `partial: true`; `classify` and `cb_watch.py` consumers that read `snr_db` are unaffected. Pinned in Task 3 (`a_partial_window_spans_its_samples`).
4. **Device loss during a retune.** `apply()` fails with `RateError::Io` on a vanished device; the exit must also be `abandon`, not `deactivate` (the retune-error exit at `source.rs:787-790`). Pinned in Task 4 (`device_loss_during_a_retune_abandons_the_stream`).
5. **Overflows with samples between them are normal.** Only `MAX_OVERFLOW_RUN` consecutive overflows with no successful read fault the source; `Overflow, Samples, Overflow, Samples...` forever never does. Pinned in Task 4 (`interleaved_overflows_never_fault`).

---

## File map

- Modify `crates/zeta-source-soapy/src/source.rs` (`Clock`, `Status`, `Ctx` gains `format: WireFormat`, `apply` reopens, `run` guard/discard/abandon/overflow run, `stats()`), `src/device.rs` (`SoapyRx::abandon`), `src/mock.rs` (`MockRead::Gone`, `calls_after_gone`, `abandon` recorded), `tests/soapy.rs`; `crates/zeta-source-soapy-driver/src/lib.rs` (`abandon` = `mem::forget`).
- Modify `crates/zeta-core/src/port.rs` (`PortStats`, `Port::stats`), `src/ops.rs` (`SourceInfo.stats`, `BankInfo.dropped`), `src/node/mod.rs` (`JobEntry.run`, `Inner.runs`, `Inner.stages`, `debug_stage_count`), `src/node/launch.rs` (`record_running` → `(Provenance, u64)`, `supervise(gen)`, `finish(id, gen, state)`, `StageRunning`, `relaunch_elastic_now` skip), `src/bank.rs` (`dropped()` on `ChannelBank`); `crates/zeta-dsp/src/bank.rs` (`dropped` counter); tests `crates/zeta-core/tests/runs.rs` (new), `tests/elastic.rs`.
- Modify `crates/zeta-dsp/src/measure.rs` (partials, notes), its in-crate tests; create `crates/zeta/tests/measure_bank.rs`.
- Modify `crates/zeta-ws/src/http.rs` (`Health.stages_running`), `crates/zeta-ui/web/*` untouched.
- Modify `crates/zeta/src/agents_md.rs` (field craft: reading `stats`, the two-node HackRF pattern, partial `measure` events), `README.md`, `docs/agents/field-craft.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 14, "Plan 14 status"), `TODO.md` (the four bug sections replaced by one line each pointing here), `AGENTS.md`, `skills/zetasdr/SKILL.md`.

---

### Task 1: The Soapy clock guard and stream reopen on a rate change (Opus)

**Files:**
- Modify: `crates/zeta-source-soapy/src/source.rs:49-64` (`Status`), `:645-658` (`Ctx`), `:671-689` (`Clock`), `:696-734` (`apply`), `:738-843` (`run`), `:324-341` (stats accessors); `crates/zeta-core/src/port.rs:99-128` (`Port::stats`), `crates/zeta-core/src/ops.rs:1000-1030` (`SourceInfo.stats`)
- Test: `crates/zeta-source-soapy/tests/soapy.rs`, `crates/zeta-core/tests/ops.rs` (schema: `stats` nullable required)

**Interfaces:**
- Produces: `pub struct PortStats { pub measured_rate_hz: Option<f64>, pub overflows: u64, pub timeouts: u64, pub restarts: u64 }` in `zeta-core::port` (Serialize, Deserialize, JsonSchema, `measured_rate_hz` nullable required); `Port::stats(&self) -> Option<PortStats>` default `None`; `SourceInfo.stats: Option<PortStats>` (`#[schemars(required, schema_with = "crate::types::nullable::<PortStats>")]`), filled from `port.stats()` in `source_list`. In `source.rs`: `const MAX_AHEAD_NS: u64 = 2_000_000_000`, `const MAX_RESTARTS: u32 = 5`, `const RESTART_WINDOW: Duration = 60 s`; `Status.restarts: u64`, `Status.restart_times: VecDeque<Instant>`; `Ctx.format: WireFormat`; `fn reopen(ctx: &Ctx, rx: &mut Box<dyn SoapyRx>) -> Result<(), String>` (drops `*rx`, `*rx = ctx.dev.open_rx(ctx.format)?`, `rx.activate()?`); `apply(ctx, rx: &mut Box<dyn SoapyRx>, ...)` uses `reopen` for the rate-change and the `Narrow` restore paths (no `deactivate`/`activate` pair remains in `apply`); in `run`, `Ok(n)` arm: `let end = clock.at(clock.count + n)`; if `end > now_ns() + MAX_AHEAD_NS` → `reopen`, `restarts += 1`, push `Instant::now()` to `restart_times` (drop entries older than `RESTART_WINDOW`), `eprintln!("zeta: soapy {id}: samples ran {lead_ms} ms ahead of the clock; stream restarted")`, set `catch_up = Some(last_end)`, do not send; while `catch_up.is_some_and(|t| now_ns() < t)` successful reads are discarded (not counted, `quiet = 0`); when caught up, `clock.anchor = now_ns().max(last_end)`, `count = 0`, `catch_up = None`; more than `MAX_RESTARTS` restarts within `RESTART_WINDOW` → `ctx.fault(format!("soapy: samples arrive faster than {rate} Hz ({n} stream restarts in {RESTART_WINDOW:?})"))`, break. `SoapySource::stats()` returns `Some(PortStats{ measured_rate_hz: self.measured_rate_hz(), overflows, timeouts, restarts })`.
- Consumes: `MockDevice::calls()` records `open_rx(<fmt>)`, `activate`, `deactivate`, `set_sample_rate(n)`, `set_frequency(n)`; `samples(n)` in `tests/soapy.rs:27`; `end_ns` at `:64`.

- [ ] **Step 1: Write the failing tests** in `tests/soapy.rs`:

```rust
#[tokio::test]
async fn fast_blocks_restart_the_stream_and_stay_near_the_clock() {
    // 1 MHz, block_len 16 384: 150 instant blocks = 2.46 s of samples in ~0 s.
    // Then 120 Slow(16384, 20) blocks: about real time, while the clock catches up.
    let mut reads = samples(150);
    reads.extend(std::iter::repeat_n(MockRead::Slow(16_384, 20), 120));
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(reads));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    let mut blocks = Vec::new();
    while let Ok(Ok(b)) = tokio::time::timeout(Duration::from_millis(1500), rx.recv()).await {
        let now = now_ns();
        assert!(end_ns(&b) <= now + MAX_AHEAD_NS + 100_000_000, "block {} ms ahead", (end_ns(&b) - now) / 1_000_000);
        blocks.push(b);
    }
    assert!(blocks.len() >= 100 && blocks.len() < 150, "{}", blocks.len());  // the burst stopped at the guard
    assert_eq!(dev.calls().iter().filter(|c| c.starts_with("open_rx")).count(), 2);
    assert!(src.fault().is_none());
    assert_eq!(src.stats().unwrap().restarts, 1);
    // monotonic across the restart
    assert!(blocks.windows(2).all(|w| w[1].t0.0 >= end_ns(&w[0])));
}

#[tokio::test]
async fn slow_reads_never_restart_the_stream() {
    // 40 blocks of 16 384 at 1 MHz delivered every 30 ms: behind the clock, never ahead.
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(vec![MockRead::Slow(16_384, 30); 40]));
    let src = source(&dev);
    let mut rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    for _ in 0..40 { next_block(&mut rx).await; }
    assert_eq!(src.stats().unwrap().restarts, 0);
    assert_eq!(dev.calls().iter().filter(|c| c.starts_with("open_rx")).count(), 1);
}

#[tokio::test]
async fn too_many_restarts_fault_the_source() {
    // Each burst of 150 instant blocks trips the guard once; the mock keeps delivering
    // instantly after each reopen, so with no catch-up pause every burst is a restart.
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(samples(150 * 7)));
    let src = source(&dev);
    let _rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    assert!(wait_until(3_000, || src.fault().is_some_and(|f| f.contains("faster than"))).await);
    assert!(src.stats().unwrap().restarts > MAX_RESTARTS as u64);
}
```

Note for the implementer of the third test: during catch-up, discarded instant reads do not count, so a pure-instant script after the first restart is discarded until `now >= last_end` (about 2 s), then the remaining burst trips the guard again. If the script runs out before 6 restarts, lengthen it (`samples(150 * 7)` → more) or shorten the wait by using `MockRead::Slow(16_384, 1)`; the assertion is the fault text and `restarts > MAX_RESTARTS`.

Rename and rewrite `retune_rate_change_reactivates_stream` (`:187-219`) as `retune_rate_change_reopens_the_stream`: the trailing calls after the 2 MHz tune are `["set_sample_rate(2000000)", "set_frequency(101000000)", "open_rx(Cs8)", "activate"]` (no `deactivate`; a dropped `MockRx` records nothing today, see Task 4 for `drop` recording); the centre-only tune still appends exactly `set_frequency(102000000)`. Keep `a_refused_retune_keeps_the_capture_it_had` green: the `Narrow` restore path reopens too (assert one extra `open_rx` there).

In `crates/zeta-core/tests/ops.rs` add `source_list_carries_nullable_stats`: a file source's `SourceInfo.stats` is `null` in the JSON, and the schema marks `stats` required.

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-source-soapy --features test-mock --test soapy fast_blocks slow_reads too_many retune_rate_change` → compile errors (`stats`, `MAX_AHEAD_NS`) or assertion failures.
- [ ] **Step 3: Implement** `PortStats` and `Port::stats` in `port.rs`, `SourceInfo.stats` in `ops.rs` (fill in the `source_list` arm beside `fault`), then `source.rs` per Interfaces. Export `MAX_AHEAD_NS` and `MAX_RESTARTS` as `pub const` so the test reads them.
- [ ] **Step 4: Run the crate tests**: `cargo test -p zeta-source-soapy --features test-mock` and `cargo test -p zeta-core --features test-fixtures --test ops` → all pass; `RUSTFLAGS="--cfg soapy" cargo clippy -p zeta-source-soapy-driver --all-targets -- -D warnings` clean.
- [ ] **Step 5: Regenerate docs**: `cargo run -p zeta -- agents-md > AGENTS.md`, `--skill > skills/zetasdr/SKILL.md`; `cargo test -p zeta --test agents_md --test transports`.
- [ ] **Step 6: Commit**: `git commit -m "Soapy: a stream whose stamps run 2 s ahead of the clock is reopened and its lead waited out; a rate change reopens the stream; source_list.stats (measured rate, overflows, timeouts, restarts)"`.

---

### Task 2: A run generation on job entries and a live-stage counter (Opus)

**Files:**
- Modify: `crates/zeta-core/src/node/mod.rs:123-140` (`JobEntry.run: u64`), `Inner` (`runs: AtomicU64`, `stages: Arc<AtomicUsize>`), `:245-278` (`stop_with` unchanged), `:1236` (`debug_stage_count` beside `debug_tee_count`); `crates/zeta-core/src/node/launch.rs:185-265` (`launch_inner`), `:370-400` (`relaunch_elastic_now`), `:530-572` (`record_running`), `:640-660` (the scan launch's `done = Some`), `:687-705` (`finish`), `:755-800` (`supervise`), `:1075-1088` (`StageRunning` beside `TeeRunning`); `crates/zeta-ws/src/http.rs:127-137, 229-237` (`Health.stages_running`)
- Test: create `crates/zeta-core/tests/runs.rs`; `crates/zeta-core/tests/elastic.rs`; `crates/zeta-ws/tests/*` (health has `stages_running`)

**Interfaces:**
- Produces: `JobEntry.run: u64`; `Inner::record_running(...) -> Result<(Provenance, u64), NodeError>` (assigns `run = self.runs.fetch_add(1) + 1`; refuses with `NodeError::Other(format!("job {} is already running a launch", id.0))` when the existing entry has `cancel.is_some()`); `supervise(inner, id, gen: u64, port, set, token, streams)` applies the `relaunch` read, `finish`, the lease release and `released` only when the entry's `run == gen`, and unregisters its streams unconditionally; `Inner::finish(&self, id: &JobId, gen: u64, state: JobState)` (no-op when `entry.run != gen`); the two `e.done = Some(h); e.outputs = ...` writes guarded by `e.run == gen`; `relaunch_elastic_now` `continue`s on an entry whose `done.is_none()` (a launch in progress); `struct StageRunning(Arc<AtomicUsize>)` RAII, one moved into every `set.spawn` future in `launch_inner` and the scan launch; `Node::debug_stage_count() -> usize`; `Health.stages_running: usize` from `node.debug_stage_count()`.
- Consumes: the `tests/elastic.rs` helpers, copied into `runs.rs`: `registry_lingering(Duration)` (`:145`, stages that linger after cancel), `cfg(dir)`, `band(center_hz, bw)`, `start(kind, tuning, elastic) -> Op`, `started(&node, op) -> JobId`, `streams(&node, &job) -> Vec<StreamId>`, `open(dir, &Arc<Radio>) -> Node` and its `Radio` test port; for the soak, `bank_resume.rs`'s `cfg(dir, pfb())`, `fixture`, `port`, `channel_hz`, `CENTER_HZ`; `Node::bus().list()`, `lease_holders`, `lease_elastic`, `debug_tee_count`.

- [ ] **Step 1: Write the failing tests** in `crates/zeta-core/tests/runs.rs` (helpers copied from `elastic.rs`: a file source, `registry_lingering`, `node_over`):

```rust
#[tokio::test]
async fn concurrent_graph_puts_leave_one_run() {
    // Stages linger 300 ms after cancel, so the first put's old supervisor is still
    // joining when the second put's stop finds the job Stopped and launches.
    let node = open_with(registry_lingering(Duration::from_millis(300))).await;  // open() with that registry
    let id = started(&node, start(JobKind::Passthrough, band(100_000_000, 1_000_000), false)).await;
    let g = match node.apply(Op::GraphGet { job: id.clone() }).await.unwrap() { OpResult::Graph(g) => g, r => panic!("{r:?}") };
    let put = |g| node.apply(Op::GraphPut { job: id.clone(), graph: g });
    let (a, b) = tokio::join!(put(g.clone()), put(g));
    assert!(a.is_ok() || b.is_ok());             // one wins; the other may be refused
    node.apply(Op::JobStop { job: id.clone() }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(node.debug_stage_count(), 0);
    assert!(streams(&node, &id).is_empty());
    assert!(node.lease_holders(&source(&node).await.id).is_empty());
}

#[tokio::test]
async fn start_stop_with_pins_returns_to_baseline() {
    // 20 rounds: elastic spectrum + rigid measure on a 2.048 MHz bank source, a pin between
    // two bands each round (relaunching the elastic job), then stop both.
    let node = Node::open(cfg(dir.path(), pfb()), registry(), vec![port(&fixture(dir.path())).1]).await.unwrap();  // as bank_resume.rs:143
    let base = (node.debug_stage_count(), node.bus().list().len());
    for i in 0..20 {
        let s = started(&node, start(JobKind::Spectrum, band(CENTER_HZ, 2_048_000), true)).await;
        let m = started(&node, start(JobKind::Measure, band(channel_hz(i % 16), 12_500), false)).await;
        let c = if i % 2 == 0 { CENTER_HZ } else { CENTER_HZ + 512_000 };
        node.apply(Op::SourcePin { source: src.clone(), center_hz: Hz(c), bandwidth_hz: Hz(1_024_000), replace: vec![] }).await.unwrap();
        node.apply(Op::JobStop { job: s }).await.unwrap();
        node.apply(Op::JobStop { job: m }).await.unwrap();
    }
    assert!(wait_until(2_000, || (node.debug_stage_count(), node.bus().list().len()) == base).await);
    assert!(node.lease_elastic(&src).is_empty());
    assert_eq!(sources(&node).await[0].bank.as_ref().map_or(0, |b| b.consumers), 0);
}

#[tokio::test]
async fn stop_during_a_relaunch_leaves_no_stage() {
    // As elastic.rs::stop_during_a_relaunch_is_not_undone, plus the stage count.
    ...
    assert!(wait_until(1_000, || node.debug_stage_count() == 0).await);
}
```

In `crates/zeta-ws` add `health_reports_running_stages`: `GET /health` with one passthrough job running has `stages_running >= 1`, and `0` after `job_stop`.

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-core --features test-fixtures --test runs` → `debug_stage_count` missing (compile) and, once stubbed, `concurrent_graph_puts_leave_one_run` fails on the stage count or streams.
- [ ] **Step 3: Implement** per Interfaces. Order: `StageRunning` and `debug_stage_count` first (so the test compiles and shows the leak), then `run`/generation, then the `record_running` refusal and the `relaunch_elastic_now` skip. Do not serialise `job_graph_set` with `relaunching` (a rigid launch's `after_gate` waits on the relaunch task, which takes `relaunching`: deadlock).
- [ ] **Step 4: Run**: `cargo test -p zeta-core --features test-fixtures` (elastic, replace, scan, resume, jobs, runs all green), `cargo test -p zeta-ws`, `cargo test -p zeta --test resume --test bank_resume`.
- [ ] **Step 5: Commit**: `git commit -m "Jobs: a run generation on job entries so a finished supervisor only ends its own run; record_running refuses to replace a live run; stages_running counter in /health"`.

---

### Task 3: `measure` emits partial windows and notes its discards; the bank reports drops (Sonnet)

**Files:**
- Modify: `crates/zeta-dsp/src/measure.rs:729-737` (doc), `:776-836` (the stage), its in-crate tests (`measure_stage_discards_window_on_gap`, `measure_stage_discards_window_on_lag`); `crates/zeta-core/src/bank.rs` (`ChannelBank::dropped(&self) -> u64` default 0), `crates/zeta-dsp/src/bank.rs:277-280` (count the drop), `crates/zeta-core/src/ops.rs:1030-1033` (`BankInfo.dropped`)
- Test: create `crates/zeta/tests/measure_bank.rs` (helpers from `bank_resume.rs:30-135`)

**Interfaces:**
- Produces: `pub const MIN_PARTIAL_S: f64 = 0.25` in `measure.rs`; on `Lagged`, a gap, or a rate/centre change, when `buf.len() >= (rate as f64 * MIN_PARTIAL_S) as usize && buf.len() >= SIZE` the stage emits the window's event with `ev["partial"] = true` and `prov.span = frame_span(t0, buf.len(), rate)`, else discards; every full-window event carries `partial: false`; `ctx.note(json!({"measure": {"discarded": d, "partial": p, "last": "gap"|"lag"|"retune"}}))` on each discard or partial (`d`, `p` cumulative for the run; `retune` = rate or centre changed); `BankInfo.dropped: u64` from `bank.dropped()` (input blocks dropped at the full queue, cumulative for the bank).
- Consumes: `StageContext::note` (`stage.rs:92`), `frame_span`, `measure_band`, `layer1_label` as today; `bank_resume.rs` helpers `registry`, `cfg`, `pfb`, `fixture`, `port`, `start`, `channel_note`, `until`, `bank_note`.

- [ ] **Step 1: Write the failing tests.** In `measure.rs` rewrite the two discard tests: `measure_stage_emits_a_partial_on_gap` (0.5 s of a tone, a 10 ms gap, then 1 s: two events, the first `partial: true` with `end_ns - start_ns` within one block of 500 ms, the second `partial: false` spanning 1 s) and `measure_stage_drops_a_short_remainder_on_lag` (0.1 s then `Lagged`: no event; the note reads `discarded: 1, last: "lag"`). Add `a_partial_window_spans_its_samples` (assert the span and `snr_db` present). In `crates/zeta/tests/measure_bank.rs`:

```rust
#[tokio::test]
async fn measure_on_the_bank_path_emits_within_4_s() {
    // 2.048 MHz looping FileSource, PfbBankFactory; measure at channel_hz(3), 12 500 Hz.
    let (node, id) = ...;
    assert_eq!(channel_note(&node, &id), bank_note());
    let ev = first_event(&node, &id, Duration::from_secs(4)).await.expect("a measure event");
    assert_eq!(ev["partial"], false);
    assert!((ev_span_ns(&ev) as i64 - 1_000_000_000).abs() < 25_000_000);
}

#[tokio::test]
async fn measure_emits_partials_when_the_input_gaps_every_400_ms() {
    // A test Port (as zeta-dsp/tests/bank.rs:434-460) at 2.048 MHz that skips one block of
    // t0 every 400 ms. Today: no event in 4 s. After: an event with partial: true, and the
    // job context's measure.partial >= 1.
}
```

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-dsp --features test-fixtures measure_stage` and `cargo test -p zeta --test measure_bank` → the gap test gets one event not two; the 400 ms test times out.
- [ ] **Step 3: Implement** per Interfaces; update the doc comment at `measure.rs:729-737`.
- [ ] **Step 4: Run**: `cargo test -p zeta-dsp --features test-fixtures`, `cargo test -p zeta --test measure_bank --test bank_resume --test cb_watch` (cb_watch reads measure events only through spectrum rows: unaffected), `cargo test -p zeta-core --features test-fixtures --test ops --test bank`.
- [ ] **Step 5: Regenerate docs** (`BankInfo.dropped`, the `partial` field in the measure note of field craft): `agents-md`, `--skill`, `cargo test -p zeta --test agents_md --test transports`.
- [ ] **Step 6: Commit**: `git commit -m "Measure: a window cut short by a gap, lag or retune is emitted as partial when it holds 250 ms; discards noted in job_context; BankInfo.dropped"`.

---

### Task 4: Device loss ends without another native call; an overflow storm faults (Sonnet)

**Files:**
- Modify: `crates/zeta-source-soapy/src/device.rs:56-61` (`SoapyRx::abandon`), `crates/zeta-source-soapy-driver/src/lib.rs:182-196` (`Rx<E>::abandon`), `crates/zeta-source-soapy/src/mock.rs:12-19, 340-360` (`MockRead::Gone`, `gone` flag, `calls_after_gone`, `abandon` and `Drop` recorded), `src/source.rs` (`run` exits, `lost: AtomicBool` on `SoapySource`, `tune` short-circuit, overflow run counter)
- Test: `crates/zeta-source-soapy/tests/soapy.rs`

**Interfaces:**
- Produces: `trait SoapyRx { ...; fn abandon(self: Box<Self>); }` (driver: `std::mem::forget(self.stream)`, so neither `deactivateStream` nor `closeStream` runs; mock: pushes `"abandon"` to calls); `MockRx: Drop` pushes `"drop"`; `MockRead::Gone(String)`: from that read on every read returns `Fatal(msg)`, sets the device's `gone` flag, and every later device/stream call is recorded in `calls_after_gone()` and returns `Err("device gone")`; `pub const MAX_OVERFLOW_RUN: u32 = 100`; in `run`: the `Fatal` read exit, the `QUIET_READS` exit and the `RateError::Io` retune exit call `rx.abandon()` and set `SoapySource.lost`; every other exit deactivates as today; `Err(Overflow)` increments `overflow_run`, a successful read resets it, `overflow_run >= MAX_OVERFLOW_RUN` → `ctx.fault(format!("overflow storm: {MAX_OVERFLOW_RUN} overflows with no samples"))`, abandon, break; `SoapySource::tune` on `lost` returns `Err(PortError::Io(format!("device lost: {fault}; restart the node")))` without touching the device.
- Consumes: `Ctx::fault`, `node_with`, `passthrough`, `wait_failed` in `tests/soapy.rs:300-340`.

- [ ] **Step 1: Write the failing tests** in `tests/soapy.rs`:

```rust
#[tokio::test]
async fn vanished_device_faults_source_fails_jobs_and_is_never_touched_again() {
    let dev = Arc::new(MockDevice::hackrf_like());
    let src = source(&dev);
    let (node, _dir) = node_with(src.clone()).await;
    node.apply(passthrough()).await.unwrap();
    let mut reads = samples(3);
    reads.push(MockRead::Gone("LIBUSB_ERROR_NO_DEVICE".into()));
    dev.push_reads(reads);
    let reason = wait_failed(&node, 1_000).await;
    assert!(reason.starts_with("source sdr0: read failed") && reason.contains("NO_DEVICE"), "{reason}");
    assert!(wait_until(500, || dev.calls().last().map(String::as_str) == Some("abandon")).await);
    assert!(!dev.calls().contains(&"deactivate".to_string()));
    assert!(dev.calls_after_gone().is_empty(), "{:?}", dev.calls_after_gone());
    let e = src.tune(tuning(100_000_000, 1_000_000)).await.unwrap_err();
    assert!(e.to_string().contains("device lost"));
    assert!(dev.calls_after_gone().is_empty());
}

#[tokio::test]
async fn device_loss_during_a_retune_abandons_the_stream() {
    // fail_next("set_sample_rate", "LIBUSB_ERROR_NO_DEVICE") before a rate-changing tune:
    // the tune errs, the fault names set_sample_rate, the last call is "abandon".
}

#[tokio::test]
async fn an_overflow_storm_faults_the_source() {
    let dev = Arc::new(MockDevice::hackrf_like().with_reads(vec![MockRead::Overflow; 1_000]));
    let src = source(&dev);
    let _rx = src.subscribe();
    src.tune(tuning(100_000_000, 1_000_000)).await.unwrap();
    assert!(wait_until(2_000, || src.fault().is_some_and(|f| f.contains("overflow storm"))).await);
    assert_eq!(src.stats().unwrap().overflows, MAX_OVERFLOW_RUN as u64);
}

#[tokio::test]
async fn interleaved_overflows_never_fault() {
    // [Overflow, Samples(16384)] x 300: 300 blocks received, no fault, overflows == 300.
}
```

Update `unplug_mid_stream_fails_job` (`:344-367`) and `fault_is_set_before_close_and_same_band_tune_recovers` (`:372-402`): a `Fatal` read now ends in `abandon` (not `deactivate`), and "same band tune recovers" becomes `a_timeout_fault_does_not_recover_by_tune` only if that test used `Fatal`; if it uses `Timeout` reads to reach the quiet fault, the quiet fault also abandons, so the recovery assertion is replaced by the `device lost` error. Keep `retune_timeout_faults_and_next_tune_reopens` (`:244`) green: a retune timeout is set from the caller side, not a device loss, and still reopens.

- [ ] **Step 2: Run to verify they fail**: `cargo test -p zeta-source-soapy --features test-mock --test soapy vanished device_loss overflow interleaved` → compile errors (`Gone`, `abandon`) then assertion failures.
- [ ] **Step 3: Implement** per Interfaces (mock first, then trait and driver, then `run`).
- [ ] **Step 4: Run**: `cargo test -p zeta-source-soapy --features test-mock`; `RUSTFLAGS="--cfg soapy" cargo clippy -p zeta-source-soapy-driver --all-targets -- -D warnings`.
- [ ] **Step 5: Commit**: `git commit -m "Soapy: a lost device ends the reader with abandon (no deactivate/close on the dead handle) and refuses later tunes; 100 overflows with no samples fault the source"`.

---

### Task 5: Field craft, the two-node HackRF pattern, plan series, TODO, final gate (Sonnet)

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` (field craft "Reading a source's health": `stats.measured_rate_hz` against the block rate, `restarts`, `overflows`, `bank.dropped`, `/health` `stages_running`; "A radio in its own process": run the HackRF in a child `zeta serve --listen 7374` with its own state directory and attach it to the main node with `--source-node hackrf=hackrf@http://127.0.0.1:7374` so a driver crash faults the source instead of ending the node; `measure` events with `partial: true`), `README.md` (the same two routes), `docs/agents/field-craft.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 14 "Delivered", "Plan 14 status" with gates and deferred items), `TODO.md` (replace the four bug sections with one line each: "Fixed in plan 14, `docs/superpowers/plans/2026-10-27-field-fix-wave.md`"; add "Soapy worker process" to the Architecture section: a `zeta soapy-worker` child owning the `Driver`, a Port that faults when the child exits, and the secondary `sync_bank` flapping note from the investigation: a bank replaced while `feed_bank` still drains old-band blocks can swap back and forth, stalling a channel stage silently), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `cargo test -p zeta --test agents_md` (field craft embedded), `git add TODO.md` (tracked from this plan on)

- [ ] **Step 1: Write the docs** per Files. **Step 2: Regenerate** `AGENTS.md` and `SKILL.md`; `cargo test -p zeta --test agents_md --test transports`.
- [ ] **Step 3: Final gate**: `cargo fmt -p <each member> -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features --no-fail-fast` (no `FAILED`, no `ui e2e skipped`; record the UI suite's time), `cargo run -p zeta -- agents-md --check`, `--skill --check`, `RUSTFLAGS="--cfg soapy" cargo clippy -p zeta-source-soapy-driver --all-targets -- -D warnings`.
- [ ] **Step 4: Commit**: `git commit -m "Plan 14: field craft (source stats, a radio in its own process, partial measures), plan series, TODO tracked"`.

---

## Live acceptance (controller, after the final review)

On the HackRF laptop (the operator's field setup, `scratch/serve-hackrf.sh`), rebuilt from `plan-14`:

1. **Clock.** Two hours of operating with pins cycling among 1, 2, 3, 6, 12, 16 and 20 MHz and `cb_watch.py` running. Every 15 min: `source list` `stats.measured_rate_hz` within 1 % of the pinned rate, `restarts` small (each one logged as `zeta: soapy hackrf: samples ran ... ahead of the clock`), `iq read` of the last 5 s of wall time succeeds without a `too_old` error, and a transcript's `start_ns` is within 10 s of its `transcripts.log` write time.
2. **Leak.** The same session: `GET /health` `stages_running` equals the sum of the running jobs' stage counts (`graph get` node counts plus one `forward_iq` per rigid or elastic job) after every round of start/stop; CPU returns to within 20 % of the first hour's idle figure; no `channel stage lagged` line in the log while the jobs are light.
3. **measure.** `job start --kind measure` on a 12.5 kHz channel of a 12 MHz pin returns an event within 4 s with `partial: false`; under a deliberate load spike (eight spectrum jobs) events keep coming, some `partial: true`, and `job context` shows `measure.partial` counting.
4. **Device loss.** With the radio in a child node (`--source-node`), pull the USB cable while a demodulator runs: the main node stays up, `source list` shows `fault: remote node http://127.0.0.1:7374: stream closed ...`, the job fails with that reason, `serve-hackrf.sh` restarts the child, a re-pin recovers. In-process (the old single-node form), the same pull: if the node survives, the fault reads `read failed ...` or `no samples for 1000 ms` and `job list` shows the failure; a tune answers `device lost`. If the node still dies in-process, record it under "Plan 14 status" as the reason the worker process moves up the sprint.
