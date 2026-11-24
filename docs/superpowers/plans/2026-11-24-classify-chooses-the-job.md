# Classify Chooses the Job (Plan 48) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:**
- For a selection or a detected signal, the node answers the job it would start (kind, centre, width, parameters), the evidence for it and a runner-up, or that it cannot tell, with no agent and no model.
- It proves the answer further as far as a per-surface policy allows.
- The page, the watch and the agent use that one answer.

**Architecture:**
- **Rows.** The four demodulators become mode rows with a `builtin` decoder form, so one description covers every job a signal can become.
- **Step 1.** `classify` ranks every row and answers a `choice` at step 1.
- **Steps 2 and 3.** A new controller job, `choose`, climbs two further steps:
  - step 2 replays the ring slice, written into the burst store, through the candidate decoders;
  - step 3 is an identify-style trial on the live band, sharing a running capture only.
- **The watch.** It starts the step-1 answer at once, on probation (a `held` job: out of the mix, its transcriber buffering), and settles on the `choose` verdict.

**Tech Stack:** Rust 1.97 (zeta-core, zeta-dsp, zeta-speech, zeta), the plain-JS page (`crates/zeta-ui/web`), Python Playwright scenarios (`crates/zeta-ui/tests/e2e/ui.py`) started by `crates/zeta/tests/ui.rs`.

**Spec:** `docs/superpowers/specs/2026-11-24-classify-chooses-the-job-design.md` (approved 2026-11-24). Read it first. It is cited below as "spec 5.3" and so on.

**Scope:** spec parts 48.1 to 48.4 and 48.6. Part 48.5, `borrow` (a trial on an idle radio under a lease that yields), is its own plan, 48.5, written after this one lands. The `borrow` lease setting is not in this plan's policy enum.

## Global Constraints

- **One operation set.** New fields and operations go in `crates/zeta-core/src/ops.rs` and are applied by `Node::apply`; the transports only translate.
- **Generated files.** `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated. Prose goes in `crates/zeta/src/agents_md.rs` or `docs/agents/field-craft.md`; regenerate, never hand-edit.
- **Existing behaviour holds.** A job that is not a `choose` and a watch without `on_signal: choose` behave exactly as before. Identify's and the watch's rankings are unchanged on every existing test. Counts that move: Op variants 56 to 58 (`tests/ops.rs`), `BUILTIN_KINDS` 19 to 20, `EMBEDDED` rows 28 to 32 (`modes/mod.rs`, `tests/modes.rs`).
- **Never proof from evidence.** Only a decode passing its row's own identify rule proves a row. Register, catalogue, learned and layer-3 terms only rank.
- **A trial never touches another job.** It never stops a job, retunes a radio, or tunes an unheld radio. Step 3 runs only where a running capture already covers the band (`share`), or grows a held capture without moving any holder's band (`widen`).
- **No grid.** The answer's centre is the measured centre, never a plan channel's or a raster's. A plan channel supplies the deviation, and the width when the measured width is distrusted, nothing else.
- **No station limits in data.** No frequency ceilings, region boxes or device caps in rows or policy defaults. Region facts come from the plans' `region` and the catalogue's `regions`.
- **Record and answer fields added after format 1** use `#[serde(default)]`, plus `skip_serializing_if` where the field is usually absent (as `JobRecord.replay` does), so older nodes and clients still read them.
- **Exact defaults** (spec 5.1, 5.4):

  | Row | `bandwidth_hz` | Width rule | Params |
  |---|---|---|---|
  | `fm_demod` | 12500 | scale 1.25, floor 5000, distrust below floor `fm`, `fsk`, `unknown` | `deviation_hz` 5000, `audio_lowpass_hz` 3400, `deemphasis_us` 530 |
  | `wfm_demod` | 200000 | scale 1.25, floor 150000 | `deviation_hz` 75000, `deemphasis_us` 50 |
  | `am_demod` | 10000 | scale 1.25, floor 5000 | |
  | `ssb_demod` | 6000 | fixed 6000, centred on the dial | `audio_bandwidth_hz` 2800 |

  - Trial quota 64 MiB inside the burst store's limit.
  - Policy defaults:
    - `menu`: default 1, cap 2;
    - `watch`: default 3, cap 3;
    - `agent`: default 1, cap 3;
    - `client`: default 1, cap 3;
    - lease `share`;
    - budget `trial_s` 30, `concurrent` 3, `slice_s` 2.0;
    - `confidence_stop` null.
- **Exact strings:**
  - `not tried: no running capture covers it`
  - `not tried: budget`
  - `not tried: the ring holds under 1 s`
  - `not tried: the slice exceeds the trial quota`
  - `not tried: capped at step <n> for <surface>`
  - `not tried: <row> is not ready: <reason>`
  - `cannot tell: <reason>`
  - Hold reason `probation`.
- **Public repository.** Use roles, not host names, and no observed traffic. Fixtures are synthesised (`zeta_dsp::synth`) except the existing real OOK and VDL2 captures. `scripts/public-check.sh` and `scripts/check-doc-links.sh` must be clean before the branch lands.
- **Gates** (`CLAUDE.md`):
  - `cargo fmt -p <crate> -- --check` per crate, because this is a nested worktree;
  - clippy with `-D warnings`;
  - a private `CARGO_TARGET_DIR`;
  - each task runs only its own tests and their neighbours; crate suites once per batch; the workspace and the whole UI suite (`cargo test -p zeta --test ui`) once at the end;
  - `zeta-core` and `zeta-dsp` test runs take `--features test-fixtures`.
- **Polling.** Tests that read state right after an operation poll for the end state (`common::until`, ui.py `wait_until`), never read once.
- **Commit messages** start `Choice: <what changed>`.

## Review Focus

- **A `choose` started on a source whose ring holds less than the slice** (a source just started, or a ring of 1 s as in the tests): step 2 replays what the ring holds when that is at least 1 s, else it records `not tried: the ring holds under 1 s`. Step 1's answer stands, and the job ends `stopped` with a verdict. (Task 9)
- **A selection on a wide capture** (20 MS/s): the slice is shortened to fit the trial quota, or step 2 is not tried with the quota string. No capture the operator made is ever expired by a trial slice. (Tasks 7, 9)
- **An operator `--modes-dir` row with id `fm_demod`:**
  - a `builtin` row naming `fm_demod` replaces the embedded one, and its defaults are used;
  - the same id with any other decoder form loads broken, naming the reason;
  - a variant id (`fm_narrow`) naming `builtin: fm_demod` starts as its own kind with its own defaults.

  (Tasks 1, 2)
- **A node restarted while a watch's job is on probation:** the held job comes back, its `choose` child does not resume, and the watch lifts the hold (the step-1 answer stands) instead of leaving the job held forever. (Task 14)
- **The box menu closed or reopened before its background `choose` ends:** the old transient job is stopped, and once all menus are closed no transient job remains (`job_list --all`). (Tasks 11, 12)

---

### Task 1: The `builtin` decoder form, `proof: none`, `role`, and the four demodulator rows

**Files:**
- Modify:
  - `crates/zeta-core/src/modes/row.rs` (`Decoder` :448, `DecoderKind` :474, `Proof` :644, `Band` :364, `validate` :1163 including the built-in name refusal at :1170-1188);
  - `crates/zeta-core/src/modes/mod.rs` (`EMBEDDED` :289, `ModeRegistry::add` :558, `LoadedMode`);
  - `crates/zeta-core/src/modes/template.rs` (`outputs` :195, `voices` :256, `expand` :73);
  - `crates/zeta-core/src/modes/handler_file.rs` (`own_handler_id` :157);
  - `crates/zeta-dsp/src/identify.rs` (`classify_candidate` :267);
  - every other match on `Decoder`/`DecoderKind` the compiler names (`node/propose.rs`, `signatures.rs`, `zeta-dsp/src/m17_decode.rs`, `zeta-ext/src/{adsb,external}.rs`, `zeta/src/node_setup.rs`);
  - `crates/zeta-core/src/ops.rs` (`ModesList` :994, `ModeInfo` :3575, dispatch :6241);
  - `crates/zeta-ui/web/menu.js` `modeRows()` :1395 (sends `role: null`).
- Create: `modes/fm_demod.json`, `modes/wfm_demod.json`, `modes/am_demod.json`, `modes/ssb_demod.json`.
- Test: `crates/zeta-core/tests/modes.rs`.

**Interfaces:**
- Produces:
  - `Decoder::Builtin(BuiltinBlock)`, written `{"kind": "builtin", "builtin": "fm_demod", "params": {...}}`.
  - `pub struct BuiltinBlock { pub builtin: String, #[serde(default)] pub params: serde_json::Map<String, Value> }`, with `deny_unknown_fields`.
  - `DecoderKind::Builtin`.
  - `pub const DEMOD_KINDS: [&str; 4] = ["fm_demod", "wfm_demod", "am_demod", "ssb_demod"]` in `row.rs`.
  - `Proof::None`.
  - `Band.width_rule: Option<WidthRule>` (`#[serde(default)]`):

    ```rust
    pub struct WidthRule { #[serde(default)] pub scale: Option<f64>, #[serde(default)] pub floor_hz: Option<u64>,
        #[serde(default)] pub fixed_hz: Option<u64>, #[serde(default)] pub distrust_below_floor: Vec<String>,
        #[serde(default)] pub center: CenterRule }
    #[serde(rename_all = "snake_case")] pub enum CenterRule { #[default] Measured, Dial }
    ```

  - `#[serde(rename_all = "snake_case")] pub enum Role { Decoder, Demodulator }`.
  - `ModeRow::role(&self) -> Role` (`Demodulator` iff the decoder is `Builtin`).
  - `LoadedMode::role()`.
  - `ModeInfo.role: Role` (`#[serde(default)]`).
  - `ModesList.role: Option<Role>`, nullable like `family`, with `#[serde(default)]` so a request without it reads as `null`.
- Rules:
  - `validate` accepts an id equal to a built-in kind only when the row is `Builtin` with `builtin == id` and `id` in `DEMOD_KINDS`. Every other built-in name stays refused, as now.
  - A `Builtin` row's `builtin` must be in `DEMOD_KINDS`. Its `identify.proof` must be `none`. Its `protocol` is `squelch` for `fm_demod`/`am_demod` and `none` for `wfm_demod`/`ssb_demod`.
  - `outputs` gives `[Audio]` (plus `Event` for the `squelch` rows).
  - `voices` gives `["audio"]`.
  - `classify_candidate` is false for a `Builtin` row.
  - `own_handler_id` is `BUILTIN` (it has no handlers).
  - A `Builtin` row is `Ready` with no probe.

- [ ] **Step 1: Write the failing tests** in `tests/modes.rs`:

```rust
#[test] fn the_four_demodulator_rows_load_ready_with_role_demodulator()  // ids == DEMOD_KINDS; role Demodulator; decoder_kind Builtin; state Ready
#[test] fn a_builtin_row_may_carry_its_own_kind_name_and_no_other()      // fm_demod row ok; a Graph row with id fm_demod Broken "fm_demod is a built-in job kind"; id "scan" still Broken
#[test] fn a_builtin_row_naming_an_unknown_kind_is_broken()               // builtin "foo_demod" -> Broken naming it
#[test] fn an_operator_builtin_row_replaces_the_embedded_one()             // --modes-dir fm_demod.json with params.deviation_hz 2500 -> registry row has 2500
#[test] fn a_variant_row_names_a_builtin_under_its_own_id()                // id fm_narrow, builtin fm_demod -> Ready, role Demodulator
#[test] fn modes_list_filters_by_role()                                    // role demodulator -> exactly the four; role decoder -> none of them; null -> all
#[test] fn a_builtin_row_is_no_classify_candidate()                        // zeta_dsp classify_candidate(fm_demod row) == false
```

Update `a_built_in_kind_name_is_refused_as_a_row_id` (:402): `fm_demod` with a non-builtin decoder is still `Broken`. Update `EMBEDDED: [&str; 32]`.

- [ ] **Step 2: Run them to see them fail.**
  - Command: `cargo test -p zeta-core --features test-fixtures --test modes`
  - Expected: FAIL (unknown variant `builtin`, and missing rows).

- [ ] **Step 3: Implement the types, the validation rules and the four rows** with the values in Global Constraints.
  - The `event` block is the minimal one `validate` accepts (`schema.properties` an empty object).
  - `identify` is `{"proof": "none", "min_events": 0, "dwell_ms": 0, "min_span_ms": 0}`.
  - `modulation`:
    - `fm_demod`: `["fm", "fsk"]`
    - `wfm_demod`: `["wfm"]`
    - `am_demod`: `["am"]`
    - `ssb_demod`: `["ssb"]`
  - `range_hz` is absent.
  - Each row's `notes` says it is the demodulator a choice falls back to.
  - Handle every `Decoder` match the compiler names.

- [ ] **Step 4: Run** `cargo test -p zeta-core --features test-fixtures --test modes` and `cargo test -p zeta-dsp --features test-fixtures --lib identify`. Expected: PASS.

- [ ] **Step 5: Commit** `Choice: demodulator rows: a builtin decoder form, proof none, role, and fm/wfm/am/ssb rows`.

### Task 2: The demodulator expansion reads its row, and a plan channel's deviation on a direct start

**Files:**
- Modify: `crates/zeta-core/src/ops.rs`:
  - `expand_voiced` :2245 and the four arms :2427-2570;
  - `check_start_params` :4888;
  - `plan_check_as` :6536;
  - the constants :1566-1573, :1413, :3079-3082 (kept, but no longer the source).
- Modify: `crates/zeta-core/src/node/mod.rs` `norm` :3274; `crates/zeta-core/src/node/watch.rs` `start_on` :1280 (passes the plan deviation).
- Test: `crates/zeta-core/tests/ops.rs` (beside `deviation_param_reaches_nbfm_node` :2037), `crates/zeta-core/tests/modes.rs`.

**Interfaces:**
- Consumes: Task 1's `Builtin` rows.
- Produces:
  - `pub struct DemodDefaults { pub deviation_hz: Option<u64>, pub audio_lowpass_hz: Option<u64>, pub deemphasis_us: Option<u64>, pub audio_bandwidth_hz: Option<u64> }`.
  - `pub fn demod_defaults(modes: &ModeRegistry, row_id: &str) -> DemodDefaults` in `ops.rs`. It reads the row's `params`; when the row is absent it falls back to the constants.
  - `pub fn plan_channel_at(center_hz: u64, region: &str) -> Option<(&'static ChannelPlan, u32)>` in `plans.rs`: `channel_at` restricted to plans whose `region` equals `region`, compared case-insensitively (`au` and `AU`), or is `world`.
  - `pub fn plan_deviation_at(center_hz: u64, region: &str) -> Option<u64>` (`plan_channel_at(..).and_then(|(p, _)| p.deviation_hz)`).
  - The region is the station's, else `knowledge::DEFAULT_REGION`, the same rule `node/origin.rs:274` uses. `pub fn Node::region(&self) -> String` returns it.
- Rules:
  - The kind's arm takes each parameter from, in order: the request, then a plan channel holding the centre (`plan_deviation_at`, `fm_demod` only, as `plan_start` does), then `demod_defaults`.
  - A `Mode(id)` job whose row is `Builtin` expands through its `builtin` kind's arm with that row's defaults.
  - `check_start_params` allows `deviation_hz` for such a row when its `builtin` is `fm_demod`/`wfm_demod`, and `sideband` when it is `ssb_demod`.
  - The watch's `start_on` passes `plan_deviation_at(tuning.center_hz, &node.region())` for an `fm_demod`.

- [ ] **Step 1: Write the failing tests.**

```rust
// tests/ops.rs
#[tokio::test] async fn the_builtin_rows_hold_the_old_constants()                // demod_defaults(fm_demod).deviation_hz == Some(5000), wfm 75000, lowpass 3400, deemph 530/50, ssb 2800
#[tokio::test] async fn a_direct_fm_demod_on_a_plan_channel_gets_the_plans_deviation() // job_start fm_demod at a uhf_cb_au channel centre (from plans), no deviation -> nbfm_demod.params.deviation_hz == 2500
#[tokio::test] async fn an_asked_deviation_beats_the_plan()                      // same with deviation_hz 4000 -> 4000
#[tokio::test] async fn a_plan_of_another_region_gives_no_deviation()           // a station with region "US": the same start -> 5000
#[tokio::test] async fn an_operator_row_changes_the_default_deviation()          // --modes-dir fm_demod params.deviation_hz 3000, off any plan -> 3000
#[tokio::test] async fn a_variant_row_starts_with_its_own_defaults()             // fm_narrow (builtin fm_demod, deviation 2000): record kind "fm_narrow", nbfm deviation 2000; deviation_hz accepted, sideband refused
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test ops -- deviation plan_s builtin variant`. Expected: FAIL.
- [ ] **Step 3: Implement** `demod_defaults`, `plan_deviation_at`, the arm changes, the `Mode`-builtin path in `expand_voiced` (before the row-template path), `check_start_params` and `start_on`. `norm` (duplicate matching) uses `demod_defaults` so that two starts that differ only by an implicit default still match.
- [ ] **Step 4: Run** the step 2 command plus `--test modes` and `--test watch`. Expected: PASS, with the existing deviation tests unchanged.
- [ ] **Step 5: Commit** `Choice: the demodulators expand from their rows; a direct fm_demod on a plan channel takes the plan's deviation`.

### Task 3: Demodulator rows stay out of every decoder ranking and list

**Files:**
- Modify:
  - `crates/zeta-core/src/modes/score.rs` `score_rows` :141 (filter at :152);
  - `crates/zeta-core/src/ops.rs` `handler_policy_set` :6294 (refuse a demodulator row);
  - `crates/zeta-core/src/node/mod.rs` `capture_decode` :2979 (refuse it, naming the reason);
  - `crates/zeta/src/modes_check.rs` :168 (a `Builtin` row is checked by expanding it at a synthetic tuning, not by a synthesised capture);
  - `crates/zeta-ui/web/actions.js` `rowOf` :396, `replayRows` :581, `decode_across` :682 (skip `role: "demodulator"`).
- Test: `crates/zeta-core/tests/identify.rs`, `crates/zeta-core/tests/modes.rs`, `crates/zeta-core/tests/capture_decode.rs`, `crates/zeta/tests/cli.rs` (modes check).

**Interfaces:**
- Consumes: `LoadedMode::role()`.
- Produces: nothing new. `score_rows` returns only `Role::Decoder` rows.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn score_rows_never_returns_a_demodulator_row()                 // score_rows on a 12.5 kHz "fm" signal: no id in DEMOD_KINDS; same order as before for the rest
#[tokio::test] async fn identify_never_trials_a_demodulator_row()      // identify on the pocsag fixture: no candidate event names fm_demod
#[tokio::test] async fn capture_decode_refuses_a_demodulator_row()     // capture_decode(.., "fm_demod") -> error naming "a demodulator row has no decoder to replay into"
#[tokio::test] async fn the_handler_policy_refuses_a_demodulator_row()
#[test] fn modes_check_checks_a_builtin_row_by_its_expansion()          // `zeta modes check` lists fm_demod passed
```

- [ ] **Step 2: Run** the named tests. Expected: FAIL.
- [ ] **Step 3: Implement** the filters and refusals. The exact refusal is `<id> is a demodulator row: <op> takes decoder rows`.
- [ ] **Step 4: Run** `cargo test -p zeta-core --features test-fixtures --test identify --test modes --test capture_decode --test watch` and `cargo test -p zeta-dsp --features test-fixtures --test classify`. Expected: PASS. Every existing identify, watch and classify test is unchanged.
- [ ] **Step 5: Commit** `Choice: demodulator rows stay out of decoder rankings, handler policy, replay and Decode`.

### Task 4: Layer 1's measurement as fields in the classify answer

**Files:**
- Modify:
  - `crates/zeta-core/src/classify.rs` (`FeatureRecord` :18, `Classification` :123, `run_all` :262);
  - `crates/zeta-dsp/src/measure.rs` (`measure_band_noted` :458, `classify_block` :880; carry `power99_hz` from `RateNote` :661, `audio_rms_hz` :778, the SSB side and dial :977-995 into the record);
  - `crates/zeta-core/src/ops.rs` (`OpResult::Classification` :4043, `classify` :8500, `classify_capture` :8472).
- Test: `crates/zeta-dsp/tests/classify.rs`, `crates/zeta-core/tests/ops.rs`.

**Interfaces:**
- Produces:
  - On `FeatureRecord`, all `#[serde(default)]`: `power99_hz: Option<Hz>`, `audio_rms_hz: Option<f32>`, `ssb: Option<SsbFeature>`, with `pub struct SsbFeature { pub sideband: Sideband, pub dial_hz: Hz }`.
  - `classify::Classification.measurement: Option<Measurement>` (`#[serde(skip)]`, filled by `run_all`).
  - `OpResult::Classification.features: Option<FeatureRecord>` (`#[serde(default)]`).
  - `pub(crate) async fn classify_samples(node: &Node, sel: &Selection) -> Result<(Classification, Vec<Complex<f32>>, Hz, Hz), OpError>`: the body of `classify` up to `supersede_proved`, returning the samples, rate and ring centre too. `classify` calls it. Task 8 reuses it.

- [ ] **Step 1: Write the failing tests.**

```rust
// zeta-dsp/tests/classify.rs
#[tokio::test] async fn classify_answers_the_measured_features_as_fields()  // nbfm_modulate speech, 2500 Hz dev: features.bandwidth_hz > 0, audio_rms_hz Some(v) with 300 < v < 2500, power99_hz Some
#[tokio::test] async fn classify_answers_the_ssb_side_and_dial()            // ssb_modulate usb at dial D: features.ssb == {usb, D ± 100}
```

- [ ] **Step 2: Run** `cargo test -p zeta-dsp --features test-fixtures --test classify -- features ssb_side`. Expected: FAIL.
- [ ] **Step 3: Implement.** The evidence strings stay as they are.
- [ ] **Step 4: Run** the step 2 command and the rest of `--test classify`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: classify answers layer 1's measurement as fields`.

### Task 5: The weights table

**Files:**
- Create: `crates/zeta-core/src/modes/weights.rs`, `modes/weights.json`.
- Modify: `crates/zeta-core/src/modes/score.rs` (the constants :9-18 come from `Weights`), `crates/zeta-core/src/modes/mod.rs` (load `weights.json` embedded, and `--modes-dir/weights.json` over it), `crates/zeta-core/src/node/mod.rs` (`Node::weights() -> Arc<Weights>`).
- Test: unit tests in `weights.rs`; the `score.rs` tests.

**Interfaces:**
- Produces:
  - `pub struct Weights { pub center: f32, pub bandwidth: f32, pub modulation: f32, pub allocation: f32, pub catalogue: f32, pub learned: f32, pub layer3: f32, pub runner_up_margin: f32 }`, with `deny_unknown_fields`.
  - Starting values: center 0.4, bandwidth 0.3, modulation 0.3, allocation 0.2 (as now); catalogue 0.25; learned 0.3; layer3 0.0; runner_up_margin 0.15.
  - `score_rows` gains `weights: &Weights` as its first parameter. Every caller (score.rs header lists them: `node/identify.rs:617`, `node/watch.rs:145`, `zeta-dsp/src/identify.rs:156,:162,:588`, the tests) passes `&node.weights()`, or `&Weights::default()` in `zeta-dsp`.
  - `impl Default for Weights` is the embedded file.
- Rule: with the starting weights, every `score_rows` result is bit-for-bit what it was. The catalogue's weight applies only in the choice (Task 6).

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn the_embedded_weights_are_the_old_constants()             // center 0.4, bandwidth 0.3, modulation 0.3, allocation 0.2
#[test] fn an_operator_weights_file_overrides_one_value()            // modes_dir/weights.json {"runner_up_margin": 0.3, ...all others} -> 0.3
#[test] fn a_weights_file_with_an_unknown_key_is_a_load_error()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --lib weights`. Expected: FAIL.
- [ ] **Step 3: Implement.** A broken operator file is a load error, listed like a broken handler file, and the embedded weights stay in force.
- [ ] **Step 4: Run** the step 2 command plus `--lib score`, `--test identify` and `--test watch`. Expected: PASS, all unchanged.
- [ ] **Step 5: Commit** `Choice: one weights table for the ranking terms, embedded with operator override`.

### Task 6: The choice at step 1, in `classify`; CLI and agent prose

**Files:**
- Create: `crates/zeta-core/src/choice.rs`.
- Modify:
  - `crates/zeta-core/src/lib.rs`;
  - `crates/zeta-core/src/ops.rs` (`OpResult::Classification.choice`, `classify`);
  - `crates/zeta/src/` (the `classify` command prints the choice first);
  - `crates/zeta/src/agents_md.rs` (the `classify` note :209: ten labels, `ook` and `psk` included; `choice`; the demodulator rows);
  - `docs/agents/field-craft.md` (:175, :238, :251, :294: replace the demodulator and width prose with a pointer to the rows and the choice);
  - `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated).
- Test: `crates/zeta-core/tests/choice.rs` (new), `crates/zeta/tests/agents_md.rs`.

**Interfaces:**
- Consumes: Tasks 1 to 5.
- Produces, in `choice.rs`:

```rust
#[serde(rename_all = "snake_case")] pub enum Settled { Proved, BestGuess, CannotTell }
pub struct ChoiceJob { pub kind: String, pub center_hz: Hz, pub bandwidth_hz: Hz, pub params: serde_json::Map<String, Value> }
pub struct Alternative { pub job: ChoiceJob, pub row: String, pub evidence: Vec<String> }
pub struct NotTried { pub row: Option<String>, pub reason: String }
pub struct Choice { pub job: Option<ChoiceJob>, pub row: Option<String>, pub step: u8, pub settled: Settled,
    pub evidence: Vec<String>, pub runner_up: Option<Alternative>, pub could_prove: Vec<String>,
    pub not_tried: Vec<NotTried>, pub reason: Option<String> }
pub fn choose_now(modes: &ModeRegistry, weights: &Weights, c: &Classification, sel: &Selection) -> Choice
pub fn job_for(row: &LoadedMode, f: &FeatureRecord, sel: &Selection) -> ChoiceJob   // the width rule and params, spec 5.1
```

- `OpResult::Classification.choice: Option<Choice>` (`#[serde(default)]`).

- **The ranking** (spec 5.2):
  1. A `Proved` entry in `c.tried` gives that row, `job_for` at the row's width, `settled: proved`.
  2. Otherwise the demodulator rows whose `modulation` holds the layer-1 label, ordered by `weights` terms. For `fsk`, `fm_demod` applies only when `features.bandwidth_hz <= 25000` (the field craft's weak-voice case).
     - `could_prove` holds the ready decoder rows from `score_rows` that fit the band and width, in score order. Rows not ready go to `not_tried` with `<row> is not ready: <reason>`.
     - `runner_up` is the first of `could_prove` when its score is within `weights.runner_up_margin` of the demodulator's, else the next demodulator row.
     - With no demodulator for the label, the top `could_prove` row is the job, `settled: best_guess`, and the next one is the runner-up.
  3. Otherwise `settled: cannot_tell`, `job: null`, and `reason`:
     - `noise` gives `the band holds noise only`;
     - a label nothing serves gives `no row serves <label>`.
- **`job_for`:**
  - Width: `fixed_hz` if set; else `max(occupied × scale, floor_hz)`; when the label is in `distrust_below_floor` and the occupied width is under the floor, the first of: the plan channel's width at the measured centre (`plan_channel_at` in the node's region), `power99_hz`, the selection's width.
  - Centre: the measured centre, or the SSB dial for `center: dial`.
  - Params: `deviation_hz` from `plan_deviation_at` in the node's region (fm), else the row's `params`; `sideband` from `features.ssb` (ssb).
- **Evidence lines:** `layer 1: <label> <width> Hz`, `plan: <id> deviation <d>`, `decode <row>: <summary>`, `catalogue: <name>` (the catalogue only ranks).

- [ ] **Step 1: Write the failing tests** in `tests/choice.rs`, a node over `zeta_dsp::synth` fixtures (`common::cfg` with a file source, as `tests/identify.rs` `node_over` :192):

```rust
#[tokio::test] async fn cb_voice_answers_fm_demod_with_the_plans_deviation()   // nbfm speech, 2500 dev, at a uhf_cb_au centre + 700 Hz: job.kind fm_demod, params.deviation_hz 2500, center_hz == measured (not the channel centre), settled best_guess
#[tokio::test] async fn a_pocsag_burst_answers_pocsag_decode_proved()
#[tokio::test] async fn a_broadcast_station_answers_wfm_demod()                  // wfm_stereo_modulate
#[tokio::test] async fn an_am_carrier_answers_am_demod()
#[tokio::test] async fn usb_answers_ssb_demod_on_the_dial()                       // center_hz == dial, params.sideband "usb", bandwidth 6000
#[tokio::test] async fn noise_answers_cannot_tell()                              // reason "the band holds noise only", job null
#[tokio::test] async fn a_dmr_burst_answers_fm_demod_with_dmr_in_could_prove()   // dmr_iq: job fm_demod, could_prove contains "dmr_decode"
#[tokio::test] async fn a_range_only_catalogue_match_never_overturns_the_label()  // a selection inside a catalogue entry's range with an "am" signal -> am_demod
```

In `tests/agents_md.rs`: `the_classify_note_lists_all_ten_labels` (each of `noise ook fsk wfm psk am ssb fm carrier unknown`).

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test choice` and `cargo test -p zeta --test agents_md`. Expected: FAIL.
- [ ] **Step 3: Implement** `choice.rs`, the answer field, the CLI line, the agent prose and the field craft pointer. Regenerate: `cargo run -q -p zeta -- agents-md > AGENTS.md` and `cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`.
- [ ] **Step 4: Run** the step 2 commands, `cargo run -q -p zeta -- agents-md --check` and `--skill --check`. Expected: PASS and no drift.
- [ ] **Step 5: Commit** `Choice: classify answers one ranked choice with its evidence, runner-up and the rows that could prove otherwise`.

### Task 7: The ring slice into the burst store, a trial quota, and `iq_read` sharing the path

**Files:**
- Modify:
  - `crates/zeta-core/src/bursts.rs` (`BurstPolicy` :29, `CaptureRecord` :53, `NewCapture` :110, `write` :193, `expire` :331);
  - `crates/zeta-core/src/ops.rs` (`iq_read` :8597, `OpResult::Iq`);
  - `crates/zeta/src/cli.rs` (`--bursts-trial-mb` beside `--bursts-keep-mb` :177);
  - `TODO.md` (close the two `iq read` gists under "Captures: a RAM-backed store").
- Test: the unit tests in `bursts.rs`, `crates/zeta-core/tests/capture_ops.rs`, `crates/zeta-core/tests/ops.rs` (`iq_read` tests :280, :313), `crates/zeta-core/tests/capture_decode.rs`.

**Interfaces:**
- Produces:
  - `NewCapture.file_center_hz: Option<Hz>`: the IQ file's centre when it differs from the record's band.
  - `CaptureRecord.trial: bool` (`#[serde(default, skip_serializing_if = "std::ops::Not::not")]`).
  - `NewCapture.trial: bool`.
  - `BurstPolicy.trial_keep_bytes: u64` (default `64 * 1024 * 1024`).
  - `pub fn write_ring_slice(node: &Node, sel: &Selection, samples: &[Complex<f32>], rate: Hz, ring_center: Hz, trial: bool) -> Result<CaptureRecord, OpError>` in `ops.rs`. It writes a fresh `set` and `job` ulid, records the selection's band, and sets `file_center_hz` to the ring centre.
  - `OpResult::Iq.capture: Option<String>` (`#[serde(default)]`).
- Rules:
  - Trial records count against `trial_keep_bytes` only, and expire oldest first when it is full. Operator records count against `keep_bytes` only. Neither ever expires the other.
  - A pinned trial counts against neither, as pinned records do now.
  - `iq_read` writes through `write_ring_slice(.., trial: false)` and returns the capture id plus the data path (the CLI output is unchanged). Its recordings now expire under the burst policy.

- [ ] **Step 1: Write the failing tests.**

```rust
// bursts.rs
#[test] fn a_trial_never_expires_an_operator_capture()      // keep 1 MB operator capture + trials past quota: operator record stays, oldest trial goes
#[test] fn operator_captures_never_expire_a_trial()
// capture_ops.rs
#[tokio::test] async fn a_ring_slice_record_reads_its_selection_not_the_ring()  // write_ring_slice on a 10 kHz selection 50 kHz off ring centre; capture_classify answers that band
// ops.rs
#[tokio::test] async fn iq_read_writes_into_the_burst_store()                   // answer.capture Some(id); capture_list holds it
// capture_decode.rs
#[tokio::test] async fn capture_decode_replays_a_ring_slice()                   // aprs_packet_iq in the ring, write_ring_slice, capture_decode aprs_decode -> context.replay.events >= 1
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --lib bursts` and `--test capture_ops --test ops --test capture_decode` with the names. Expected: FAIL.
- [ ] **Step 3: Implement.** `write` uses `file_center_hz.unwrap_or(rec.center_hz)` for the file's meta. The `--bursts-trial-mb` flag maps to `trial_keep_bytes`.
- [ ] **Step 4: Run** the step 2 commands plus the existing bursts, `iq_read` and replay tests. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: a ring slice into the burst store under a trial quota; iq_read shares the path and its recordings expire`.

### Task 8: The choice policy

**Files:**
- Create: `crates/zeta-core/src/choice_policy.rs`.
- Modify:
  - `crates/zeta-core/src/ops.rs` (`ChoicePolicySet`, `ChoicePolicyGet` beside `HandlerPolicySet` :1105; `OpResult::ChoicePolicy`; names; `read_only` list :4210);
  - `crates/zeta-core/src/node/persist.rs` (beside `HANDLER_POLICY_FILE` :33);
  - `crates/zeta-core/src/node/mod.rs` (`Inner.choice_policy`, loaded at open beside :1558);
  - `crates/zeta/src/cli.rs` (`zeta choice policy get|set`, beside `HandlersCmd::Policy` :948).
- Test: `crates/zeta-core/tests/choice_policy.rs` (new), `crates/zeta-core/tests/ops.rs` (variant count 56 to 58).

**Interfaces:**
- Produces:

```rust
#[serde(rename_all = "snake_case")] pub enum Surface { Menu, Watch, Agent, Client }
pub struct Ceiling { pub default: u8, pub cap: u8 }                       // 1..=3, default <= cap
#[serde(rename_all = "snake_case")] pub enum LeaseSetting { Share, Widen }
pub struct Budget { pub trial_s: u32, pub concurrent: u32, pub slice_s: f32 }
pub struct ChoicePolicy { pub surfaces: BTreeMap<Surface, Ceiling>, pub lease: LeaseSetting,
    pub budget: Budget, #[serde(default)] pub confidence_stop: Option<f32> }  // Default = Global Constraints values
pub fn surface_of(transport: &str) -> Surface                              // "ui" Menu, "agent" Agent, else Client
impl ChoicePolicy { pub fn steps(&self, s: Surface, asked: Option<u8>) -> (u8, Option<String>) } // (step, clamp note "not tried: capped at step <n> for <surface>")
```

- `Op::ChoicePolicySet { policy: ChoicePolicy }` replaces the whole value. `Op::ChoicePolicyGet {}`.
- The file is `choice-policy.json`, loaded with `load_choice_policy(state_dir) -> (ChoicePolicy, Option<String>)` and saved with `persist_choice_policy`.
- `Node::choice_policy()` and `set_choice_policy(p) -> Result<ChoicePolicy, NodeError>`: persist first, then swap, as for the handler policy.

- [ ] **Step 1: Write the failing tests.**

```rust
#[tokio::test] async fn the_default_policy_is_the_specs()          // menu {1,2}, watch {3,3}, agent {1,3}, client {1,3}, share, budget {30,3,2.0}
#[tokio::test] async fn a_set_policy_survives_a_restart()
#[tokio::test] async fn a_default_above_its_cap_is_refused()
#[test] fn an_ask_above_the_cap_is_clamped_and_said()                // steps(Menu, Some(3)) == (2, Some("not tried: capped at step 2 for menu"))
#[test] fn the_surface_follows_the_transport()                       // ui->Menu, agent->Agent, local/http/cli->Client
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test choice_policy` and the `tests/ops.rs` variant test. Expected: FAIL.
- [ ] **Step 3: Implement**, including the CLI subcommand. `set` takes `--file <json>`, and `--lease share|widen` for the common change.
- [ ] **Step 4: Run** the step 2 commands. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: the choice policy, per-surface default and cap, lease setting and budget`.

### Task 9: The `choose` controller job, steps 1 and 2

**Files:**
- Create: `crates/zeta-core/src/node/choose.rs`.
- Modify:
  - `crates/zeta-core/src/ops.rs` (`JobKind::Choose`, `BUILTIN_KINDS` 20, `name` "choose", an empty graph like Identify :2319; `JobStart` gains `#[serde(default)] choose: Option<ChooseParams>`);
  - `crates/zeta-core/src/node/launch.rs` (dispatch beside Identify :184);
  - `crates/zeta-core/src/node/identify.rs` (make `await_proof` :437 `pub(super)`).
- Test: `crates/zeta-core/tests/choose.rs` (new).

**Interfaces:**
- Consumes:
  - `classify_samples` (Task 4);
  - `choose_now` (Task 6);
  - `write_ring_slice` (Task 7);
  - `ChoicePolicy::steps`, `surface_of`, `Budget` (Task 8);
  - `Node::capture_decode(&self, capture, kind, handler, packets) -> Result<JobId, NodeError>` (node/mod.rs:2979);
  - identify's `await_proof`.
- Produces:
  - `pub struct ChooseParams { #[serde(default)] pub up_to: Option<u8>, #[serde(default)] pub surface: Option<Surface>, #[serde(default)] pub start: bool, #[serde(default)] pub transient: bool }`.
  - `pub(super) async fn launch_choose(self: &Arc<Self>, rec: JobRecord, port: Arc<dyn Port>, gate: Option<&OwnedMutexGuard<()>>, after: &mut AfterGate) -> Result<(), NodeError>`.
  - It takes no lease for steps 1 and 2.
  - Its selection is the job's `center_hz`/`bandwidth_hz` over the newest `budget.slice_s` seconds of the ring: what the ring holds, if at least 1 s; else step 2 is not tried with the ring string.
  - Its event stream carries `{"protocol": "choice", "step": n, "choice": Choice}` at start and at every change, then `{"protocol": "verdict", "choice": Choice}`.
  - The context holds `choose.choice` (latest), then `choose.verdict`.
- Rules:
  - Step 2 writes the slice (`trial: true`, pinned while the job runs, unpinned at its end) and replays the `could_prove` rows, at most `budget.concurrent` at once, each `capture_decode` paced as it is now.
  - Each replay's events are judged by the row's identify rule through `await_proof`, within `budget.trial_s` overall.
  - The first proof settles the job: `settled: proved`, `step: 2`.
  - Replays still running are stopped when the job settles or ends.
  - With `start: true` the job starts the final `choice.job` (a `job_start` of that kind, centre, width and params on the job's source) and ends.
  - Without `start: true` it starts nothing. It ends `stopped`, never `failed`, for a `cannot_tell`.

- [ ] **Step 1: Write the failing tests** in `tests/choose.rs`, over a 10 s ring (`cfg.ring_seconds = 10.0`):

```rust
#[tokio::test] async fn choose_revises_fm_to_dmr_at_step_two()           // dmr_iq in the ring; up_to 2: events choice{step1, fm_demod} then choice{step2, dmr_decode, proved}; verdict dmr_decode
#[tokio::test] async fn choose_on_voice_stays_fm_demod()                 // nbfm speech: verdict fm_demod best_guess, step 2 tried every could_prove row
#[tokio::test] async fn choose_up_to_one_never_writes_a_slice()          // up_to 1: capture_list has no trial record
#[tokio::test] async fn a_short_ring_skips_step_two_and_says_so()        // ring_seconds 0.5: not_tried has "not tried: the ring holds under 1 s"
#[tokio::test] async fn start_true_starts_the_verdict_and_ends()         // dmr: a running dmr_decode at the job's centre; the choose job stopped
#[tokio::test] async fn a_trial_slice_is_unpinned_when_choose_ends()
#[tokio::test] async fn choose_holds_no_lease_and_stops_no_job()         // beside a rigid fm_demod on the band: the rigid job keeps running, lease holders unchanged
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test choose`. Expected: FAIL.
- [ ] **Step 3: Implement** `launch_choose` and the step-2 trial engine. The engine runs each trial as a spawned task under the job's cancel token. Use `JoinSet` for the concurrency bound.
- [ ] **Step 4: Run** `--test choose`, plus `--test identify` and `--test capture_decode`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: the choose job: step 1 at once, step 2 replays the ring slice through the decoders that could prove otherwise`.

### Task 10: `choose` step 3, the live trial, under `share` and `widen`

**Files:**
- Modify:
  - `crates/zeta-core/src/node/choose.rs`;
  - `crates/zeta-core/src/node/identify.rs` (extract the per-row trial: `start_child` :355 plus `await_proof`, as `pub(super) async fn trial_row(self: &Arc<Self>, r: &Run, mode: &LoadedMode, tuning: Tuning) -> Result<Option<Value>, String>`, which identify's loop then calls);
  - `crates/zeta-core/src/lease.rs` (`pub fn covers_untuned(&self, source: &SourceId, band: Tuning) -> bool`: true when the source's current capture contains the band and starting a job there needs no retune).
- Test: `crates/zeta-core/tests/choose.rs`, `crates/zeta-core/tests/identify.rs` (unchanged).

**Interfaces:**
- Consumes: Task 9, `ChoicePolicy.lease`.
- Rules:
  - Step 3 runs the `could_prove` rows step 2 did not prove, in score order, one at a time, each at identify's trial tuning (`trial_tuning` :779), within what is left of `budget.trial_s`.
  - Under `share` a row is tried only when `covers_untuned` holds, else `not tried: no running capture covers it`.
  - Under `widen` it may grow a held capture through the path `job_start` uses to widen a held capture (in `lease.rs`), never moving any holder's band, and is skipped with the same string when that path refuses.
  - A proved child is kept only with `start: true` and only if it is the verdict. Every other child is stopped.

- [ ] **Step 1: Write the failing tests.**

```rust
#[tokio::test] async fn step_three_proves_on_the_live_band_under_share()   // dmr in a ring too short for step 2 (budget.slice_s 0.5): verdict at step 3, dmr_decode
#[tokio::test] async fn share_skips_a_band_no_capture_covers()            // selection at the capture's edge needing a retune: not_tried "no running capture covers it", no tune op in ops.log
#[tokio::test] async fn widen_grows_a_held_capture_and_keeps_every_holder()
#[tokio::test] async fn a_step_three_child_is_stopped_unless_started()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test choose -- step_three share widen child`. Expected: FAIL.
- [ ] **Step 3: Implement** `covers_untuned`, `trial_row` (identify's loop uses it with no change in behaviour), and step 3.
- [ ] **Step 4: Run** `--test choose --test identify --test lease`. Expected: PASS, with identify unchanged.
- [ ] **Step 5: Commit** `Choice: step 3 trials the live band, sharing a running capture or widening a held one`.

### Task 11: Transient jobs

**Files:**
- Modify:
  - `crates/zeta-core/src/job.rs` (`JobRecord`; the history drop at :68-74);
  - `crates/zeta-core/src/node/recover.rs` (`resumable` :319);
  - `crates/zeta-core/src/ops.rs` (`JobList { #[serde(default)] all: bool }` :737);
  - the zeta CLI (`job list --all`);
  - every `JobRecord { .. }` literal the compiler names (`transient: false`).
- Test: `crates/zeta-core/tests/choose.rs`, `crates/zeta-core/tests/jobs.rs`.

**Interfaces:**
- Produces: `JobRecord.transient: bool` (`#[serde(default, skip_serializing_if = "std::ops::Not::not")]`), set from `ChooseParams.transient`.
- Rules:
  - A transient record is never resumed.
  - It is dropped from history when it stops.
  - `job_list` leaves it out unless `all: true`.
  - It is still in `ops.log`.

- [ ] **Step 1: Write the failing tests.**

```rust
#[tokio::test] async fn a_transient_choose_is_listed_only_with_all()
#[tokio::test] async fn a_transient_choose_does_not_resume()               // restart the node mid-job: no record afterwards
#[tokio::test] async fn a_stopped_transient_choose_leaves_no_history()
```

- [ ] **Step 2: Run** them. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test choose --test jobs` and `cargo test -p zeta --test cli`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: transient jobs, never resumed nor kept, listed with --all`.

### Task 12: The page, the first entry and its revision

**Files:**
- Modify:
  - `crates/zeta-ui/web/actions.js` (`GROUPS.box` :322: a first group `{id: "choice", direct: true}`; an entry `choice_start`; Listen ordered with the chosen demodulator first);
  - `crates/zeta-ui/web/menu.js` (`boxCtx` :787 gains `choice`; on `open()` it asks `classify` quietly through `this.ov.api.op` and repaints, as `modeRows` does at :1115-1130; then it starts a transient `choose` with `{surface: "menu", up_to: 2, transient: true}`, reads its record's `context.choose.choice` every 500 ms, repaints on change, and `job_stop`s it on close or reopen; `paint` :855 sorts the `listen` group).
- Test: `crates/zeta-ui/tests/e2e/ui.py`; `crates/zeta/tests/ui.rs` (a `cbvoice` fixture: `nbfm_modulate` of `speech_like`, keyed on for 2 s and off for 2 s, 2500 Hz deviation, at a `uhf_cb_au` channel centre + 700 Hz, 48 kHz, through `write_fixture`).

**Interfaces:**
- Consumes: the classify `choice` (Task 6), `ChooseParams` (Task 9), `transient` (Task 11).
- Labels (exact):
  - `Start <kind> here: <width in kHz> kHz <row label>`, with ` (not proved)` added for a `best_guess` decoder;
  - `Can't tell: <reason>`;
  - hover title `runner-up: <kind> <width> kHz`;
  - while step 2 runs, the entry carries the class `revising`;
  - a revision replaces the label in place.
- `run` starts `choice.job` through `Menu.startJob`.

- [ ] **Step 1: Write the failing scenarios.**

```python
def scenario_the_first_box_entry_is_the_choice(page, url, secret)        # cbvoice box: #ctx first item text starts "Start fm_demod here:"
def scenario_the_choice_entry_starts_its_job(page, url, secret)          # click: running fm_demod with deviation 2500, centre within 300 Hz of the fixture's
def scenario_the_choice_entry_revises_to_dmr(page, url, secret)          # dmr fixture: label becomes "Start dmr_decode here:" within 15 s
def scenario_closing_the_menu_stops_its_choose(page, url, secret)        # after close, job_list all=true has no choose within 3 s
def scenario_listen_puts_the_chosen_demodulator_first(page, url, secret)
```

- [ ] **Step 2: Run** `ZETA_UI_ONLY=the_first_box_entry_is_the_choice,the_choice_entry_starts_its_job,the_choice_entry_revises_to_dmr,closing_the_menu_stops_its_choose,listen_puts_the_chosen_demodulator_first cargo test -p zeta --test ui -- --nocapture`. Expected: FAIL (and check that no `ui e2e skipped` line is printed).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the step 2 command, plus `menu_entry_table_is_whole` and `what_is_this_classifies`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: the box menu's first entry is the node's choice, revised in place by a transient choose`.

### Task 13: `held` jobs: the mix skips them and an attached transcriber buffers

**Files:**
- Modify:
  - `crates/zeta-core/src/job.rs` (`JobRecord.held`);
  - `crates/zeta-core/src/node/mod.rs` (`mixable` :499; `pub fn set_held(&self, job: &JobId, held: Option<Hold>) -> Result<(), NodeError>`, which persists);
  - `crates/zeta-core/src/stage.rs` (`StageContext.held: Option<HeldHook>`);
  - `crates/zeta-core/src/node/launch.rs` (build the hook beside `with_mix_inputs` :279-299, reading the record of `rec.input`);
  - `crates/zeta-speech/src/transcribe.rs` (buffer in `Worker::publish` :305 while the hook says held; publish in order on release; drop on cancel, with no flush);
  - every `JobRecord { .. }` literal (`held: None`).
- Test: `crates/zeta-core/tests/voices.rs` or `tests/mix.rs` (wherever mix inputs are tested), `crates/zeta-speech` tests (the transcribe stand-in backend).

**Interfaces:**
- Produces:
  - `pub struct Hold { pub reason: String, pub since_ns: u64 }`.
  - `JobRecord.held: Option<Hold>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`).
  - `pub type HeldHook = Arc<dyn Fn() -> bool + Send + Sync>`.
- Rules:
  - `mixable` is false for a held record, so a hold lifted is mixed within one poll (500 ms) with no mix restart.
  - The transcriber checks the hook before each publish. Released lines keep their own `start_ns`/`end_ns`.
  - A transcribe job cancelled while held publishes nothing it held.

- [ ] **Step 1: Write the failing tests.**

```rust
#[tokio::test] async fn a_held_job_is_not_mixed_and_joins_when_lifted()   // mix_inputs excludes it; set_held(None) -> included (poll until)
#[tokio::test] async fn a_held_transcriber_publishes_its_lines_on_release_in_order()
#[tokio::test] async fn a_held_transcriber_cancelled_publishes_nothing()
#[tokio::test] async fn held_survives_a_restart_on_the_record()
```

- [ ] **Step 2: Run** them. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the step 2 tests, then `cargo test -p zeta-speech` and the mix tests. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: held jobs: out of the mix, an attached transcriber buffering until the hold lifts`.

### Task 14: The watch starts the choice on each signal, on probation

**Files:**
- Modify:
  - `crates/zeta-dsp/src/detect.rs` (`DetectParams` :37 and `Default` :179: `on_signal`, `attach`, `probation`);
  - `crates/zeta-core/src/node/watch.rs`:
    - `Policy` :372, `Policy::of` :413;
    - `check_modes_list` :532, which accepts demodulator rows;
    - `next()` :2635: an `on_signal: choose` branch;
    - `Decoding` :703 gains `attached`, `held` and `choose`;
    - `decoders()` :2467: a demodulator row's liveness is the time of its last `squelch` `open: true`;
    - `release()` :926 and `watch_loop` :1778 stop the attached jobs too;
  - `crates/zeta-core/src/ops.rs` (make `attach_one` :8022 `pub(crate)`).
- Test: `crates/zeta-core/tests/watch.rs`, the UI scenario in `ui.py`.

**Interfaces:**
- Consumes:
  - `choose_now` through `classify_samples` (Tasks 4, 6);
  - the `choose` job (Tasks 9, 10), started with `owner: Some(watch)`, `surface: watch`, `transient: true`;
  - `set_held` (Task 13);
  - `attach_one`;
  - `start_on` with the plan deviation (Task 2).
- Produces:
  - `#[serde(rename_all = "snake_case")] pub enum OnSignal { #[default] Identify, Choose }`;
  - `attach: Vec<String>` (only `transcribe` accepted);
  - `probation: bool` (default true).
- Rules (spec 5.5):
  - On a new signal: answer step 1, then start its job. When the answer is an unproved demodulator and `could_prove` is not empty, start it held (`Hold { reason: "probation" }`).
  - Attach the `attach` kinds, then start the `choose` child.
  - On the child's verdict:
    - if it names the started row, lift the hold;
    - otherwise stop the attached transcriber first, then the held job, start the verdict's row, and attach the transcriber to the row's voice when it has one.
  - A held job with no running `choose` after a restart has its hold lifted.
  - Stopping a gone signal: the signal is gone and no `squelch` `open: true` has arrived for `hold_ms`. The attached jobs are stopped first.

- [ ] **Step 1: Write the failing tests** in `tests/watch.rs`:

```rust
#[tokio::test] async fn a_choose_watch_starts_fm_demod_and_a_transcriber_on_voice()   // cbvoice-like ring: fm_demod running (deviation 2500) + an attached transcribe
#[tokio::test] async fn a_voice_signal_is_held_then_joins_the_mix()                    // held at start (could_prove non-empty), held None after the verdict; mix_inputs then include it
#[tokio::test] async fn a_dmr_signal_never_reaches_the_mix_and_becomes_dmr_decode()     // fm_demod held and never in mix_inputs; then dmr_decode running, fm_demod stopped, no transcript line from the held job
#[tokio::test] async fn a_signal_nothing_could_prove_starts_unheld()                    // am carrier: am_demod held None from the start
#[tokio::test] async fn a_gone_voice_signal_is_stopped_after_the_squelch_hold()          // demod and transcriber stopped, transcriber first
#[tokio::test] async fn a_restart_lifts_a_hold_whose_choose_is_gone()
#[tokio::test] async fn check_modes_list_accepts_a_demodulator_row()
```

Add the scenario `def scenario_a_choose_watch_lists_held_then_live(page, url, secret)` in `ui.py`: over the `cbvoice` fixture, the job row shows `held` first and then joins the mix.

- [ ] **Step 2: Run** `cargo test -p zeta-core --features test-fixtures --test watch -- choose held dmr unheld squelch_hold restart_lifts demodulator_row`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test watch` (the whole file: no existing test changes), plus the UI scenario with `ZETA_UI_ONLY`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: a watch starts the node's choice on each signal, on probation until its choose settles`.

### Task 15: The training record on a settled trial slice

**Files:**
- Modify: `crates/zeta-core/src/bursts.rs` (`CaptureRecord.training: Option<Value>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`; `pub fn annotate(&self, id: &str, training: Value) -> Result<(), StoreError>`), `crates/zeta-core/src/node/choose.rs`.
- Test: `crates/zeta-core/tests/choose.rs`.

**Interfaces:**
- Produces: `training = {"sources": [{"source": "layer1"|"catalogue"|"learned"|"layer3", "guess": <row id or label>, "confidence": f}], "settled": {"row", "step", "settled"}}`. It is written when a `choose` settles at step 2 or 3.
- Rules:
  - A slice carrying a training record is kept under the trial quota and expires with it.
  - A slice with no record expires as Task 7 says.

- [ ] **Step 1: Write the failing test:** `#[tokio::test] async fn a_slice_settled_at_step_two_carries_its_training_record()` (dmr: `training.settled.row == "dmr_decode"`, and `sources` holds the layer-1 `fm` guess).
- [ ] **Step 2: Run** it. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `--test choose`. Expected: PASS.
- [ ] **Step 5: Commit** `Choice: a settled trial slice keeps a training record of each source's guess and the answer`.

### Task 16: Docs, UAT, TODO, and the whole-branch gates

**Files:**
- Modify:
  - `crates/zeta/src/agents_md.rs` (notes for `choose`, `choice_policy_*`, the watch's `on_signal`/`attach`/`probation`, `held`, `transient`, `job_list.all`, `iq_read.capture`);
  - `docs/agents/field-craft.md`;
  - `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated);
  - `UAT.md` (a "Plan 48" section);
  - `TODO.md`;
  - `.github/workflows/ci.yml` (`SMOKE` gains `the_first_box_entry_is_the_choice`).
- `UAT.md`, plan 48, checks to add:
  - 48.1: the first entry on live FM voice, on a pager and on a broadcast station, each with its width and deviation;
  - 48.2: a watch's choice on a live voice band: held, then mixed, then transcribed;
  - 48.3: a live digital voice signal revised from `fm_demod` to its decoder;
  - 48.4: `zeta choice policy set --lease widen` beside a running job, with no holder moved;
  - 48.5: the trial quota under a 20 MS/s selection.
- `TODO.md`:
  - close "Classify: layers 1 and 2 choose the job", the voice case of "Watch: start a voice demodulator and a transcriber on each new signal", and "Audio: the default `deviation_hz` on a 12.5 kHz channel" (done by Task 2), each with a line saying where;
  - add one-line gists for what is left: a released transcript line draws on the waterfall when it arrives, not at its own time; plan 48.5 (borrow); the layer-3 model; replaying a capture into a demodulator to listen to it.

- [ ] **Step 1: Write the docs and regenerate.** Then run `cargo run -q -p zeta -- agents-md --check` and `--skill --check`. Expected: no drift.
- [ ] **Step 2: Run the gates:**
  - `cargo fmt -p <crate> -- --check` for each changed crate;
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
  - `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`;
  - `cargo test -p zeta --test ui -- --nocapture` (the whole suite; check that no `ui e2e skipped` line is printed);
  - `bash scripts/public-check.sh`;
  - `bash scripts/check-doc-links.sh`.

  Expected: all clean.
- [ ] **Step 3: Commit** `Choice: docs, UAT plan 48, TODO, and the smoke set`.
