# Scan Grid and Mask Implementation Plan (plan 15)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `scan` whose rows can be trusted: alternate sweeps shift the step grid by half a step so no frequency sits under a step centre on every sweep, and the bins a step cannot see (the receiver's DC notch, the outer band edges) are masked once in the `fft` stage and carried in the row as `masked`, which the page's coarse layer, the `detect` stage and an agent read as no data; `measure` on a local hardware receiver ignores the DC spike.

**Architecture:** `scan_grid(span, caps)` in `zeta-core::ops` yields the even grid (today's `scan_steps`) and the odd grid (shifted by `w/2`, one more step); `scan_loop` cycles them by sweep parity. The `fft` stage gains two optional params, `notch_hz` and `edge_mask`, computes the masked bin ranges per row, excludes them from `floor_db` and emits `masked: [[lo, hi], ...]` when non-empty. `JobKind::Scan` expands to a one-node `fft` graph whose two mask params `launch_scan` writes from `port.capabilities()` on the first launch (`remote == false && push == false` gets the defaults) and reads on every launch, so `graph put` adjusts the mask; `job_context.scan` reports `alternate` and `mask`. `detect` skips masked bins; `measure` gets the capture centre through `StageContext.dc_hz` and masks `MEASURE_DC_MASK_HZ` around it. The page's `fillColumn` skips masked bins (the pixel keeps what the previous row left) and redraws replay the last two sweeps counted by steps.

**Tech Stack:** Rust 2024 (`zeta-core`, `zeta-dsp`, the `zeta` crate's integration tests), serde/serde_json/schemars, the existing `ScanPort`/`FakeFft` harness in `crates/zeta-core/tests/scan.rs`, `FileSource` fixtures (`zeta-dsp::fixtures`, `zeta-source-file` `test-fixtures`), browser JS (`crates/zeta-ui/web`), Playwright e2e in `crates/zeta-ui/tests/e2e/ui.py`.

**Spec:** `docs/superpowers/specs/2026-10-31-scan-grid-mask-design.md` (read it first; section numbers below refer to it).

## Global Constraints

- Branch `plan-15` from `main` 8053f43, in the main checkout (NOT a worktree: the worktree Bash hook refuses the word "source"). One commit per task.
- This desktop has 14 GB RAM and runs another plan's build concurrently: every cargo command is run as `flock /Projects/ZetaSDR/target/.cargo.lock env CARGO_BUILD_JOBS=2 cargo <...>` and tests add `-- --test-threads=2`. Never `cargo clean`. If a build looks stale (shared `CARGO_TARGET_DIR`), `touch crates/*/src/lib.rs`.
- The spec's constants: `SCAN_NOTCH_HZ = 300_000`, `SCAN_EDGE_MASK = 0.10`, `MEASURE_DC_MASK_HZ = 25_000`; the `fft` params `notch_hz` (integer, default 0) and `edge_mask` (number, default 0.0, range 0..0.5, a value outside refused naming the field); the row field `masked: [[lo, hi], ...]` (lo inclusive, hi exclusive, sorted, non-overlapping, absent when empty); the mask default gate `remote == false && push == false`; a step narrower than `4 * notch_hz` gets no notch; `job_context.scan.alternate: {shift_hz, steps_shifted}` (null for a one-step span) and `scan.mask: {notch_hz, edge_mask}`; `job_context.measure.dc_mask_hz`.
- Temp files under `$CLAUDE_JOB_DIR/tmp`, never /tmp. Python via `python3 -I`.
- The UI e2e suite runs near its 300 s deadline: this plan adds exactly one scenario.
- All three constants live in `crates/zeta-core/src/ops.rs` beside `SCAN_STEP_MAX_HZ`; `zeta-dsp` imports `MEASURE_DC_MASK_HZ` from there. No new third-party dependency.
- A row without a masked bin is byte-for-byte today's row (spec §2 anti-goals): `masked` is present only when non-empty, and `q` is computed for every bin, masked ones included. Every existing `fft` node (`spectrum`, `watch`) keeps today's output: both params default to no mask.
- Every task that changes an op result, a stage's params or field craft regenerates `AGENTS.md` (`flock ... cargo run -p zeta -- agents-md > AGENTS.md`) and `skills/zetasdr/SKILL.md` (`flock ... cargo run -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and runs `cargo test -p zeta --test agents_md --test transports`.
- Gates per task: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the touched crates' tests. Final gate: `cargo test --workspace --all-features --no-fail-fast -- --test-threads=2` (no `FAILED`, no `ui e2e skipped`), `agents-md --check`, `agents-md --skill --check`.
- Commit messages in the repo's style (`Scan: ...`, `Fft: ...`, `Detect: ...`, `Measure: ...`, `UI: ...`, `Docs: ...`).

## Review Focus

1. **A notch wider than the row must not blank the row or break the floor.** A `graph put` of `notch_hz: 5_000_000` on a scan with 1 MHz steps gets no notch (`w < 4 * notch_hz`, `scan.mask.notch_hz` reads 0), and the `fft` stage itself, handed a notch covering every bin, masks the whole row and still computes a finite `floor_db` from all bins. Pinned in Task 2 (`masked_ranges_merge_and_clamp`, `quantise_row_masked_with_everything_masked_falls_back`) and Task 3 (`a_one_step_scan_does_not_alternate`).
2. **A row without `masked`, or with a malformed one, still paints and detects.** Older nodes and foreign rows carry no `masked`; a `masked` that is not an array of two-integer pairs is ignored, never a crash. Pinned in Task 4 (`a_malformed_masked_field_is_ignored`) and Task 6 (`decodeRow` leaves `masked` absent: the existing e2e `scenario_waterfall_decodes_quantised_rows` passes unchanged).
3. **A scan persisted before this plan (an empty graph) resumes with the default mask.** The record's graph gains the `fft` node on resume, and a scan whose mask was set with `graph put` keeps it across a restart. Pinned in Task 3 (`a_scan_record_with_an_empty_graph_resumes_with_the_default_mask` in `tests/resume.rs`).
4. **`measure` on a box that lies wholly inside the DC mask still measures.** A 12.5 kHz box around the capture centre on a local device would have every in-band bin masked; the mask is then not applied to that window and the event is today's. Pinned in Task 5 (`measure_band_masked_with_the_mask_over_the_whole_band_measures_as_today`).
5. **A refused `graph put` leaves the running scan untouched.** `edge_mask: 0.6` is refused naming the field before anything restarts: the job stays `Running` with its previous mask and rows keep coming. Pinned in Task 3 (`graph_put_on_a_scan_changes_its_mask`).

---

## File map

- Modify `crates/zeta-core/src/ops.rs` (constants, `local_hardware`, `scan_mask_default`, `scan_notch_in_force`, `ScanGrid`, `scan_grid`, `scan_steps` delegating, `JobKind::Scan` expansion ~L703, unit tests ~L2530-2560), `crates/zeta-core/src/stage.rs:68-100` (`StageContext.dc_hz`, `with_dc_hz`), `crates/zeta-core/src/node/launch.rs` (`launch_inner` ~L173-205: `dc_hz` into the context; `launch_scan` L678-760; `Scan` L1129-1138; `scan_loop` L1147-1180; `fft_step` L1187-1250).
- Modify `crates/zeta-dsp/src/fft.rs` (`FftParams`, `FftParams::check`, `Stage::check_params`, `masked_ranges`, `quantise_row_masked`, row emission), `crates/zeta-dsp/src/detect.rs` (`SpectrumRowView.masked`, `row_masked`, `runs`), `crates/zeta-dsp/src/measure.rs` (`measure_band_masked`, the stage's `dc_hz` and `dc_mask_hz` note), `crates/zeta-dsp/tests/bank.rs:114` (the `StageContext` literal gains `dc_hz: None`).
- Modify tests `crates/zeta-core/tests/scan.rs` (`FakeFft` echoes the mask params, `ScanPort::with_caps`, new tests), `crates/zeta-core/tests/resume.rs` (one test). Create `crates/zeta/tests/measure_dc.rs`.
- Modify `crates/zeta-ui/web/waterfall.js` (`decodeRow`, `fillColumn`, `stepsBack`, `replayCoarse`, `redraw`, `coarseMasked`), `crates/zeta-ui/web/app.js:826-880` (`zetaTest.waterfall()`, `wf().coarseMasked`), `crates/zeta-ui/tests/e2e/ui.py` (one scenario after `scenario_coarse_layer_moves_on_when_the_fine_stream_stops`, ~L1527). `coarse.js` is unchanged (it hands frames through).
- Modify `docs/agents/field-craft.md`, `crates/zeta/src/agents_md.rs`, `README.md:669-680`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (row 15 at L39, a "Plan 15 status" section above "Plan 20 status" at L47), `TODO.md:3-33`, `AGENTS.md`, `skills/zetasdr/SKILL.md`.

---

### Task 1: The alternating grid (`scan_grid`) and the mask constants (Sonnet)

**Files:**
- Modify: `crates/zeta-core/src/ops.rs:960-1000` (beside `SCAN_STEP_MAX_HZ`, `SCAN_DWELL_MS`, `scan_steps`), `:703-707` (`JobKind::Scan` expansion), `:2505-2560` (unit tests: `caps_16m`, `t`, the two `scan_steps_*` tests)
- Test: `crates/zeta-core/src/ops.rs` (`mod tests`)

**Interfaces:**
- Produces, in `zeta_core::ops`:
  - `pub const SCAN_NOTCH_HZ: u64 = 300_000;`, `pub const SCAN_EDGE_MASK: f64 = 0.10;`, `pub const MEASURE_DC_MASK_HZ: u64 = 25_000;`
  - `pub fn local_hardware(caps: &Capabilities) -> bool` (`!caps.remote && !caps.push`: a SoapySDR device; a recording has `push: true`, a Kiwi, a `--source-node` and a VRT sender `remote: true`).
  - `pub fn scan_mask_default(caps: &Capabilities) -> (u64, f64)`: `(SCAN_NOTCH_HZ, SCAN_EDGE_MASK)` when `local_hardware`, else `(0, 0.0)`.
  - `pub fn scan_notch_in_force(step_hz: u64, notch_hz: u64) -> u64`: `0` when `step_hz < 4 * notch_hz`, else `notch_hz`.
  - `#[derive(Clone, Debug, PartialEq)] pub struct ScanGrid { pub even: Vec<Tuning>, pub odd: Vec<Tuning> }` with `pub fn alternates(&self) -> bool` (`self.odd != self.even`), `pub fn steps(&self, sweep: u64) -> &[Tuning]` (`even` for an even `sweep`, `odd` otherwise), `pub fn shift_hz(&self) -> u64` (`even[0].bandwidth_hz.0 / 2`).
  - `pub fn scan_grid(span: Tuning, caps: &Capabilities) -> ScanGrid`: `w`, `n`, `start`, `lo`, `hi` as today's `scan_steps`; `even` = centres `start + w/2 + i*w` for `i in 0..n`; `odd` = centres `start + i*w` for `i in 0..=n` when `n > 1`, else a copy of `even` (a one-step span does not alternate, spec §3); every centre clamped to `lo..=hi`; in both lists a centre equal to the one before it is dropped (the clamp's duplicate), so `odd.len() <= n + 1`. The even list is today's `scan_steps` output except for a span reaching past the tune range by more than `w/2`, where today's grid repeats a step and this one does not (a builder decision under the anti-goal "a slower sweep").
  - `pub fn scan_steps(span: Tuning, caps: &Capabilities) -> Vec<Tuning>` becomes `scan_grid(span, caps).even` (the doc comment moves to `scan_grid`).
  - `JobKind::Scan` expands to one node `{"id": NodeId::new(), "kind": "fft", "params": {"size": spectrum_size(tuning.bandwidth_hz.0.min(SCAN_STEP_MAX_HZ)), "window": "hann", "rows_per_s": SPECTRUM_ROWS_PER_S}}`, no edges, and **no** `notch_hz`/`edge_mask` keys (their absence is how `launch_scan` knows the first launch, Task 3). `JobKind::Identify` keeps the empty graph. Update the `JobKind` doc comment (ops.rs ~L331: "it expands to no graph" → one `fft` node whose mask params the node fills in).

- [ ] **Step 1: Write the failing tests** in `ops.rs` `mod tests`, next to `scan_steps_stay_inside_the_tune_range` (keep `caps_16m()` and `t()`; add `fn caps_min(tune_min_hz: u64) -> Capabilities` = `caps_16m` with that floor, and `fn caps_flags(remote: bool, push: bool) -> Capabilities`):

```rust
#[test]
fn scan_grid_odd_sweep_shifts_by_half_a_step_with_one_more_step() {
    let g = scan_grid(t(20_000_000, 40_000_000), &caps_16m());
    assert_eq!(g.even, scan_steps(t(20_000_000, 40_000_000), &caps_16m()));
    assert_eq!(g.even, [t(9_000_000, 16_000_000), t(24_000_000, 16_000_000), t(40_000_000, 16_000_000)]);
    // 0, 16, 32, 48 MHz; the first clamped up to the 1 MHz floor + w/2.
    assert_eq!(g.odd, [t(9_000_000, 16_000_000), t(16_000_000, 16_000_000), t(32_000_000, 16_000_000), t(48_000_000, 16_000_000)]);
    assert_eq!(g.shift_hz(), 8_000_000);
    assert!(g.alternates());
    assert_eq!(g.steps(0), &g.even[..]);
    assert_eq!(g.steps(1), &g.odd[..]);
    assert_eq!(g.steps(2), &g.even[..]);
}
#[test]
fn scan_grid_drops_a_clamped_duplicate_at_the_tune_floor() {
    // Tune floor 20 MHz: lo = 28 MHz. Odd 0, 16, 32, 48 → 28, 28, 32, 48; even 8, 24, 40 → 28, 28, 40.
    let g = scan_grid(t(20_000_000, 40_000_000), &caps_min(20_000_000));
    assert_eq!(g.odd, [t(28_000_000, 16_000_000), t(32_000_000, 16_000_000), t(48_000_000, 16_000_000)]);
    assert_eq!(g.even, [t(28_000_000, 16_000_000), t(40_000_000, 16_000_000)]);
}
#[test]
fn scan_grid_of_one_step_does_not_alternate() {
    let g = scan_grid(t(100_000_000, 1_000_000), &caps_16m());
    assert_eq!(g.even, [t(100_000_000, 1_000_000)]);
    assert_eq!(g.odd, g.even);
    assert!(!g.alternates());
}
#[test]
fn scan_mask_default_follows_the_capabilities() {
    assert!(local_hardware(&caps_flags(false, false)));
    assert!(!local_hardware(&caps_flags(true, false)));
    assert!(!local_hardware(&caps_flags(false, true)));
    assert_eq!(scan_mask_default(&caps_flags(false, false)), (300_000, 0.10));
    assert_eq!(scan_mask_default(&caps_flags(true, false)), (0, 0.0));
    assert_eq!(scan_mask_default(&caps_flags(false, true)), (0, 0.0));
    assert_eq!(scan_notch_in_force(16_000_000, 300_000), 300_000);
    assert_eq!(scan_notch_in_force(1_200_000, 300_000), 300_000);
    assert_eq!(scan_notch_in_force(1_000_000, 300_000), 0);
    assert_eq!(scan_notch_in_force(48_000, 12_000), 12_000);
}
#[test]
fn scan_expands_to_one_fft_node_without_mask_params() {
    // The embedded rows with no stages: Scan and Identify need no row.
    let modes = ModeRegistry::load(&crate::stage::StageRegistry::new(), None);
    let g = JobKind::Scan.expand(&SourceId("s".into()), t(20_000_000, 40_000_000), Hz(0), None, None, None, &modes).unwrap();
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(g.nodes[0].kind, "fft");
    assert_eq!(g.nodes[0].params, serde_json::json!({"size": 4096, "window": "hann", "rows_per_s": 20}));
    assert!(g.edges.is_empty());
    let id = JobKind::Identify.expand(&SourceId("s".into()), t(100_000_000, 12_500), Hz(0), None, None, None, &modes).unwrap();
    assert!(id.nodes.is_empty());
}
```
  The two existing tests `a_sub_band_scan_is_one_step_as_wide_as_the_span` and `scan_steps_stay_inside_the_tune_range` stay as they are and must pass.

- [ ] **Step 2: Run to verify they fail**: `flock /Projects/ZetaSDR/target/.cargo.lock env CARGO_BUILD_JOBS=2 cargo test -p zeta-core --lib scan_ -- --test-threads=2` → compile errors (`scan_grid`, `ScanGrid`, `local_hardware` undefined).
- [ ] **Step 3: Implement** the constants, the four functions, `ScanGrid`, `scan_grid`, `scan_steps` as a delegate, and the `Scan` arm of `JobKind::expand` (split the `Scan | Identify` arm).
- [ ] **Step 4: Run**: `cargo test -p zeta-core --lib` (all unit tests), `cargo test -p zeta-core --test scan --test resume` (the scan harness still runs today's even grid: `scan_loop` is untouched until Task 3; `launch_scan` ignores the graph today, so the one-node expansion changes nothing yet), clippy, fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "Scan: scan_grid gives the even grid and the half-step odd grid (one more step, clamped duplicates dropped, none for a one-step span); the mask constants and the local-hardware gate; a scan expands to one fft node"`.

---

### Task 2: The `fft` stage masks bins and says so (Opus)

**Files:**
- Modify: `crates/zeta-dsp/src/fft.rs:166-200` (`FftParams`, `rows_per_s_default`), `:208-226` (`quantise_row`), `:395-480` (`FftStage::run`, the row payload), tests at the end of the file
- Test: `crates/zeta-dsp/src/fft.rs` (`mod tests`); `crates/zeta-dsp/tests/fft_rows.rs` must pass unchanged

**Interfaces:**
- `FftParams` gains `#[serde(default)] pub notch_hz: i64` (schema: integer, default 0; `i64` so a negative value reaches `check` and is refused naming the field rather than by the schema's anonymous `minimum` message) and `#[serde(default)] pub edge_mask: f64` (schema: number, default 0.0). Both optional in the schema (`required` lists neither).
- `impl FftParams { pub fn check(&self) -> Result<(), String> }`: `Err("notch_hz must be at least 0")` for a negative notch, `Err("edge_mask must be in 0..0.5")` outside `0.0..0.5` (0.5 excluded, as `DetectParams::check`), the range check also catching NaN; `Ok` otherwise.
- `impl Stage for FftStage { fn check_params(&self, params: &serde_json::Value) -> Result<(), String> }`: deserialise then `check` (as `DetectStage::check_params`). `run` also calls `check` after parsing (`fft params: <msg>`).
- `pub fn masked_ranges(size: usize, bin_hz: f64, notch_hz: u64, edge_mask: f64) -> Vec<[usize; 2]>`: the notch masks bins `k` with `|(k - size/2) * bin_hz| <= notch_hz / 2` (bin `size/2` is the DC bin, so with `m = floor((notch_hz / 2) / bin_hz)` the range is `[size/2 - m, size/2 + m + 1)`, `2m + 1` bins: the builder's convention for spec §8); `notch_hz == 0` masks nothing. `e = floor(size * edge_mask)` masks `[0, e)` and `[size - e, size)`; `e == 0` masks nothing. Ranges are clamped into `0..size`, sorted by `lo`, and overlapping or touching ranges merged, so the result is sorted and non-overlapping; `[0, size]` when everything is masked.
- `pub fn quantise_row_masked(db: &[f32], masked: &[[usize; 2]]) -> (f32, Vec<u8>)`: `floor_db` is the 2nd percentile minus 5 dB of the bins **outside** `masked` (all bins when `masked` covers every bin); `q` is computed for every bin against that floor. `pub fn quantise_row(db) -> (f32, Vec<u8>)` stays and equals `quantise_row_masked(db, &[])`.
- The row payload gains `"masked": <ranges>` only when the ranges are non-empty; `masked_ranges` is evaluated once per `(rate, size)` (the cutter's restart) and reused per row.

- [ ] **Step 1: Write the failing tests** in `fft.rs` `mod tests`:

```rust
#[test]
fn masked_ranges_of_a_notch_are_symmetric_about_the_dc_bin() {
    // 48 kHz over 1024 bins: 46.875 Hz each; 1875 Hz is 40 bins, so m = 20: 41 bins.
    assert_eq!(masked_ranges(1024, 46.875, 1875, 0.0), vec![[492, 533]]);
    assert_eq!(masked_ranges(1024, 46.875, 0, 0.0), Vec::<[usize; 2]>::new());
}
#[test]
fn masked_ranges_of_an_edge_mask() {
    assert_eq!(masked_ranges(1024, 46.875, 0, 0.1), vec![[0, 102], [922, 1024]]);
    assert_eq!(masked_ranges(1024, 46.875, 1875, 0.1), vec![[0, 102], [492, 533], [922, 1024]]);
}
#[test]
fn masked_ranges_merge_and_clamp() {
    // 64 bins of 750 Hz; a 60 kHz notch is m = 40: [-8, 73) clamped to the row and merged with the edges.
    assert_eq!(masked_ranges(64, 750.0, 60_000, 0.1), vec![[0, 64]]);
    // A notch touching an edge range merges with it: m = 28 → [4, 61) plus [0, 6) and [58, 64).
    assert_eq!(masked_ranges(64, 750.0, 42_000, 0.1), vec![[0, 64]]);
}
#[test]
fn quantise_row_masked_takes_the_floor_from_unmasked_bins() {
    let db: Vec<f32> = (0..100).map(|i| -100.0 + 2.0 * i as f32).collect();
    // The lower half masked (a hump that would otherwise set the floor): the floor is the
    // 2nd percentile of 0, 2, ..., 98 (index 0 → 0 dB) minus 5.
    let (floor, q) = quantise_row_masked(&db, &[[0, 50]]);
    assert_eq!(floor, -5.0);
    assert_eq!(q.len(), 100);
    assert_eq!(q[0], 0); // -100 dB, masked, still quantised (clamped at 0)
    assert_eq!(q[50], 10);
    assert_eq!(q[99], 206);
    assert_eq!(quantise_row_masked(&db, &[]), quantise_row(&db));
}
#[test]
fn quantise_row_masked_with_everything_masked_falls_back() {
    let db: Vec<f32> = (0..100).map(|i| -100.0 + 2.0 * i as f32).collect();
    assert_eq!(quantise_row_masked(&db, &[[0, 100]]), quantise_row(&db)); // floor -103
}
#[test]
fn fft_params_check_names_the_field() {
    let p = |notch_hz: i64, edge_mask: f64| FftParams { size: 1024, window: Window::Hann, rows_per_s: 20, notch_hz, edge_mask };
    assert_eq!(p(0, 0.0).check(), Ok(()));
    assert_eq!(p(300_000, 0.49).check(), Ok(()));
    assert_eq!(p(-1, 0.0).check(), Err("notch_hz must be at least 0".into()));
    assert_eq!(p(0, 0.5).check(), Err("edge_mask must be in 0..0.5".into()));
    assert_eq!(p(0, -0.1).check(), Err("edge_mask must be in 0..0.5".into()));
    let stage = FftStage::new(Arc::new(CpuFft::new()));
    let e = stage.check_params(&serde_json::json!({"size": 1024, "window": "hann", "edge_mask": 0.6})).unwrap_err();
    assert!(e.contains("edge_mask"), "{e}");
    assert_eq!(stage.check_params(&serde_json::json!({"size": 1024, "window": "hann"})), Ok(()));
}
#[test]
fn fft_params_schema_lists_the_mask_params_with_defaults() {
    let s = serde_json::to_value(schemars::schema_for!(FftParams)).unwrap();
    assert_eq!(s["properties"]["notch_hz"]["default"], 0);
    assert_eq!(s["properties"]["edge_mask"]["default"], 0.0);
    let required: Vec<&str> = s["required"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert!(!required.contains(&"notch_hz") && !required.contains(&"edge_mask"), "{required:?}");
    // A graph with a bad edge_mask is refused by validate naming the field (as detect's is).
    let mut r = StageRegistry::new();
    r.register(Arc::new(FftStage::new(Arc::new(CpuFft::new()))));
    let g = Graph { id: GraphId::new(), nodes: vec![Node { id: NodeId("a".into()), kind: "fft".into(), params: serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20, "edge_mask": 0.6}) }], edges: vec![] };
    match validate(&g, &r) { Err(GraphError::BadParams { reason, .. }) => assert!(reason.contains("edge_mask"), "{reason}"), other => panic!("{other:?}") }
}
#[tokio::test]
async fn fft_stage_rows_carry_masked_only_when_a_bin_is_masked() {
    // As fft_stage_emits_v2_rows (a 4800-sample 6 kHz tone block at 48 kHz, one 1024-bin row),
    // run twice: with {"notch_hz": 1875, "edge_mask": 0.1} the row has
    // ev["masked"] == json!([[0, 102], [492, 533], [922, 1024]]), q.len() == 1024 and a finite
    // floor_db; with the params left out, ev.get("masked").is_none() and the row equals
    // fft_stage_emits_v2_rows's (size, bin_hz, rows_per_s, step_db, center_hz).
}
```
  Pull the block-and-bus setup of `fft_stage_emits_v2_rows` into a helper `async fn one_row(params: serde_json::Value) -> serde_json::Value` both tests use.

- [ ] **Step 2: Run to verify they fail**: `flock ... cargo test -p zeta-dsp --lib fft:: -- --test-threads=2` → compile errors.
- [ ] **Step 3: Implement** the two params, `check`, `check_params`, `masked_ranges`, `quantise_row_masked`, the payload field. In `run`, keep `masked` beside the cutter: recompute when `row.rate_hz` differs from the last row's (the cutter restarts on a rate or centre change anyway).
- [ ] **Step 4: Run**: `cargo test -p zeta-dsp` (unit and `tests/fft_rows.rs`, `tests/detect.rs`, `tests/gpu_fft_software.rs` unchanged), `cargo test -p zeta-core --test ops` (`nested_schemas_are_strict` sees the core registry only; still run it), clippy, fmt → green. Regenerate `AGENTS.md` and the skill (the `fft` stage kind now lists `params.notch_hz: integer (optional, default 0)` and `params.edge_mask: number (optional, default 0.0)`); `cargo test -p zeta --test agents_md --test transports` → green.
- [ ] **Step 5: Commit**: `git commit -m "Fft: notch_hz and edge_mask params (default none, refused naming the field), rows carry masked: [[lo, hi], ...] when a bin is masked and the floor excludes masked bins"`.

---

### Task 3: The scan alternates, carries its mask from the capabilities, and takes a `graph put` (Opus, higher effort)

**Files:**
- Modify: `crates/zeta-core/src/node/launch.rs:671-760` (`launch_scan`), `:1127-1138` (`Scan`), `:1140-1180` (`scan_loop`), `:1187-1250` (`fft_step`)
- Modify: `crates/zeta-core/tests/scan.rs` (`ScanPort`, `FakeFft`, `scan_steps_the_port_across_the_span_and_emits_rows`, new tests), `crates/zeta-core/tests/resume.rs` (one test)
- Test: `crates/zeta-core/tests/scan.rs`, `crates/zeta-core/tests/resume.rs`

**Interfaces:**
- Consumes Task 1's `scan_grid`, `ScanGrid`, `scan_mask_default`, `scan_notch_in_force`, `SCAN_STEP_MAX_HZ`, `spectrum_size`, `SPECTRUM_ROWS_PER_S`, and `graph::validate`.
- `struct Scan` replaces `steps: Vec<Tuning>` with `grid: ScanGrid` and gains `notch_hz: u64` (the notch **in force**) and `edge_mask: f64`.
- `launch_scan(self, rec, port, gate, after)` (signature unchanged), before taking the lease: (1) if `rec.graph` has no node of kind `fft` (a record persisted before this plan: an empty graph), set `rec.graph` to what `JobKind::Scan.expand(&rec.source, rec.tuning, Hz(0), None, None, None, &self.modes)` gives; (2) `graph::validate(&rec.graph, &self.registry)?` (a bad stored mask fails the launch here, before the lease); (3) `let grid = scan_grid(rec.tuning, port.capabilities()); let w = grid.even[0].bandwidth_hz.0;`; (4) on the first `fft` node, when its params lack `notch_hz`: insert `notch_hz` and `edge_mask` from `scan_mask_default(port.capabilities())` and set `size` to `spectrum_size(w)` (the first launch decides the mask; a resume or a `graph put` finds the keys present and keeps them); (5) read `notch_cfg = params["notch_hz"]` (u64, non-negative after `validate`) and `edge_mask = params["edge_mask"]` (f64), `notch = scan_notch_in_force(w, notch_cfg)`. Then the lease as today, and the context `scan` becomes `{"steps": grid.even.len(), "step_hz": w, "dwell_ms": self.scan_dwell_ms, "alternate": <{"shift_hz": grid.shift_hz(), "steps_shifted": grid.odd.len()} when grid.alternates(), else null>, "mask": {"notch_hz": notch, "edge_mask": edge_mask}}`. `record_running` persists the record, the rewritten graph with it. Only the first `fft` node is read; `size`, `window` and `rows_per_s` of the stored node are information only (`fft_step` derives them per step as today).
- `scan_loop`: holds `sweep: u64` and `i: usize`; `let steps = s.grid.steps(sweep);` before each tune; after a step `i += 1; if i == steps.len() { i = 0; sweep += 1; }`. The row channel stays `scan/<i>`, `i` the index within the current sweep's list.
- `fft_step(inner, s, step, center, block, size)` (signature unchanged) adds `"notch_hz": s.notch_hz, "edge_mask": s.edge_mask` to the `fft` params it builds.
- In `tests/scan.rs`: `FakeFft`'s event gains `"notch_hz": params["notch_hz"].clone(), "edge_mask": params["edge_mask"].clone()`; `ScanPort::with_caps(remote: bool, push: bool) -> Arc<Self>` (`new()` = `with_caps(false, false)`; `with_fail_tune` keeps its shape by sharing one private constructor).

- [ ] **Step 1: Write the failing tests** in `tests/scan.rs` (helpers: `async fn tunes_at_least(port: &ScanPort, n: usize, within: Duration)` waiting for `port.tunes().len() >= n`; `fn fft_node(g: &Graph) -> &GraphNode`; `async fn graph_of(node: &Node, id: &JobId) -> Graph` via `Op::GraphGet`; `fn event_rx(node: &Node, id: &JobId) -> broadcast::Receiver<Frame>`, the event-stream lookup pulled out of `start_scan`, which then calls it):

```rust
#[tokio::test(flavor = "multi_thread")]
async fn scan_alternates_its_grid_between_sweeps() {
    // 0..40 MHz on radio0: even 9, 24, 40 MHz; odd 9, 16, 32, 48 MHz (0 clamped up to 9).
    let (_dir, node, port) = open().await;
    let (id, mut rx) = start_scan(&node).await;
    tunes_at_least(&port, 10, Duration::from_secs(5)).await;
    let want = [9, 24, 40, 9, 16, 32, 48, 9, 24, 40].map(|m| m * MHZ);
    let got: Vec<u64> = port.tunes().iter().take(10).map(|t| t.center_hz.0).collect();
    assert_eq!(got, want);
    assert!(port.tunes().iter().all(|t| t.bandwidth_hz.0 == 16 * MHZ));
    // Each row's channel is its index within its own sweep's list.
    let mut seen = std::collections::HashMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Ok(f)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        let Payload::Event(ev) = &f.payload else { panic!("events") };
        let c = ev["center_hz"].as_u64().unwrap();
        let ch = f.provenance.channel.as_ref().unwrap().0.clone();
        seen.insert(c, ch);
    }
    for (c, ch) in [(9, "scan/0"), (24, "scan/1"), (40, "scan/2"), (16, "scan/1"), (32, "scan/2"), (48, "scan/3")] {
        assert_eq!(seen.get(&(c * MHZ)).map(String::as_str), Some(ch), "{seen:?}");
    }
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["steps"], 3);
    assert_eq!(ctx["scan"]["step_hz"], 16 * MHZ);
    assert_eq!(ctx["scan"]["alternate"], serde_json::json!({"shift_hz": 8 * MHZ, "steps_shifted": 4}));
    node.job_stop(&id).await.unwrap();
}
#[tokio::test(flavor = "multi_thread")]
async fn scan_mask_defaults_follow_the_capabilities() {
    // radio0 is local hardware (remote false, push false): the defaults are in force on a 16 MHz step.
    let (_dir, node, _port) = open().await;
    let (id, mut rx) = start_scan(&node).await;
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["mask"], serde_json::json!({"notch_hz": 300_000, "edge_mask": 0.1}));
    let Payload::Event(ev) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap().payload else { panic!() };
    assert_eq!(ev["notch_hz"], 300_000);
    assert_eq!(ev["edge_mask"], 0.1);
    let g = graph_of(&node, &id).await;
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(fft_node(&g).params["notch_hz"], 300_000);
    assert_eq!(fft_node(&g).params["edge_mask"], 0.1);
    assert_eq!(fft_node(&g).params["size"], 4096);
    node.job_stop(&id).await.unwrap();
    // A remote source gets none.
    let (_dir, node, _port) = open_with(ScanPort::with_caps(true, false)).await;
    let (id, mut rx) = start_scan(&node).await;
    assert_eq!(node.job_context(&id).unwrap()["scan"]["mask"], serde_json::json!({"notch_hz": 0, "edge_mask": 0.0}));
    let Payload::Event(ev) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap().payload else { panic!() };
    assert_eq!(ev["notch_hz"], 0);
    assert_eq!(ev["edge_mask"], 0.0);
    node.job_stop(&id).await.unwrap();
}
#[tokio::test(flavor = "multi_thread")]
async fn a_one_step_scan_does_not_alternate() {
    // 1 MHz is narrower than four 300 kHz notches: no notch in force, though the node stores it.
    let (_dir, node, port) = open().await;
    let id = match node.apply(start(JobKind::Scan, 100 * MHZ, MHZ)).await { Ok(OpResult::Job(id)) => id, o => panic!("{o:?}") };
    tunes_at_least(&port, 3, Duration::from_secs(3)).await;
    assert!(port.tunes().iter().all(|t| *t == Tuning { center_hz: Hz(100 * MHZ), bandwidth_hz: Hz(MHZ) }));
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["steps"], 1);
    assert!(ctx["scan"]["alternate"].is_null(), "{ctx}");
    assert_eq!(ctx["scan"]["mask"], serde_json::json!({"notch_hz": 0, "edge_mask": 0.1}));
    assert_eq!(fft_node(&graph_of(&node, &id).await).params["notch_hz"], 300_000);
    node.job_stop(&id).await.unwrap();
}
#[tokio::test(flavor = "multi_thread")]
async fn graph_put_on_a_scan_changes_its_mask() {
    let (_dir, node, _port) = open().await;
    let (id, _rx) = start_scan(&node).await;
    let mut g = graph_of(&node, &id).await;
    g.nodes[0].params["notch_hz"] = serde_json::json!(1_000_000);
    g.nodes[0].params["edge_mask"] = serde_json::json!(0.25);
    assert!(matches!(node.apply(Op::GraphPut { job: id.clone(), graph: g.clone() }).await, Ok(OpResult::Ok)));
    assert_eq!(node.job_list().iter().find(|r| r.id == id).unwrap().state, JobState::Running);
    assert_eq!(node.job_context(&id).unwrap()["scan"]["mask"], serde_json::json!({"notch_hz": 1_000_000, "edge_mask": 0.25}));
    // The restarted scan's rows carry the new params (a fresh event stream: look it up again).
    let mut rx = event_rx(&node, &id);
    let Payload::Event(ev) = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap().payload else { panic!() };
    assert_eq!(ev["notch_hz"], 1_000_000);
    assert_eq!(ev["edge_mask"], 0.25);
    // A bad mask is refused naming the field; the scan runs on with its mask.
    g.nodes[0].params["edge_mask"] = serde_json::json!(0.6);
    match node.apply(Op::GraphPut { job: id.clone(), graph: g }).await {
        Err(OpError::Graph { detail: GraphError::BadParams { reason, .. } }) => assert!(reason.contains("edge_mask"), "{reason}"),
        other => panic!("expected a bad-params refusal, got {other:?}"),
    }
    assert_eq!(node.job_list().iter().find(|r| r.id == id).unwrap().state, JobState::Running);
    assert_eq!(node.job_context(&id).unwrap()["scan"]["mask"]["edge_mask"], 0.25);
    node.job_stop(&id).await.unwrap();
}
// tests/resume.rs (its Radio is local hardware; `running(id, "scan", ..)` writes an empty graph)
#[tokio::test]
async fn a_scan_record_with_an_empty_graph_resumes_with_the_default_mask() {
    let scan = JobId::new();
    let dir = tempfile::tempdir().unwrap();
    write_records(dir.path(), &[running(&scan, "scan", band(40_000_000), 1)]);
    let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()]).await.unwrap();
    assert_eq!(record(&node, &scan).state, JobState::Running);
    let g = &record(&node, &scan).graph;
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(g.nodes[0].kind, "fft");
    assert_eq!(g.nodes[0].params["notch_hz"], 300_000);
    assert_eq!(g.nodes[0].params["edge_mask"], 0.1);
    let ctx = node.job_context(&scan).unwrap();
    assert_eq!(ctx["scan"]["mask"], serde_json::json!({"notch_hz": 300_000, "edge_mask": 0.1}));
    assert!(ctx["scan"]["alternate"].is_object(), "{ctx}");
    node.job_stop(&scan).await.unwrap();
}
```
  Also update `scan_steps_the_port_across_the_span_and_emits_rows`: drop its `tunes[k].center_hz == centres[k % 3]` loop (the alternation test owns the tune order) and let a row's `center_hz` be any of `[9, 16, 24, 32, 40, 48]` MHz, keeping its other assertions (`size == 4096`, `samples >= 4096`, the tone within 1 kHz of `tone_hz(c)`, at least 2 rows per even centre, a dwell read of at least two blocks, `tunes.len() >= 6`, every bandwidth 16 MHz). `stream_list_stays_flat_across_sweeps` and `no_bank_is_made_per_step_while_a_scan_holds_the_source` count to 7 tunes = one even plus one odd sweep: unchanged. `scan_context_carries_its_step_count` unchanged.

- [ ] **Step 2: Run to verify they fail**: `flock ... cargo test -p zeta-core --test scan --test resume -- --test-threads=2` → the new tests fail (`alternate`/`mask` missing, tunes in today's order, `GraphGet` shows the node without `notch_hz`), `FakeFft` compile error until it echoes.
- [ ] **Step 3: Implement** `Scan`, `launch_scan`, `scan_loop`, `fft_step` as the Interfaces say, and the harness changes in `tests/scan.rs`.
- [ ] **Step 4: Run**: `cargo test -p zeta-core --test scan --test resume --test ops --test jobs`, then `cargo test -p zeta-core`, `cargo test -p zeta --test resume --test bank_resume --test cli` (the CLI's scan paths), clippy, fmt → green. Regenerate `AGENTS.md`/skill (no op schema change expected; confirm `agents-md --check` still passes).
- [ ] **Step 5: Commit**: `git commit -m "Scan: sweeps alternate between the even and the half-step odd grid; the scan's fft node carries notch_hz and edge_mask, written from the capabilities on the first launch and honoured on every launch (graph put changes the mask); job_context.scan gains alternate and mask"`.

---

### Task 4: `detect` ignores masked bins (Sonnet)

**Files:**
- Modify: `crates/zeta-dsp/src/detect.rs:100-110` (`SpectrumRowView`), `:126-175` (`runs`), `:408-430` (`row_view`, a new `row_masked`), tests `:468-500` (`view()` helper) and new tests
- Test: `crates/zeta-dsp/src/detect.rs` (`mod tests`); `crates/zeta-dsp/tests/detect.rs` and `crates/zeta-core/tests/watch.rs` must pass unchanged

**Interfaces:**
- `pub struct SpectrumRowView<'a>` gains `pub masked: &'a [[usize; 2]]` (still `Clone, Copy`). The doc says a bin inside a range takes no part in a run.
- `fn row_masked(v: &serde_json::Value) -> Vec<[usize; 2]>`: the payload's `masked` as pairs of `usize`; an absent field, or one that is not an array of two-integer arrays (any element malformed), gives an empty vector (the mask is ignored, never an error).
- `runs(row, p)`: the `masked(k)` closure is also true when some `[lo, hi]` in `row.masked` has `lo <= k < hi`; the stage's `run` builds the view with `masked: &row_masked(v)`. Today's `dc_mask_hz`, `edge_mask` and `spurs_hz` stay (the two compose, spec §5).

- [ ] **Step 1: Write the failing tests** in `detect.rs` `mod tests` (give `view()` a sibling `fn view_masked<'a>(q: &'a [u8], masked: &'a [[usize; 2]]) -> SpectrumRowView<'a>`; `view(q)` passes `&[]`):

```rust
#[test]
fn runs_ignore_bins_inside_a_masked_range() {
    // The 10 kHz bump of runs_find_a_10_khz_bump_and_skip_the_dc_spike, wholly masked.
    let up = 10 + 24;
    let k = bin(1_000_000.0) - 1;
    let q = row(&[(k, 3, up)]);
    assert_eq!(runs(&view(&q), &DetectParams::default()).len(), 1);
    assert!(runs(&view_masked(&q, &[[k, k + 3]]), &DetectParams::default()).is_empty());
}
#[test]
fn a_masked_range_cuts_a_run() {
    // A 6-bin (21 kHz) bump with its first 3 bins masked: one 3-bin run on the unmasked half.
    let up = 10 + 24;
    let k = bin(1_000_000.0) - 1;
    let q = row(&[(k, 6, up)]);
    let found = runs(&view_masked(&q, &[[k, k + 3]]), &DetectParams::default());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].bandwidth_hz, (3.0 * BIN_HZ) as u64);
    let centre = CENTER_HZ as f64 + (k as f64 + 4.0 - (SIZE / 2) as f64) * BIN_HZ;
    assert!((found[0].center_hz as f64 - centre).abs() <= BIN_HZ, "{found:?}");
}
#[test]
fn a_malformed_masked_field_is_ignored() {
    let ok = serde_json::json!({"masked": [[1, 2], [5, 9]]});
    assert_eq!(row_masked(&ok), vec![[1, 2], [5, 9]]);
    for bad in [serde_json::json!({}), serde_json::json!({"masked": null}), serde_json::json!({"masked": [[1]]}), serde_json::json!({"masked": [[1, "x"]]}), serde_json::json!({"masked": "no"})] {
        assert!(row_masked(&bad).is_empty(), "{bad}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**: `flock ... cargo test -p zeta-dsp --lib detect:: -- --test-threads=2` → compile errors (`masked` field, `view_masked`, `row_masked`).
- [ ] **Step 3: Implement** the field, `row_masked`, the `runs` closure, the stage's view.
- [ ] **Step 4: Run**: `cargo test -p zeta-dsp` (unit, `tests/detect.rs`), `cargo test -p zeta-core --test watch` (the watch harness feeds rows without `masked`), clippy, fmt → green.
- [ ] **Step 5: Commit**: `git commit -m "Detect: bins inside a row's masked ranges take no part in a run (a malformed masked field is ignored); SpectrumRowView carries the ranges"`.

---

### Task 5: `measure` masks the DC spike on a local hardware receiver (Opus)

**Files:**
- Modify: `crates/zeta-core/src/stage.rs:68-100` (`StageContext.dc_hz`, `with_dc_hz`), `crates/zeta-core/src/node/launch.rs:173-205` (`launch_inner`: `dc_hz` into the context), `crates/zeta-dsp/src/measure.rs:356-400` (`measure_band_masked`), `:770-895` (the stage's `run`, `window_event`), tests `:1180-1320` (`run_stage`, the two note assertions), `crates/zeta-dsp/tests/bank.rs:114` (`dc_hz: None` in the `StageContext` literal)
- Create: `crates/zeta/tests/measure_dc.rs`
- Test: `crates/zeta-dsp/src/measure.rs` (`mod tests`), `crates/zeta/tests/measure_dc.rs`

**Interfaces:**
- Consumes Task 1's `local_hardware` and `MEASURE_DC_MASK_HZ`.
- `StageContext` gains `pub dc_hz: Option<u64>`: the absolute frequency of the receiver's DC (the capture's centre), `Some` only for a job on a local hardware source; `StageContext::new` sets `None`; `pub fn with_dc_hz(self, hz: Option<u64>) -> Self`. `launch_inner` sets it from the capture the lease left the port at: `capture.filter(|_| local_hardware(port.capabilities())).map(|b| b.center_hz.0)` (`None` for an attached job, which has no port). The `StageContext { bank, mode, ..StageContext::new(..) }` literal gains `dc_hz`.
- `pub fn measure_band_masked(iq: &[Complex<f32>], rate_hz: u64, block_center_hz: u64, bandwidth_hz: u64, dc: Option<(u64, u64)>) -> FeatureRecord`: `dc = Some((dc_hz, half_width_hz))` masks the in-band bins whose centre lies within `half_width_hz` of `dc_hz - block_center_hz` (the DC's offset in the block). The floor is the 15th percentile of the unmasked in-band bins; masked bins are then set to that floor before the peak and the occupied run are taken (so they are neither the peak nor occupied). When the mask covers every in-band bin, or `dc` lies outside the band, it is not applied and the result is `measure_band`'s. `pub fn measure_band(..)` stays and equals `measure_band_masked(.., None)`. `measure_selection` (classify) is unchanged.
- The stage: `let dc = ctx.dc_hz.map(|hz| (hz, MEASURE_DC_MASK_HZ)); let dc_mask_hz = dc.map_or(0, |d| d.1);` `window_event` gains `dc: Option<(u64, u64)>` and calls `measure_band_masked`. The context note becomes `{"measure": {"discarded", "partial", "last", "dc_mask_hz"}}`: once when the stage starts (`discarded: 0, partial: 0, last: null`) and at every cut as today, `dc_mask_hz` on each (the merge is shallow per top-level key, so every note carries the whole object).

- [ ] **Step 1: Write the failing tests**. In `measure.rs` `mod tests` (constants local to the tests: `const WIDE_RATE: u64 = 192_000;`):

```rust
/// A tone 40 kHz above centre at 0.1 under a DC spike (a constant 0.5) and 20 dB noise.
fn spike_and_tone(n: usize) -> Vec<Complex<f32>> { /* nco(n, 40_000.0, 192_000.0) scaled 0.1, plus Complex::new(0.5, 0.0), awgn 20 dB seed 7 */ }
#[test]
fn measure_band_masked_ignores_the_dc_spike() {
    let iq = spike_and_tone(WIDE_RATE as usize);
    let bin = WIDE_RATE as f64 / SIZE as f64; // 187.5 Hz
    let plain = measure_band(&iq, WIDE_RATE, CENTER, WIDE_RATE);
    assert!((plain.center_hz.0 as f64 - CENTER as f64).abs() <= bin, "{plain:?}");
    let masked = measure_band_masked(&iq, WIDE_RATE, CENTER, WIDE_RATE, Some((CENTER, 25_000)));
    assert!((masked.center_hz.0 as f64 - (CENTER + 40_000) as f64).abs() <= 2.0 * bin, "{masked:?}");
    assert!(masked.snr_db > 10.0, "{masked:?}");
    assert_eq!(measure_band_masked(&iq, WIDE_RATE, CENTER, WIDE_RATE, None), plain);
}
#[test]
fn measure_band_masked_with_the_mask_over_the_whole_band_measures_as_today() {
    // A 12.5 kHz band around the DC: every in-band bin is within 25 kHz of it.
    let iq = spike_and_tone(WIDE_RATE as usize);
    let plain = measure_band(&iq, WIDE_RATE, CENTER, 12_500);
    assert_eq!(measure_band_masked(&iq, WIDE_RATE, CENTER, 12_500, Some((CENTER, 25_000))), plain);
    // A DC outside the band: not applied either.
    assert_eq!(measure_band_masked(&iq, WIDE_RATE, CENTER, WIDE_RATE, Some((CENTER + 500_000, 25_000))), measure_band(&iq, WIDE_RATE, CENTER, WIDE_RATE));
}
#[tokio::test]
async fn measure_stage_masks_the_dc_only_with_a_dc_hz_in_its_context() {
    // run_stage gains a `dc_hz: Option<u64>` argument it passes through `with_dc_hz`; blocks of
    // spike_and_tone at WIDE_RATE (iq_frame with sample_rate_hz WIDE_RATE), one full second.
    // With Some(CENTER): the event's center_hz within 2 bins of CENTER + 40_000 and
    // notes[0] == json!({"measure": {"discarded": 0, "partial": 0, "last": null, "dc_mask_hz": 25_000}}).
    // With None: center_hz within a bin of CENTER and notes[0]["measure"]["dc_mask_hz"] == 0.
}
```
  The two existing assertions on `notes.last()` (L1290-1292 `{"discarded": 0, "partial": 1, "last": "gap"}` and L1312-1314 `{"discarded": 1, "partial": 0, "last": "lag"}`) gain `"dc_mask_hz": 0`.

  In `crates/zeta/tests/measure_dc.rs` (modelled on `measure_bank.rs`: its `registry()`, `cfg(dir, None)`, `start_measure`, `first_event`; the fixture 2 s of `spike_and_tone` at 192 kHz written with `zeta_dsp::fixtures::write_fixture(dir, "dc", &iq, 192_000, 27_000_000)` and played by a looping real-time `FileSource`; a wrapper `struct LocalCaps { inner: Arc<FileSource>, caps: Capabilities }` implementing `Port` by delegation of `id`, `tune`, `current`, `subscribe` with `capabilities()` returning the recording's record with `push: false`):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_measure_on_a_local_device_ignores_the_dc_spike() {
    // LocalCaps port (remote false, push false); measure at 27 MHz / 192 kHz.
    // first_event within 4 s: ev["center_hz"] within 400 Hz of 27_040_000 (the label is not
    // asserted: the DC offset wobbles the envelope and phase, so layer 1 may say fm or carrier);
    // node.job_context(&id)["measure"]["dc_mask_hz"] == 25_000.
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_measure_on_a_recording_measures_the_spike() {
    // The plain FileSource (push true): center_hz within 400 Hz of 27_000_000; dc_mask_hz == 0.
}
```

- [ ] **Step 2: Run to verify they fail**: `flock ... cargo test -p zeta-dsp --lib measure:: -- --test-threads=2` and `cargo test -p zeta --test measure_dc` → compile errors (`measure_band_masked`, `dc_hz`).
- [ ] **Step 3: Implement** `StageContext.dc_hz`/`with_dc_hz` (fix `tests/bank.rs:114`), the `launch_inner` gate, `measure_band_masked`, the stage's `dc` and notes.
- [ ] **Step 4: Run**: `cargo test -p zeta-core` (the `StageContext` change), `cargo test -p zeta-dsp`, `cargo test -p zeta --test measure_dc --test measure_bank`, `cargo test -p zeta-plugin` (its tests build a `StageContext` through `new`), clippy, fmt → green. Regenerate `AGENTS.md`/skill: the `measure` paragraph of field craft is Task 7's; confirm `--check` passes (no schema change here).
- [ ] **Step 5: Commit**: `git commit -m "Measure: on a local hardware receiver the bins within MEASURE_DC_MASK_HZ of the capture centre take no part in the floor, the peak or the occupied band; StageContext.dc_hz; job_context.measure.dc_mask_hz"`.

---

### Task 6: The page paints nothing under a mask and replays two sweeps (Opus, higher effort)

**Files:**
- Modify: `crates/zeta-ui/web/waterfall.js:259-272` (`decodeRow`), `:351-355` (`sweepCount`; a new `stepsBack` beside it), `:382-384` (constructor: `coarseMasked`), `:1249-1262` (`addCoarseEvent`), `:1289-1297` (`replayCoarse`), `:1338-1395` (`fillColumn`), `:1407-1453` (`redraw`, `sweepRows`), the header comment `:51-55`
- Modify: `crates/zeta-ui/web/app.js:854-877` (`zetaTest.wf()` gains `coarseMasked`; new `zetaTest.waterfall()`)
- Modify: `crates/zeta-ui/tests/e2e/ui.py` (one scenario after `scenario_coarse_layer_moves_on_when_the_fine_stream_stops`, ~L1527)
- Test: `cargo test -p zeta --test ui` (the e2e suite; `ZETA_UI_ONLY=coarse_layer_leaves_masked_bins_unpainted,coarse_layer_paints_outside_the_capture,waterfall_decodes_quantised_rows` for the inner loop)

**Interfaces:**
- `decodeRow(p)`: when `p.masked` is an array, the row gains `masked`, the pairs `[lo, hi]` that are arrays of two numbers, and only when at least one survives; a payload without `masked` gives today's object exactly (`scenario_waterfall_decodes_quantised_rows` compares whole objects).
- `fillColumn(row, fill, buf)`: a pixel row whose covered bins are all masked is left as it is (whatever `fill`); a pixel row covering several bins takes the strongest **unmasked** one. The mask is a per-row `Uint8Array(size)` built lazily as `row.maskBin` (like `row.lut`).
- `function stepsBack(rowAt, end, steps, floor = 0) -> number`: walks `i` from `end - 1` down to `floor` over rows `rowAt(i)` (an object with `lo`/`hi`, or anything else, skipped), counting a step each time the centre `(lo + hi) / 2` differs from the previous row's; returns the first index whose inclusion keeps the count at most `steps` (`end` when `steps` is 0, `floor` when the rows run out).
- `replayCoarse(end)`: `from = stepsBack((i) => this.coarseRows[i].row, end, 2 * sweepCount(newest, this.coarseSpan))` replaces today's `end - sweepCount(..)` (two sweeps by steps, spec §5).
- `redraw()` in scan mode: the pre-replay of stored rows older than the oldest column shown walks `stepsBack((c) => this.rows[c % RING_COLUMNS], oldest, 2 * this.sweepRows(), this.columns - stored)` and fills those rows into `this.column`, replacing the `extra`/`sweepRows()`-rows loop; `sweepRows()` keeps its body (steps per sweep over the newest row) and its comment says steps, not rows.
- `this.coarseMasked` (constructor, 0): coarse rows handed to `addCoarseEvent` that carried `masked` (for tests; never reset). The header comment lists it with `coarsePainted`.
- `app.js`: `wf()` gains `coarseMasked: wf.coarseMasked`; `waterfall() { return state.wf || null; }` (the instance, for e2e probes that call its methods inside the page; documented as test-only).

- [ ] **Step 1: Write the failing e2e scenario** in `ui.py` (after `scenario_coarse_layer_moves_on_when_the_fine_stream_stops`; the probe constant beside `TOP_QUARTER_LIT_JS`):

```python
# Paints the newest masked coarse row alone into a blank buffer and counts the background
# pixel rows under its notch and the lit ones elsewhere in its band; then redraws and counts
# the background rows under that notch in the replayed coarse column (both parities fill it).
MASK_PROBE_JS = """() => {
  const wf = window.zetaTest.waterfall();
  const rows = wf.coarseRows.map((r) => r.row);
  const row = [...rows].reverse().find((r) => r.masked);
  if (!row) return null;
  const h = wf.coarseColumn.height;
  const cw = wf.coarseColumn.width;
  const s = wf.shown();
  const yOf = (hz) => Math.round(((s.hi - hz) / (s.hi - s.lo)) * h);
  const isBg = (b, y) => b.data[y * cw * 4] === 0 && b.data[y * cw * 4 + 1] === 0 && b.data[y * cw * 4 + 2] === 48;
  const count = (b, y0, y1, bg) => {
    let n = 0;
    for (let y = Math.max(0, y0); y < Math.min(h, y1); y++) if (isBg(b, y) === bg) n++;
    return n;
  };
  const perBin = (row.hi - row.lo) / row.q.length;
  const [lo, hi] = row.masked[0];
  const notchTop = yOf(row.lo + hi * perBin) + 1;   // one px inside each edge
  const notchBottom = yOf(row.lo + lo * perBin) - 1;
  const buf = wf.ctx.createImageData(cw, h);
  wf.blankBuffer(buf);
  wf.fillColumn(row, false, buf);
  const notchRows = notchBottom - notchTop;
  const notchBgAlone = count(buf, notchTop, notchBottom, true);
  const restLit = count(buf, yOf(row.hi) + 1, notchTop - 1, false) + count(buf, notchBottom + 1, yOf(row.lo) - 1, false);
  wf.redraw();
  const notchBgRedraw = count(wf.coarseColumn, notchTop, notchBottom, true);
  return { masked: row.masked, notchRows, notchBgAlone, restLit, notchBgRedraw };
}"""


def scenario_coarse_layer_leaves_masked_bins_unpainted(page, url, secret):
    # A graph put of a 12 kHz notch on the page's own scan (48 kHz steps: four notches fit, so
    # it is in force): the notch's pixel rows are not painted by the row that masks them, and a
    # redraw, replaying both parities, fills them from the shifted sweep.
    stop_scans(url, secret)
    try:
        zoom_tone_out(page, url, secret)
        own = wait_until(lambda: running_scan_jobs(url, secret, "scanner"), 3.0,
                         "an automatic scan on scanner")[0]["id"]
        page.wait_for_function("() => window.zetaTest.wf().coarsePainted > 0", timeout=5_000)
        graph = http_json(url, secret, "/op", {"op": "graph_get", "job": own})["value"]
        fft = [n for n in graph["nodes"] if n["kind"] == "fft"]
        assert len(fft) == 1, f"scan graph {graph}"
        fft[0]["params"]["notch_hz"] = 12_000
        http_json(url, secret, "/op", {"op": "graph_put", "job": own, "graph": graph})
        ctx = http_json(url, secret, "/op", {"op": "job_context", "job": own})["value"]["scan"]
        assert ctx["mask"] == {"notch_hz": 12_000, "edge_mask": 0.0}, ctx
        assert ctx["alternate"]["shift_hz"] == 24_000, ctx
        assert ctx["alternate"]["steps_shifted"] == ctx["steps"] + 1, ctx
        # Two sweeps of masked rows (about 5 rows per 250 ms step, 5 steps).
        page.wait_for_function("() => window.zetaTest.wf().coarseMasked >= 20", timeout=10_000)
        probe = page.evaluate(MASK_PROBE_JS)
        assert probe and probe["masked"] == [[384, 641]], probe
        assert probe["notchRows"] > 10, probe
        assert probe["notchBgAlone"] == probe["notchRows"], probe
        assert probe["restLit"] > 0, probe
        assert probe["notchBgRedraw"] < probe["notchRows"] / 2, probe
    finally:
        stop_scans(url, secret)
```
  (`[[384, 641]]`: `spectrum_size(48000)` = 1024 bins of 46.875 Hz, `m = floor(6000 / 46.875) = 128`, so `[512 - 128, 512 + 128 + 1)`. A quiet bin 10 dB under the row's 15th percentile paints the background colour, so the test never asserts "no background outside the notch", only "all background inside it when painted alone" and "mostly filled after the redraw".)

- [ ] **Step 2: Run to verify it fails**: `ZETA_UI_ONLY=coarse_layer_leaves_masked_bins_unpainted flock ... cargo test -p zeta --test ui -- --test-threads=2` → FAIL (`zetaTest.waterfall` is not a function; with Tasks 2-3 in place the node already emits `masked` rows).
- [ ] **Step 3: Implement** `decodeRow`, `fillColumn`, `stepsBack`, `replayCoarse`, `redraw`, `coarseMasked`, the header comment and the two `app.js` hooks.
- [ ] **Step 4: Run**: the three scenarios with `ZETA_UI_ONLY`, then the whole `cargo test -p zeta --test ui` once (it must finish inside its 300 s deadline; note the wall time in the commit message), `cargo fmt`/clippy unaffected (JS) but run the workspace clippy anyway → green. Spec §5's last point, checked here: the other readers of `q` (`examples/python/cb_watch.py:91-106`, the harness's `scenario_waterfall_decodes_quantised_rows`) read fine `spectrum` rows, which carry no `masked` by default, so they need nothing; say so in the commit message.
- [ ] **Step 5: Commit**: `git commit -m "UI: a masked bin paints nothing (the pixel keeps what the previous row left); redraws replay the last two sweeps counted by steps; coarseMasked and zetaTest.waterfall() for the e2e; one scenario"`.

---

### Task 7: Field craft, docs, TODO and the plan series (Sonnet)

**Files:**
- Modify: `docs/agents/field-craft.md:13` (the `spectrum` event sentence), `crates/zeta/src/agents_md.rs:56` (the `job_start` note's `scan` sentences), `README.md:669-680`, `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md:39` (row 15) and a new "## Plan 15 status" section above "## Plan 20 status" (L47), `TODO.md:3-33`, `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `cargo test -p zeta --test agents_md --test transports`

- [ ] **Step 1: Write the prose.**
  - `agents_md.rs` `job_start` note: after "wrapping until stopped." add: "Alternate sweeps shift the step grid by half a step (the odd sweep has one more step), so no frequency sits under a step centre on every sweep; `scan.alternate` in the job context gives `shift_hz` and `steps_shifted` (`null` for a one-step span). A scan on a local hardware receiver masks each row's centre notch (`notch_hz`, 300000 Hz) and its outer edges (`edge_mask`, 0.10 of the bins each side); every other source gets none. A row with masked bins carries `masked: [[lo, hi], ...]` (bin index ranges, `lo` inclusive, `hi` exclusive): those bins are no data, not low power; their `q` is still present. `scan.mask` reports the values in force (a step narrower than four notches gets no notch), and `graph get`, edit the `fft` node's `notch_hz`/`edge_mask`, `graph put` changes them." Extend "A scan's `job_context` holds `scan.steps` ..., `scan.step_hz` and `scan.dwell_ms`" with `scan.alternate` and `scan.mask`. In the `watch` sentence on `detect` params, add that `detect` also honours a row's `masked` ranges.
  - `docs/agents/field-craft.md` ("Reading measurements", the `spectrum` event sentence): append "A row may carry `masked: [[lo, hi], ...]`, bin index ranges (`lo` inclusive, `hi` exclusive) the receiver could not see honestly (a scan step's DC notch, its band edges): read them as no data, never as a quiet band; the `q` under them is present only so an older reader sees a whole row. A scan's steps alternate between two grids half a step apart, so a frequency masked in one sweep is clear in the next: judge a frequency from two consecutive sweeps. On a local hardware receiver a `measure` job ignores the bins within 25 kHz of the capture centre (`job context` shows `measure.dc_mask_hz`, 0 when none), so a box that holds the DC spike measures the signal."
  - `README.md` scan paragraph: alternation, `masked`, `scan.alternate`/`scan.mask`, `graph put` on the scan's `fft` node.
  - Plan series row 15: "Spec `docs/superpowers/specs/2026-10-31-scan-grid-mask-design.md`, plan `docs/superpowers/plans/2026-10-31-scan-grid-mask.md`. **Merged** to `main` pending the session's fast-forward, 2026-10-31; live acceptance on the desktop HackRF is pending (see "Plan 15 status")." New "## Plan 15 status" (above "Plan 20 status"): what was built per task (one line each), the default constants and that they are to be corrected by the live run, then the acceptance checklist of spec §9 as unchecked items: (1) the 440-488 MHz survey at 45 dB with a 2 s dwell look shows no hump at any step centre and no 100 kHz grid of peaks moving with the parity; (2) with `notch_hz: 0` for one run, the hump's width at the floor at 2, 8 and 16 Msps read from the rows, `SCAN_NOTCH_HZ` set to the widest plus 20 %, the same for the edge roll-off and `SCAN_EDGE_MASK`, figures recorded here and in the TODO gist; (3) `zeta job context` on the scan shows `scan.alternate` and `scan.mask`, `zeta stream tail` shows rows with `masked`; (4) the `detect` change is covered by its unit test only (no watch inside a scan step is started).
  - `TODO.md`: replace the body of "## Scan: alternate the step grid and mask the centre and edge bins" (L3-33) with "Done in plan 15 (docs/superpowers/specs/2026-10-31-scan-grid-mask-design.md); live acceptance pending: the notch and edge widths it measures set `SCAN_NOTCH_HZ` and `SCAN_EDGE_MASK` (300 kHz and 0.10 to begin with)." Add, after it, "## Lease: keep a lone job's capture centre off its channel" with the spec's wording (§2, §6): "From plan 15's non-goals, 2026-10-31. A demodulator's or decoder's own capture centre sits on its channel, so the receiver's DC spike lands in the channel (the same problem the scan grid had; a one-off demod read it as a 59 dB carrier on 2026-10-23). Keeping the centre off the channel needs the lease manager to know the capture's minimum width before it tunes, which the capability record does not carry, and the offset depends on the notch width plan 15 measures (`SCAN_NOTCH_HZ`: its live acceptance's figure is this gist's input). Its own small plan after plan 15."
- [ ] **Step 2: Regenerate** `AGENTS.md` and the skill; `flock ... cargo run -p zeta -- agents-md --check`, `--skill --check`, `cargo test -p zeta --test agents_md --test transports` → green.
- [ ] **Step 3: Final gate**: `flock /Projects/ZetaSDR/target/.cargo.lock env CARGO_BUILD_JOBS=2 cargo test --workspace --all-features --no-fail-fast -- --test-threads=2` → no `FAILED`, no `ui e2e skipped`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo fmt --all -- --check`.
- [ ] **Step 4: Commit**: `git commit -m "Docs: plan 15 field craft (masked bins are no data, two sweeps make one look), README, plan series status with the live-acceptance checklist, TODO (scan gist closed, lease-offset gist added)"`.

---

## After the plan (not tasks: the session does these)

1. Whole-branch review, then merge `plan-15` into `main` (fast-forward).
2. On the desktop HackRF (`--source-soapy hackrf=driver=hackrf,gain=45`), spec §9: the 440-488 MHz survey with a 2 s dwell look, the `notch_hz: 0` run to measure the hump at 2, 8 and 16 Msps and the edge roll-off, `SCAN_NOTCH_HZ`/`SCAN_EDGE_MASK` corrected to the widest plus 20 %, `job context` and `stream tail` checked. Record the figures in "Plan 15 status" and the lease-offset TODO gist.
3. Update the HackRF field-setup memory file with the measured notch and the alternation (a survey needs two sweeps).
