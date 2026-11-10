# Plan 36.2: a source that sweeps for itself — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `scan` on a source that can deliver power spectra itself asks the source for rows over the span and forwards them in the scan row shape, without touching the source's IQ channel or its lease; a KiwiSDR's `W/F` waterfall is the first such source, sweeping 0-30 MHz in under 5 s while a demodulator plays on it.

**Architecture:** `Capabilities` gains an optional `sweep` and `Port` a defaulted `sweep(SweepRequest) -> SweepRows` (a plan, a bounded receiver of rows, a state). `launch_scan` chooses `source` or `stepped` once at start, holds a `sweep` claim in the lease manager that no IQ request ever sees, and runs a forwarding loop that reuses plan 36.1's quantisers, `SweepAverage` and `timing` record. The Kiwi source opens one more websocket of its own (`/<ts>/W/F`), steps `SET zoom= cf=` across the span, and keeps only frames whose header names the asked view.

**Tech Stack:** Rust 1.97 workspace (tokio `mpsc` and `watch`, `tokio-tungstenite` as the Kiwi crate has it; no new crates); vanilla JS page; the Playwright e2e suite.

**Spec:** `docs/superpowers/specs/2026-11-10-sweep-rate-design.md`, sections 4.7 and 4.8 with the 36.2 parts of 2, 3, 5, 6 and 7. **Builds on** plan 36.1, `docs/superpowers/plans/2026-11-11-sweep-rate-stepped.md`, assumed merged: its names are used as written there.

## Global Constraints

- Verbatim from the spec: a self-sweep "does not take the time-shared lease"; a refused sweep "makes the scan try `stepped`"; the scan "does not wait and does not replace anyone" and "does not change method while running"; a Kiwi is never given "more than one user slot beyond the IQ session's"; 0-30 MHz "at 8 kHz a bin or finer" in "5 s or less"; a `W/F` frame is a 16-byte header and 1024 bytes of `dB = byte - 255`; the default target bin is `SPECTRUM_BIN_TARGET_HZ` (6 kHz) or the source's `bin_hz_min`, whichever is coarser.
- `Capabilities`, `SourceInfo`, `FftParams` and `Op` are `deny_unknown_fields`: every field added is `#[serde(default)]` and required in the schema, as plan 36.1 did for `depths`. Scan rows keep `size`, `bin_hz`, `rows_per_s`, `center_hz`, `floor_db`, `step_db`, `q`, `masked`, and 36.1's `sweeps`.
- The lease rules for IQ holders do not change: no existing test in `crates/zeta-core/src/lease.rs` is edited. No branch on a source kind in `zeta-core`, and no new dependency there.
- Tests: 2-CPU runners. Poll for the end state (`until(what, within, f)` in `tests/scan.rs` from 36.1; `wait_until(ms, f)` in `tests/kiwi.rs`); generous upper bounds only. No test needs a real Kiwi: the crate's `FakeKiwi` (`src/testing.rs`, feature `test-fake`) is extended.
- Public repository: "a KiwiSDR", "the operator's KiwiSDR", "the field host"; no host names, no frequencies of real services. `AGENTS.md` and `skills/zetasdr/SKILL.md` are regenerated in any task that changes a schema or the prose, never edited. Nothing under `scratch/` is committed, and no code from `scratch/upstream/` is built or run.
- Gates before each commit, with a private `CARGO_TARGET_DIR`, as plan 36.1's: `cargo fmt -p <crate> -- --check` per touched crate, the workspace clippy with `-D warnings`, the task's tests, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, both `agents-md --check`s (`zeta-core --features test-fixtures`). Before the branch lands: `bash scripts/public-check.sh` and `bash scripts/check-doc-links.sh`.
- One commit per task, message `Plan 36.2: <what changed>`. Deploys and live checks are the operator's word; never stop a job unasked.

## Models per task

Opus for Tasks 2, 3, 4 and 6 (the lease claim, the forwarding loop, the start decision under the gate, the `W/F` session). Sonnet for Tasks 1, 5, 7, 8 and 9 (complete briefs).

## What the KiwiSDR server's source says

Read 2026-11-11 in `jks-prv/Beagle_SDR_GPS` at `efb38e2b` (v1.711) and `jks-prv/kiwiclient` at `4eb733e6`, shallow clones under `scratch/upstream/`. Paths are in the server tree.

**Verified by reading:**

- **V1, pairing.** A new `SND` or `W/F` whose path timestamp equals a live connection's of the other type (and whose client address matches, unless bit 62 of the timestamp is set) is linked to it and takes its receiver channel, in either order: one user (`rx/rx_server.cpp:466-496`, `:684-688`).
- **V2, alone.** An unpaired `W/F` is given a receiver channel of its own, a user slot, searched from channel 0 because its path names `W/F`; with no channel free, or the free one having no waterfall, it gets `MSG too_busy=<n>` and is closed (`rx/rx_server.cpp:342`, `:565`, `:610-650`).
- **V3, where the IQ session sits.** An `SND` from a client that is not the Kiwi's own page is given the channels without a waterfall first (`rx/rx_util.cpp:223-227`). A `W/F` paired to an `SND` on such a channel is closed with no message (`rx/rx_server.cpp:672-680`).
- **V4, a pair ends together.** A `W/F` that closes, is kicked or misses its keepalive stops its paired `SND` (`rx/rx_waterfall.cpp:713-723`; a closed socket sets `kick`, `rx/rx_server.cpp:268-276`), and an `SND` stops its paired `W/F` (`rx/rx_sound.cpp:384-400`).
- **V5, one address.** With the admin's `no_dup_ip` set, a second unpaired connection from one address gets `MSG badp=5` and is kicked (`rx/rx_cmd.cpp:759-792`).
- **V6, the handshake.** After `SET auth`: `MSG center_freq= bandwidth= adc_clk_nom=` and `MSG wf_fft_size=1024 wf_fps=23 wf_fps_max=23 zoom_max=14 rx_chans= wf_chans= wf_chans_real= wf_cal= wf_setup` (`rx/rx_waterfall.cpp:226-236`). `wf_chans` is the configured count, not "none free". Frames start once zoom and start, `maxdb/mindb` and `wf_speed` are set (`:218`, `:750`); the keepalive limit is 60 s.
- **V7, a frame's view.** `x_bin_server` and the low 16 bits of `flags_x_zoom_server` are the start and zoom the frame was computed with (`:1650-1653`; the branch above it is never armed). Commands are drained between frames (`:664-674`). At `wf_speed=4` and zoom 10 or less every frame refills the sample buffer from empty (`:993`, `:1027`), so the first frame whose header names the new view was sampled after the change. A start counts `bandwidth / (1024 << zoom_max)` Hz and is clamped to the band (`:375-381`, `:488`).
- **V8, the levels.** `dB = byte - 255`, -200..0 (`:1577-1583`); the client adds `wf_cal` (`kiwirecorder.py:666`). The first 2 pixels are zeroed, 4 at zoom 0 (`:1341`). Flag bit `0x00010000` marks ADPCM; `SET wf_comp=0` turns it off (`rx/rx_cmd.cpp:1964`). `SET interp=10` is the strongest FFT bin per pixel, CIC compensated (`:576-585`). `bandwidth` is 30 or 32 MHz (`rx/rx_init.cpp:152`).
- **V9.** The status page has no line naming waterfall channels (`rx/rx_server_ajax.cpp:734-757`).

**Assumed, checked live in Task 9:** that a KiwiSDR 2 on a later version (the IQ codec was confirmed against v1.837, `crates/zeta-source-kiwi/src/proto.rs:4`) behaves as V1-V7; how long the down-converter holds samples of the old view after a change (the server's comment calls it "uncertain", `:1124`; one frame is dropped by default); the frame rate over the operator's link.

## The builder's calls (spec section 7)

- **`SweepRequest` / `SweepRows`:** Task 2's: a plan, a bounded receiver of rows, a state; one row a step a sweep, averaged over the dwell by the source; dropping the rows ends the sweep.
- **The `sweep` claim:** a second map in `LeaseManager` (`source -> jobs`), written by `claim_sweep`, cleared by `release`, read by `sweepers` and by `idle_if_unheld` only. No lease path consults it; what a source cannot do at once it refuses in `Port::sweep`, and the scan then falls back.
- **The method and the target bin** join the look in the scan's `fft` node (`method`, `bin_hz`), set by `job_start` and `graph_put`. `method` is `stepped`, `source` (fail instead of falling back) or absent (the source's sweep when the span is inside `sweep`, no `bits` is asked and the source accepts). `bin_hz` is how the operator "asks for the coarsest bins".
- **The Kiwi's zoom:** the larger of the coarsest zoom whose pixel is at or under the target and the finest zoom whose view still holds the whole span, never over 10 (above it the server samples overlapped and V7 no longer holds). The whole band at the default is zoom 3: 8 steps of 3.75 MHz, 3662 Hz a pixel.
- **The settle after `SET zoom= cf=`:** frames whose header names another view are dropped, then `wf_settle_frames` more (default 1), from V7; Task 9 measures it.
- **The alternate grid on the Kiwi:** odd sweeps place each view 8 pixels lower, so the zeroed pixels at a view's low edge are seen clean in the other sweep.
- **The fake self-sweeping port** is `ScanPort::sweeping(SweepScript)` in `crates/zeta-core/tests/scan.rs`.
- **A failed sweep is not a source fault:** its reason reads `sweep on source <id>: …`, never `source <id>: …`, which would open a fault episode for the IQ side (`node/launch.rs`, `finish`).

## Deviations from the spec, for the operator to confirm

- **The `W/F` socket always stands alone, with a timestamp of its own; it is never paired with `SND`.** 4.8 pairs them when the IQ session is open. By V4 a paired `W/F` that ends (a sweep finishing, an error on that socket) stops the IQ session under a running demodulator, against 4.8's last bullet; by V3 the pair is refused on a Kiwi with fewer waterfalls than receivers. Alone, a sweep costs one user slot beyond the IQ session's, the anti-goal's bound, and nothing reconnects when the IQ session starts or stops. Under `no_dup_ip` (V5) the sweep is refused with that reason while the IQ session is open. Pairing to save the slot is a `TODO.md` entry.
- **`MSG wf_chans=0` is not the "no waterfall channel free" sign** (V6): the refusal is `MSG too_busy=` (V2). `wf_chans=0` is read as "this Kiwi is configured without waterfalls".
- **The capability is asserted at construction**, not from the status page (V9; a record cannot change mid-run). A Kiwi that cannot sweep says so when asked.
- **`method: "source"`** and **`bin_hz`** on `job_start` are additions (the calls above). So is the page change of Task 7: without it the page refuses "Scan this span" on a held Kiwi before the node is asked.

## Review Focus

1. **A `W/F` socket that never shows the asked view** (frames of another zoom, or none after the handshake): each step times out and is skipped, a whole sweep of skips is a lost connection, and the scan says `waiting`; nothing hangs. Task 6 (`a_view_that_never_arrives_is_skipped_not_hung`).
2. **A `--source-node` source copying a far Kiwi's `sweep`** into its own record: every scan on it would ask a port that cannot sweep. Task 2 (`a_node_source_does_not_claim_the_remotes_sweep`).
3. **The IQ session faulting or going idle under a running sweep** (the Kiwi full on the IQ side, the last IQ holder leaving): the sweep carries on and the scan is neither failed nor relaunched as a source fault. Tasks 3 and 6 (`a_sweep_the_source_gives_up_on_fails_the_scan_without_faulting_the_source`, `a_sweep_runs_beside_the_iq_session_and_neither_ends_the_other`).
4. **A node slower than the source's rows:** the channel is bounded and the source drops, counted as skipped, never queued without limit. Task 3 (`a_slow_reader_costs_rows_not_memory`).
5. **Two sockets opened in the same second.** `path_ts` is seconds plus the pid, so the `W/F` socket would pair with the `SND` by accident (V1, then V4). Task 6 (`the_wf_socket_never_uses_the_snd_timestamp`).

---

### Task 1: The server's source, checked and written down (Sonnet)

**Files:**
- Create: `docs/kiwi-waterfall.md`
- Modify: `docs/README.md` (one index line), `scratch/upstream/README.md` (untracked: a row per clone, as its table has)

Read-only: nothing from the clones is built or run.

- [ ] **Step 1: The clones.** Reuse `scratch/upstream/Beagle_SDR_GPS` and `scratch/upstream/kiwiclient` when present, else `git clone --depth 1` each from `https://github.com/jks-prv/<name>`. Add a row for each to the README's table.
- [ ] **Step 2: Check V1-V9** above, each at its file and line; where the checkout has moved, find the same code and note the new line. If the server's author publishes a later tree for the KiwiSDR 2, check V1-V5 and V7 there too. **If V1, V2, V4 or V7 does not hold, stop and report:** Tasks 5 and 6 rest on them.
- [ ] **Step 3: Write `docs/kiwi-waterfall.md`** (under 80 lines, roles only): the frame and its header, the commands and their order, the handshake messages, how a `W/F` socket is counted and paired, what ends what, the refusals and their texts, the settle reasoning, and a last section "Not verified" with the assumed list. Cite the server by repository, commit and path.
- [ ] **Step 4: Run** `bash scripts/check-doc-links.sh && bash scripts/public-check.sh`: clean.
- [ ] **Step 5: Commit**: `Plan 36.2: the KiwiSDR waterfall stream as the server's source has it: the frame, the commands, how a W/F socket is counted, paired and ended`.

### Task 2: The seam: the capability, `Port::sweep`, the claim (Opus)

**Files:**
- Modify: `crates/zeta-core/src/types.rs` (`SweepCaps`, `Capabilities.sweep`), `crates/zeta-core/src/port.rs` (the sweep types, `Port::sweep`), `crates/zeta-core/src/lease.rs` (`sweeps`, `claim_sweep`, `sweepers`, `release`, `idle_if_unheld`), `crates/zeta-core/src/ops.rs` (`SourceInfo.sweeps` and where `source_list` fills it), `crates/zeta-core/src/node/mod.rs` (`Node::lease_sweepers`), every `Capabilities { .. }` literal (a scripted edit adding `sweep: None`, as 36.1's Task 4 did), `crates/zeta-source-node/src/port.rs` (the copied record sets `sweep: None`)
- Test: unit tests in `types.rs`, `port.rs`, `lease.rs`; `crates/zeta-core/tests/ops.rs`; `crates/zeta-source-node/tests/port.rs`

**Interfaces:**
- Consumes: 36.1's `Capabilities` (with `usable_fraction`, `depths`) and its schema rule.
- Produces:
  - `pub struct SweepCaps { pub min_hz: Hz, pub max_hz: Hz, pub bin_hz_min: Hz, pub rows_per_s_max: u32, pub calibrated: bool }`; `Capabilities.sweep: Option<SweepCaps>` (absent reads `None`; nullable and required in the schema); `SweepCaps::holds(&self, span: Tuning) -> bool` (the span's edges inside `min_hz..=max_hz`).
  - In `port.rs`:
    ```rust
    pub struct SweepRequest { pub span: Tuning, pub bin_hz: Hz, pub dwell_ms: u64 }
    pub struct SweepPlan { pub steps: u32, pub step_hz: Hz, pub size: u32, pub bin_hz: f64,
                           pub dwell_ms: u64, pub shift_hz: Hz }
    pub struct SweepRow { pub sweep: u64, pub step: u32, pub center_hz: Hz, pub bin_hz: f64,
                          pub db: Vec<f32>, pub masked: Vec<[usize; 2]>, pub span: Span }
    pub enum SweepState { Running, Waiting(String), Ended(String) }
    pub struct SweepRows { pub plan: SweepPlan, pub rows: mpsc::Receiver<SweepRow>,
                           pub state: watch::Receiver<SweepState> }
    pub const SWEEP_ROWS_CAPACITY: usize = 64;
    async fn sweep(&self, req: SweepRequest) -> Result<SweepRows, PortError>;
    ```
    `Port::sweep` defaults to `Err(PortError::Unsupported("source does not sweep for itself".into()))`. Its doc is the contract: the source chooses its stepping; `plan.dwell_ms` is the dwell in force, `plan.shift_hz` the odd sweeps' shift (0: none); `db` is `plan.size` values, dBm when the capability says `calibrated`; `masked` is `[lo, hi)` bin pairs; a row holds only samples of its own view (4.1's contract for this stream); a full channel drops the row; `Waiting` is the source retrying, `Ended` precedes the close; dropping `SweepRows` ends the sweep and frees what it held upstream.
  - `LeaseManager::claim_sweep(&self, source: &SourceId, holder: JobId)` (idempotent, never refuses); `sweepers(&self, source: &SourceId) -> Vec<JobId>`; `release(holder)` also drops its claims; `idle_if_unheld` does not call `Port::idle` while a claim is held. No other method reads the claims.
  - `SourceInfo.sweeps: Vec<JobId>` (`#[serde(default)]`): the scans a source is sweeping for; `Node::lease_sweepers(&self, source: &SourceId) -> Vec<JobId>`.

- [ ] **Step 1: Write the failing tests**:
  - `types.rs`: `a_record_without_sweep_loads_as_none`; `sweep_caps_hold_a_span_inside_them_only` (0..30 MHz holds 15 MHz ± 15 MHz, not 15 MHz ± 15.001 MHz); the schema test still requires every field.
  - `port.rs`: `a_port_does_not_sweep_by_default` (the `Unsupported` text above).
  - `lease.rs`: `a_sweep_claim_is_invisible_to_every_iq_request` (with a claim held: `ensure` tunes an unheld port, `acquire_shared` is granted, `holders` and `elastic_holders` are empty, `free_sources` lists the port, `time_shared` is false); `a_sweep_claim_is_granted_beside_a_rigid_and_a_time_shared_lease`; `release_drops_a_holders_claim`; `a_port_is_not_idled_under_a_sweep_claim` (a fake port counting `idle` calls: 0 with the claim, 1 after `release`).
  - `ops.rs`: `a_source_record_without_sweeps_still_parses`.
  - node source `port.rs`: `a_node_source_does_not_claim_the_remotes_sweep` (a remote `source_list` whose record has `sweep`: the port's capabilities have `None`).
- [ ] **Step 2: Run** `cargo test -p zeta-core --lib lease::tests::a_sweep_claim`: compile error.
- [ ] **Step 3: Implement**; the scripted literal edit.
- [ ] **Step 4: Run** the workspace tests; regenerate `AGENTS.md` and the skill. All pass.
- [ ] **Step 5: Commit**: `Plan 36.2: a source can say it sweeps for itself (Capabilities.sweep, Port::sweep) and a scan can hold a sweep claim no IQ request sees`.

### Task 3: The self-sweep scan loop (Opus)

**Files:**
- Modify: `crates/zeta-core/src/node/scan.rs` (`launch_scan`'s choice, `sweep_loop`)
- Test: `crates/zeta-core/tests/scan.rs`

**Interfaces:**
- Consumes: Task 2; from 36.1: `launch_scan` in `node/scan.rs`, the look in the scan's `fft` node (`dwell_ms`, `average`, `bits`), `NodeConfig::scan_dwell_ms`, `zeta_core::spectrum::{quantise_row_masked, SweepAverage, STEP_DB}`, `SweepTiming` (`step`, `finish`), the once-a-second rule for writing `timing`, `Node::job_context_merge`, `SPECTRUM_BIN_TARGET_HZ`.
- Produces:
  - In `launch_scan`, before the stepped path's lease: when the source's `sweep` holds the span and the stored node asks no `bits`, `claim_sweep`, then `port.sweep(SweepRequest { span, bin_hz: max(sweep.bin_hz_min, SPECTRUM_BIN_TARGET_HZ), dwell_ms })`. On `Ok` the scan is recorded running with `sweep_loop` as its one task: no time-shared lease, no `stop_elastic`, `AfterGate::Nothing`, no `set_depth`. On `Err` the claim is released and, in this task, the scan goes on to the stepped path (Task 4 gives the fallback its rules). The call is made under the gate when `launch_scan` holds one, as a first `tune` is.
  - `context.scan` for such a scan: `{method: "source", steps, step_hz, bin_hz, dwell_ms, dwell_ms_asked (when it differs), average, alternate: {shift_hz, steps_shifted: steps} or null when `shift_hz` is 0, mask: {notch_hz: 0, edge_mask: 0.0}, calibrated, waiting: null, timing}`. A stepped scan's record gains `method: "stepped"`.
  - `sweep_loop`: each `SweepRow` becomes one event on the scan's stream, channel `scan/<row.step>`, provenance span `row.span`: `size`, `bin_hz`, `rows_per_s` (`clamp(round(1000 / plan.dwell_ms), 1, 60)`), `center_hz`, `floor_db` and `q` from `quantise_row_masked(&row.db, &row.masked)`, `step_db`, `masked` (when not empty), `sweeps`, and `calibrated: true` when the capability says so. With `average` over 1 the row goes through `SweepAverage::push((center_hz, 0), ..)` and is requantised from the mean, as 36.1's FFT task does.
  - `timing`: a row is `SweepTiming::step(Duration::ZERO, since the last row, false)`; when `row.sweep` changes, each step of the finished sweep that sent no row is a `step(.., true)`, then `finish(span_hz, the sweep's time)`.
  - `state`: `Waiting(r)` writes `context.scan.waiting = r`, `Running` writes `null`. The rows closing ends the job as `StageError::Failed("sweep on source <id>: <the Ended reason, or "ended">")`. Cancel drops the `SweepRows` first. The loop never reads `Port::fault`.

- [ ] **Step 1: Write the failing tests** (`ScanPort::sweeping(SweepScript)`: `sweep` covers the tune range, `bin_hz_min: Hz(1000)`, `calibrated: true`; `sweep()` records the request and returns a plan of 4 steps of 1024 bins, then sends a row every 5 ms, step by step, `db` constant per step, `masked: [[0, 2]]`. `SweepScript { refuse: Option<String>, wait_after: Option<usize>, end_after: Option<usize>, skip_step: Option<u32>, burst: bool }`: `wait_after` sets `Waiting("socket lost")` for 300 ms after that many rows, `end_after` sets `Ended("gone")` and closes, `burst` sends without pause. `requests()`, `live()` (sweeps whose rows are not dropped yet), `dropped()` (rows a full channel refused)):
  - `a_self_sweeping_port_is_asked_and_its_rows_come_out_in_the_scan_row_shape`: rows on `scan/0..=3` in order, each with `size 1024`, `q` of 1024, `step_db 0.5`, `masked [[0, 2]]`, `sweeps 1`, `calibrated true`, the step's `center_hz`; `port.tunes()` stays empty; `context.scan.method == "source"`, `steps == 4`.
  - `a_self_sweep_takes_no_lease_and_runs_beside_a_demodulator`: a rigid `measure` job, then the scan: both `running`; `source_list` has `holders == [measure]`, `sweeps == [scan]`; an elastic `spectrum` job started before the scan is still `running` after it.
  - `stopping_a_self_sweep_drops_the_rows_and_the_claim`: `until` `live() == 0` and `sweeps` is empty.
  - `dwell_ms_goes_to_the_source_and_average_is_the_nodes`: the request has `dwell_ms 200` and `bin_hz 6000`; with `average: 4` a centre's rows carry `sweeps` 1, 2, 3, 4, 4.
  - `a_self_sweeps_record_fills_the_same_timing`: `until` `timing.sweep_ms > 0`; every key of 36.1's record is present; `tune_ms == {median: 0, max: 0}`; with `skip_step: Some(2)`, `until` `timing.skipped >= 1`.
  - `a_lost_sweep_says_waiting_and_resumes`: `until` `waiting == "socket lost"`, then `until` it is `null`; the job stayed `running`, `method` stayed `source`, no tune was made.
  - `a_sweep_the_source_gives_up_on_fails_the_scan_without_faulting_the_source`: the job is `Failed` with a reason starting `sweep on source radio0: gone`; `source_list[].fault` is `null`; a `measure` job on the source is still `running`.
  - `a_slow_reader_costs_rows_not_memory` (`burst`): `until` `dropped() > 0`; rows still arrive in step order; the job is `running`.
  - `a_span_outside_the_sources_sweep_is_stepped` and `a_scan_with_bits_is_stepped` (`requests()` empty, tunes made, `method == "stepped"`).
- [ ] **Step 2: Run** `cargo test -p zeta-core --test scan -- self_sweep`: fails (`requests()` is empty).
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** `cargo test -p zeta-core --features test-fixtures --test scan --test resume --test recover`: pass.
- [ ] **Step 5: Commit**: `Plan 36.2: a scan asks a source that sweeps for itself and forwards its rows in the scan row shape, under a sweep claim; the record says method, waiting and the same timing`.

### Task 4: `method`, `bin_hz` and the fallback (Opus)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`ScanMethod`, `Op::JobStart.method`, `.bin_hz`, `SCAN_BIN_MAX_HZ = 1_000_000`, the checks), `crates/zeta-dsp/src/fft.rs` (`FftParams.method`, `.bin_hz`), `crates/zeta-core/src/node/scan.rs`, `crates/zeta/src/cli.rs` (`--method`, `--bin-hz`), every `Op::JobStart { .. }` literal (36.1's scripted edit, adding `method: None, bin_hz: None`)
- Test: `crates/zeta-core/tests/scan.rs`, `tests/ops.rs`, `tests/resume.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Consumes: Task 3's choice in `launch_scan`; how 36.1 carries `dwell_ms` from `job_start` into the `fft` node and through `graph_put`.
- Produces:
  - `pub enum ScanMethod { Stepped, Source }` (`snake_case`); `Op::JobStart { .., method: Option<ScanMethod>, bin_hz: Option<u64> }`, required and nullable in the schema; `bin_hz` 1..=`SCAN_BIN_MAX_HZ` else `out_of_range` naming it; either on another kind is refused as `dwell_ms` is. Both are stored in the scan's `fft` node and changed by `graph_put`, which restarts the scan as for the mask.
  - The start, in order: `stepped`, or a `bits`: the stepped path, the port never asked. `source` with a span the capability does not hold: `other`, `source <id> does not sweep <lo>-<hi> Hz for itself`. Otherwise ask the port, with `bin_hz` or the default. A refusal under `source` is the start's error, `sweep on source <id>: <reason>`. A refusal under no method goes to the stepped path: granted, `context.scan` has `method: "stepped"` and `fallback: <reason>`; a `conflict` there keeps its holders and gains the option `the source refused to sweep for itself: <reason>`. Nothing is stopped or replaced that `replace` did not name.
  - `bin_hz` with `method: "stepped"` is `other`: `bin_hz applies to a source that sweeps for itself`.

- [ ] **Step 1: Write the failing tests**:
  - `scan.rs`: `a_refused_sweep_falls_back_to_stepped_and_says_why` (`refuse: "no waterfall channel free"`: tunes are made, `method == "stepped"`, `fallback` is the reason); `a_refused_sweep_under_a_held_channel_is_the_conflict_with_the_reason` (a `measure` job first: `conflict`, `holders == [measure]`, an option holds the reason, the `measure` job is `running`, no scan is listed); `method_stepped_never_asks_the_source`; `method_source_fails_instead_of_falling_back` (the error holds the reason, no tune); `method_source_outside_the_sweep_is_refused_naming_the_span`; `bin_hz_goes_to_the_source` (`bin_hz: 30000` in the request; without it 6000); `graph_put_changes_the_method_on_a_running_scan` (to `stepped`: `until` `live() == 0` and tunes are made).
  - `resume.rs`: `a_stored_self_sweep_resumes_as_one`.
  - `ops.rs`: `method_and_bin_hz_on_another_kind_are_refused`; `bin_hz_out_of_range_names_the_field` (0, 1_000_001); `bin_hz_with_method_stepped_is_refused`; `job_start_without_method_still_parses`. `cli.rs`: `job_start_takes_method_and_bin_hz`.
- [ ] **Step 2: Run** `cargo test -p zeta-core --test scan -- method`: compile error.
- [ ] **Step 3: Implement**; the literal edit.
- [ ] **Step 4: Run** the workspace tests; regenerate `AGENTS.md` and the skill. All pass.
- [ ] **Step 5: Commit**: `Plan 36.2: a scan takes method and bin_hz; a refused sweep falls back to stepped and says why, and under a held channel is the usual conflict with the source's reason`.

### Task 5: The `W/F` codec and the sweep's plan (Sonnet)

**Files:**
- Create: `crates/zeta-source-kiwi/src/wf.rs` (the plan only, in this task)
- Modify: `crates/zeta-source-kiwi/src/proto.rs`, `src/lib.rs` (`mod wf`), `src/session.rs` (`Conn::on_msg` ignores the new messages)
- Test: unit tests in `proto.rs` and `wf.rs`

**Interfaces:**
- Consumes: V6-V8.
- Produces, in `proto.rs`:
  - `Tag::Wf` for `b"W/F"` (`tags_are_recognised` is updated); `wf_path(ts: u32) -> String` (`/<ts>/W/F`), `wf_url(host, port, ts)`.
  - `pub const WF_PIXELS: usize = 1024; WF_HEADER: usize = 16; WF_FLAG_COMPRESSED: u32 = 0x0001_0000;`
  - `pub struct Wf { pub x_bin: u32, pub zoom: u8, pub seq: u32, pub db: Vec<f32> }`; `pub fn parse_wf(payload: &[u8]) -> Result<Wf, ProtoError>`: `[0..3]` `W/F`, `[3]` pad, `[4..8]` `x_bin` LE, `[8..12]` flags and zoom LE (zoom the low 16 bits), `[12..16]` `seq` LE, `[16..]` exactly `WF_PIXELS` bytes, `db = byte - 255`. New errors: `ProtoError::WfCompressed`, `ProtoError::WfPixels { bytes: usize }`; a short frame is `Short`.
  - `cmd_wf_view(zoom: u8, cf_hz: u64) -> String` (`SET zoom=<z> cf=<kHz, three decimals, from integers>`); `CMD_WF_COMP_OFF = "SET wf_comp=0"`, `CMD_WF_DB = "SET maxdb=0 mindb=-100"`, `CMD_WF_INTERP = "SET interp=10"`, `CMD_WF_SPEED = "SET wf_speed=4"`.
  - `KiwiMsg::{WfChans(u32), WfCal(i32), ZoomMax(u8), Bandwidth(u64), WfSetup}` for `wf_chans=`, `wf_cal=`, `zoom_max=`, `bandwidth=`, `wf_setup`.
  - Under `test-fake`: `encode_wf(x_bin: u32, zoom: u8, flags: u32, seq: u32, pixels: &[u8]) -> Vec<u8>`.
- Produces, in `wf.rs`:
  - `pub(crate) const WF_ZOOM_MAX: u8 = 10; WF_FPS: u32 = 23; WF_SHIFT_PIXELS: u64 = 8; WF_SETTLE_FRAMES: u32 = 1;`
  - `pub(crate) struct WfPlan { pub zoom: u8, pub view_hz: u64, pub bin_hz: f64, pub starts: Vec<u64>, pub shift_hz: u64, pub frames: u32, pub dwell_ms: u64 }` with `fn start(&self, sweep: u64, step: usize) -> u64` (odd sweeps `shift_hz` lower, never under 0) and `fn x_bin(&self, start_hz: u64, bandwidth_hz: u64, zoom_max: u8) -> u32` (`round(start × (1024 << zoom_max) / bandwidth)`).
  - `pub(crate) fn wf_plan(span: Tuning, bin_hz: u64, dwell_ms: u64, bandwidth_hz: u64) -> WfPlan`: `view(z) = bandwidth >> z`. `z_target` is the smallest `z` in `0..=WF_ZOOM_MAX` with `view(z) / 1024 <= bin_hz` (else `WF_ZOOM_MAX`); `z_fit` the largest with `view(z) >=` the span's width; `zoom = max(z_target, z_fit)`. One view that holds the span is centred on it; otherwise `n = ceil(width / view)` starts at `lo + k × view`. Every start is clamped to `0..=bandwidth - view` and a repeated start is dropped. `shift_hz = WF_SHIFT_PIXELS × view / 1024`, 0 at zoom 0. `frames = max(1, ceil(dwell_ms × WF_FPS / 1000))`, `dwell_ms = round(frames × 1000 / WF_FPS)`.

- [ ] **Step 1: Write the failing tests**:
  - `proto.rs`: `parses_a_wf_frame_built_to_the_servers_format` (`encode_wf(4096, 3, 0, 7, ..)` with byte 155 at pixel 5 and 55 at pixels 0-1: `x_bin 4096`, `zoom 3`, `seq 7`, `db[5] == -100.0`, `db[0] == -200.0`, 1024 values); `a_compressed_wf_frame_is_refused`; `a_wf_frame_of_another_width_is_refused` (512 and 1025 bytes); `short_wf_frames_are_errors_not_panics` (every length under 16); `wf_commands_are_exact` (`cmd_wf_view(3, 1_875_000) == "SET zoom=3 cf=1875.000"`, the four constants); `parses_the_wf_setup_msgs` (V6's two lines: `WfChans(4)`, `WfCal(-13)`, `ZoomMax(14)`, `WfSetup`, `Bandwidth(30_000_000)`).
  - `wf.rs` (bandwidth 30 MHz): `the_whole_band_at_6_khz_is_zoom_3_in_eight_steps` (starts `k × 3_750_000`, `bin_hz` within 0.1 of 3662.1); `at_8_khz_it_is_zoom_2_in_four`; `the_coarsest_bins_are_zoom_0_in_one_step_with_no_shift` (`bin_hz 30_000`); `a_narrow_span_takes_the_finest_zoom_that_holds_it` (300 kHz wide: zoom 6, one start, the span centred); `views_clamp_at_the_band_edges_without_a_repeated_step` (29-30 MHz at zoom 6); `the_zoom_never_passes_10` (`bin_hz 1`); `the_odd_sweep_sees_each_zeroed_pixel_clean` (for each step, pixels 0-3 of the even view lie in pixels 4.. of the odd one, and the reverse, but for the view at 0 Hz); `a_dwell_is_whole_frames_at_least_one` (20 gives `(1, 43)`, 250 gives `(6, 261)`).
- [ ] **Step 2: Run** `cargo test -p zeta-source-kiwi --lib wf`: compile error.
- [ ] **Step 3: Implement**; the module doc of `proto.rs` names both streams.
- [ ] **Step 4: Run** `cargo test -p zeta-source-kiwi`: pass.
- [ ] **Step 5: Commit**: `Plan 36.2: the KiwiSDR W/F frame codec and commands, and the sweep's plan of zoom and views`.

### Task 6: The Kiwi's waterfall session and `KiwiSource::sweep` (Opus)

**Files:**
- Modify: `crates/zeta-source-kiwi/src/wf.rs` (the session), `src/source.rs` (`caps.sweep`, `Port::sweep`, `KiwiConfig.wf_settle_frames`, the live sweep's handle), `src/session.rs` (`path_ts`, `close`, `badp_meaning` and the frame limits become `pub(crate)`), `src/testing.rs` (the fake serves `W/F`)
- Test: `crates/zeta-source-kiwi/tests/kiwi.rs`

**Interfaces:**
- Consumes: Task 5; Task 2's types; `KiwiConfig` (`reconnect_delays_ms`, `read_timeout_ms`, `handshake_timeout_ms`), `Status`.
- Produces:
  - `KiwiSource::new` asserts `sweep: Some(SweepCaps { min_hz: Hz(0), max_hz: Hz(30_000_000), bin_hz_min: Hz(29), rows_per_s_max: 23, calibrated: true })`.
  - `pub(crate) fn wf_ts() -> u32` = `path_ts() ^ 0x8000_0000`: never an `SND` timestamp of this process.
  - `KiwiSource::sweep`: a span outside 0-30 MHz is `OutOfRange`; a sweep already live on this source is `Unsupported("a sweep is already running on this Kiwi (one waterfall connection)")`. Else it spawns `wf::run` and waits, at most `handshake_timeout_ms`, for the first connection: `Ok(SweepRows)` once `wf_setup` and `bandwidth` have arrived, else an error with the reason: `too_busy` as `Unsupported("no waterfall channel free (too_busy: <n> channels)")`, `wf_chans=0` as `Unsupported("this Kiwi is configured without waterfall channels")`, `badp` through `badp_meaning`, `down`, and transport failures as `Io`. It never touches the IQ session, `Status.fault` or `Status.current`.
  - `wf::run`, per connection: `SET auth`; on `wf_setup`: `CMD_IDENT`, `CMD_WF_COMP_OFF`, `CMD_WF_DB`, `CMD_WF_INTERP`, `CMD_WF_SPEED`, then the first step's `cmd_wf_view`; `SET keepalive` every second. Per step: a frame belongs to it when its zoom is the plan's and its `x_bin` is within 1 of the plan's; others are dropped; the first `wf_settle_frames` that belong are dropped; the next `plan.frames` are averaged in linear power per pixel and sent as one `SweepRow` (`center_hz` from the frame's own `x_bin` and the server's `bandwidth`, `db` plus the last `wf_cal`, `masked` `[[0, 4]]` at zoom 0 else `[[0, 2]]`, `span` from the first to the last kept frame's arrival) with `try_send`. A step with no row after `(wf_settle_frames + frames + 10)` frame times is skipped. Ten bad frames in a row (`parse_wf` errors) lose the connection, as does a sweep whose every step was skipped.
  - A lost connection sets `Waiting(reason)` and reconnects after `reconnect_delays_ms` in turn (the budget resets after a whole sweep delivered); the budget spent, or a refusal on a reconnect, sets `Ended(reason)` and ends the task. The rows' receiver dropped, or the source dropped, closes the socket cleanly.
  - `FakeScript.wf: FakeWf { on_auth: Vec<FakeReply>, wf_chans: u32, wf_cal: i32, lag_frames: usize, frames_per_connection: Option<usize>, compressed: bool, ignore_view: bool }`: a connection whose path ends `/W/F` gets V6's messages, then a frame every `frame_interval_ms` whose header follows the last `SET zoom= cf=` after `lag_frames` more frames of the view before; pixels 0-1 are 55 and the rest `100 + (x_bin / 4096) % 100`. `FakeKiwi::wf_connections()`.

- [ ] **Step 1: Write the failing tests** (`frame_interval_ms: 5`):
  - `the_wf_socket_never_uses_the_snd_timestamp` (a tune and a sweep in the same second: `paths()` has one `/…/SND` and one `/…/W/F` with different timestamps).
  - `sweep_sends_the_exact_commands_in_order` (after auth: ident, `SET wf_comp=0`, `SET maxdb=0 mindb=-100`, `SET interp=10`, `SET wf_speed=4`, `SET zoom=3 cf=1875.000`).
  - `a_whole_band_sweep_is_eight_rows_of_1024_calibrated_bins` (`plan.steps == 8`; the first sweep's centres are `1_875_000 + k × 3_750_000`; `db[5] == pixel - 255 + wf_cal`; `masked == [[0, 2]]`; the second sweep's centres are 8 pixels lower but for the first).
  - `frames_of_another_view_and_the_settle_frame_are_dropped` (`lag_frames: 3`: every row's `db[5]` is its own view's value).
  - `a_sweep_runs_beside_the_iq_session_and_neither_ends_the_other` (blocks flow before, during and after a sweep; dropping the rows makes one clean close and blocks still flow; then, with a new sweep, dropping every IQ receiver ends the `SND` and rows still arrive).
  - `no_waterfall_channel_refuses_the_sweep_with_the_reason` (`TooBusy(4)` on `W/F`; `wf_chans: 0`; `BadPassword(5)`): each `sweep()` is an error holding its text, and `fault()` stays `None`.
  - `a_second_sweep_is_refused_while_one_runs`, and granted after the first is dropped.
  - `a_lost_wf_socket_says_waiting_then_resumes` (`frames_per_connection: Some(40)`: the state passes through `Waiting`, rows resume, `wf_connections() >= 2`).
  - `reconnects_spent_end_the_sweep_with_the_reason` (no delays configured: `Ended`, the rows close).
  - `a_view_that_never_arrives_is_skipped_not_hung` (`ignore_view`, a 300 kHz span, so one step: no row; `Waiting` within 5 s).
  - `compressed_frames_are_never_rows` (`compressed`: no row; `Waiting`).
  - `a_scan_on_a_fake_kiwi_sweeps_for_itself_beside_a_demodulator` (through `node_with`: a `measure` job, then `job_start` `scan` over 0-30 MHz: `context.scan.method == "source"`, `steps == 8`, rows arrive on the scan's stream, the `measure` job is `running`, `paths()` has exactly one of each socket).
- [ ] **Step 2: Run** `cargo test -p zeta-source-kiwi --test kiwi -- sweep`: compile error.
- [ ] **Step 3: Implement**; the module doc of `wf.rs` states the standing-alone rule and cites `docs/kiwi-waterfall.md`.
- [ ] **Step 4: Run** `cargo test -p zeta-source-kiwi` and `cargo test -p zeta --test cli`: pass.
- [ ] **Step 5: Commit**: `Plan 36.2: a KiwiSDR sweeps for itself over one W/F socket of its own, stepping zoom and centre, keeping only frames of the asked view; its IQ session is untouched`.

### Task 7: The page lets a self-sweeping source be scanned while held (Sonnet)

**Files:**
- Modify: `crates/zeta-ui/web/waterfall.js` (`release` and its header comment), `crates/zeta-ui/web/coarse.js` (the scanner's choice and its header comment), `crates/zeta-ui/web/menu.js` (the `scan` entry's comment)
- Test: `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Consumes: `source_list[].capabilities.sweep` (Task 2).
- Produces: `export function sweepsItself(src, lo, hi)` in `waterfall.js` (true when `src.capabilities.sweep` holds `lo..hi`). `wf.release(src, span)` does not refuse for rigid holders when the source sweeps the span itself; the node decides. In `coarse.js` a source that sweeps the view itself is a scanner whatever holds or pins it, the shown one included. Rows need no change (checked in `waterfall.js`): the page keys on `center_hz`, `bin_hz` and `size` and colours each row from its own floor, so 1024 calibrated bins at a zoom's width paint as any step does.

- [ ] **Step 1: Write the failing scenario** `scenario_scan_entry_on_a_self_sweeping_source_asks_the_node`, beside `scenario_scan_entry_starts_a_scan_job`: with a `page.route("**/op", …)` that fetches each `source_list` reply and adds `sweep: {min_hz: 0, max_hz: 1e12, bin_hz_min: 1, rows_per_s_max: 23, calibrated: true}` to `tone`'s capabilities, and a rigid `measure` job running on `tone`: "Scan this span" posts a `job_start` of kind `scan` (seen by the route), and the transcript shows the node's `conflict` (a recording does not sweep), not the page's `conflict: source tone held by`. Without the route the page's own notice appears and nothing is posted. Unroute in a `finally`.
- [ ] **Step 2: Run** `ZETA_UI_ONLY=scan_entry_on_a_self_sweeping_source_asks_the_node cargo test -p zeta --test ui -- --nocapture`: fails (nothing posted); check it did not print `ui e2e skipped`.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** the whole `cargo test -p zeta --test ui -- --nocapture`: the existing scan and coarse scenarios pass unchanged.
- [ ] **Step 5: Commit**: `Plan 36.2: the page asks the node for a scan on a source that sweeps for itself, whoever holds its IQ channel`.

### Task 8: Prose, docs and bookkeeping (Sonnet)

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` (the `job_start`, `job_context` and `source_list` notes), `docs/agents/field-craft.md`, `docs/guide.md` (the KiwiSDR and scan sections), `docs/kiwi-waterfall.md` (what was built), `TODO.md`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md`, `AGENTS.md` and `skills/zetasdr/SKILL.md` (regenerated)

- [ ] **Step 1: Write the prose.** The agents' guide and the field craft say: a source with `capabilities.sweep` sweeps for a scan itself, leaving its IQ channel and holders alone; `method` and `bin_hz`; `context.scan.method`, `fallback`, `waiting`; a row's `calibrated` (absolute dBm, which a local radio's rows are not: never compare levels across the two); `source_list[].sweeps`; on a KiwiSDR a sweep takes one more user slot while it runs and paints the whole band about once a second at the default, the first pixels of each view masked; a refused sweep is a `conflict` with the Kiwi's reason when its IQ channel is held, else a stepped scan whose record says `fallback`; a `--source-node` source does not forward a far source's sweep.
- [ ] **Step 2: `TODO.md`** (generic text, roles only): in "Sources: a source that delivers spectrum rows" note what plan 36.2 built (a scan's rows) and what stays (a `spectrum` or `watch` reading them; a `--source-node` forwarding them); close outcome (a) of "Spike: wideband views from the Kiwi and from custom B210-clone gateware". New entries: "Kiwi: pair the waterfall socket with the IQ session to save a user slot" (V1, V3, V4: the socket would be parked, not closed, at a sweep's end); "Sources: the HackRF's firmware sweep behind `Port::sweep`"; "Page: a self-sweep's rows as the coarse layer under the same source's fine capture" (today the waterfall follows the scan and hides the fine rows).
- [ ] **Step 3: Plan series**: a row for 36.2 and a "Plan 36.2 status" section (what was built, the calls and deviations above, the branch and commits).
- [ ] **Step 4: Run** the two regenerations, both `--check`s, `bash scripts/check-doc-links.sh`, `bash scripts/public-check.sh`: all clean.
- [ ] **Step 5: Commit**: `Plan 36.2: docs: self-sweeping sources, method and bin_hz, the Kiwi's waterfall sweep and its slot in the agent guide, the field craft and the guide; TODO entries; plan series`.

### Task 9: Live acceptance, written for the operator (Sonnet)

**Files:**
- Modify: `UAT.md` (a "Plan 36.2: a source that sweeps for itself" section in the house form)

Written, not run: the deploy and each check are the operator's word.

- [ ] **Step 1: Write the section** with these checks, against "a KiwiSDR attached to the field host's node": (1) with a demodulator playing on the Kiwi, a scan of 0-30 MHz at the default: `context.scan.method` is `source`, `timing.sweep_ms` is 5000 or less, the audio does not break and the demodulator's job never leaves `running`; (2) the Kiwi's status page shows `users` one higher while the sweep runs and back after, and stopping the scan does not end the IQ session (V1-V4 on this Kiwi's version, which is written into the result); (3) a steady carrier reads within 3 dB of the Kiwi's own page at the same zoom, `wf_cal` noted; (4) the settle: a steady carrier's level in the fast look's rows against the long look's (six frames a step) agrees within 0.5 dB, else `WF_SETTLE_FRAMES` is raised and the figure recorded; (5) with every waterfall channel taken by other users: with the demodulator running the scan is a `conflict` naming it with the Kiwi's reason, and without it the scan runs `stepped` and its record says `fallback`; (6) `bin_hz` 30000 is one step a sweep, and a 300 kHz span is one step at a finer zoom; (7) `average` 16 shows a weak steady carrier the fast look does not; (8) the network path to the Kiwi cut for ten seconds: the record says `waiting`, then the rows resume; (9) the page on the Kiwi's tab: "Scan this span" with the demodulator running paints the whole band; (10) `--method stepped` on the same span, for the record: its sweep time beside the source's.
- [ ] **Step 2: Run** `bash scripts/public-check.sh && bash scripts/check-doc-links.sh`: clean.
- [ ] **Step 3: Commit**: `Plan 36.2: UAT checks for the Kiwi's waterfall sweep`.

---

## Self-review notes

- Spec coverage: 4.7's capability, trait and claim are Task 2; its scan, averaging and dwell Task 3; its fallback and mid-run rules Tasks 3, 4 and 6; 4.8 is Tasks 1, 5 and 6; section 3's record and one-operation-set constraints Tasks 2 and 4; section 5's prose row Task 8; section 6's codec, fake-port and live proofs Tasks 5, 3-4 and 9. The pairing rule, the `wf_chans=0` sign and the status page's part are the deviations above.
- The budget behind the 5 s goal: 8 steps of one dropped and one kept frame at 23 frames a second is about 0.7 s a sweep, the long look (6 kept frames) about 2.4 s.
