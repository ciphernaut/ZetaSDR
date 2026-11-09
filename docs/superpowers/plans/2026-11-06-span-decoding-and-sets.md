# Plan 31: Span decoding and named sets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `watch` with a `modes` list decodes every channel of those modes the detector finds in its band (one ordinary decoder per signal, no identify run when one row fits, a cap, a hold and a cooldown), and a named set (`sets/*.json`, `set_start`, `set_list`, `set_stop`) starts jobs across several radios as one unit under one lease decision per source; the page folds a watch's decoders and a set's jobs under group rows, and the pager row gets its Subjects table and the access marker a later ACL gates on.

**Architecture:** Watch policy fields on the `detect` node's params (`zeta-dsp` `DetectParams`, read by `Policy::of` in `crates/zeta-core/src/node/watch.rs`) drive a direct-start path through the existing `start_on`, a candidate filter handed to the identify child through its record's context, and three new per-run state lists (`known`, `cooled`, the cap). Sets are a sibling of the mode registry (`crates/zeta-core/src/sets.rs`: embedded files plus an operator directory, broken files kept with their reason), three ops in `ops.rs` built from `plan_start`'s discipline split into gated pieces, a side-effect-free `LeaseManager::decide`, and an instance file per set under the state directory. The page reuses the plan group row's DOM (`jobGroup`, `placeChildren`) under a membership predicate, gives the selection menu two entries, and merges a watch's decoders into one Subjects section. Nothing here starts trunking, `vdl2_decode` (plan 30), recipes or an ACL.

**Tech Stack:** Rust (tokio, serde, schemars; `zeta_dsp::synth::pocsag_packet_iq` and `gfsk_modulate` for fixtures), vanilla JS page, the Playwright e2e suite in `crates/zeta-ui/tests/e2e/ui.py`.

**Spec:** `docs/superpowers/specs/2026-11-06-span-decoding-and-sets-design.md` (all of it; section 8 holds the operator's decisions of 2026-11-06, section 9 the calls, section 10 what this plan settles). Sibling: `docs/superpowers/specs/2026-11-06-aircraft-links-design.md` (plan 30) writes `vdl2_decode`; this plan ships the `airband` set with that member and works without the row (Task 9).

## Global Constraints

- Every op field is required and unknown fields are rejected (`deny_unknown_fields` on `Op`); a nullable op field uses `#[serde(deserialize_with = "crate::modes::nullable")]` plus `#[schemars(required, schema_with = "crate::types::nullable::<T>")]` as `PlanStart.channels` does (`crates/zeta-core/src/ops.rs`). A new field on an existing op is forbidden (spec 9.1): the watch's policy travels on the `detect` node's params.
- `DetectParams` is `#[serde(default, deny_unknown_fields)]` with a `Default` impl: every new field gets a default there (`stage_params_schemas_are_strict` in `crates/zeta-dsp/src/lib.rs` must keep passing), so every stored watch graph and the e2e suite's `graph_put` of the old fields load unchanged.
- `JobRecord` gains no field (spec 4.6, 9.21); `ModeRow` gains `access` and `pocsag_decode` gains `subject`, both `#[serde(default)]` so every row and operator file keeps loading.
- Numbers from the spec are named constants, never literals in logic: `WATCH_MAX_DECODERS_DEFAULT = 16`, `WATCH_HOLD_MS_DEFAULT = SILENCE_MS` (120 000), `WATCH_COOLDOWN_MS_DEFAULT = 30_000`, `WATCH_CONFIRM_MS_DEFAULT = 0`, `WIDTH_FIT = (0.5, 1.5)` (today's literal in `identify_band`), `SETS_INSTANCE_FORMAT = 1`; `pagers_au`'s params are data in `sets/pagers_au.json` (`persist_ms` 500, `width_hz` [6000, 20000], `gone_ms` 60000, `hold_ms` 600000, `cooldown_ms` 120000, `confirm_ms` 180000).
- The field craft: a watch never retunes, widens or pins the radio; a decoder sits on the detector's centroid, never on a raster or a register centre; nothing is stopped or replaced unless the request named it; a `conflict` names the holders and starts nothing anywhere.
- Every asynchronous read in a test polls with the suite's `until` (the CI-flake rule); the hosted runner has 2 CPUs.
- The node runs, and every test passes, with no sets directory, no station, no network and no `dump1090`/`dumpvdl2`.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: after any op, flag, stage-param or field-craft change run `cargo run -q -p zeta -- agents-md --check`, regenerate (`cargo run -q -p zeta -- agents-md > AGENTS.md`, `cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and commit the result in the same commit. Never edit either by hand. The prose for a new op goes in `crates/zeta/src/agents_md.rs` (the `match name` of per-op notes, and `RULES`) and in `docs/agents/field-craft.md`.
- Gates (CLAUDE.md): `cargo fmt -p <crate> -- --check` per crate (the nested worktree breaks `cargo fmt --all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped` line), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Use a private `CARGO_TARGET_DIR`. The watch suite needs `--features test-fixtures`.
- The repository is public: roles, never private host names or LAN addresses, in code, tests, docs, set files and commit messages ("the field host"; `b210` and `hackrf` are the roles `RECIPES.md` uses). The demo station is Brisbane Airport (-27.3842, 153.1175, 4 m). No off-air recording as a fixture: every fixture is synthesised.
- Commits go on this branch; one commit per task, message in the repo's style (`Area: what changed`), no trailer.

## Models per task

Opus where the task changes the watch loop, the lease manager or the launch path, or defines an interface several later tasks consume; Sonnet where the brief is complete and the tests pin the behaviour.

| Task | Model | Why |
|---|---|---|
| 1 watch policy params, validation, `hold_ms`, `why`, `via` | Opus | the fields every later watch task reads; the validation path |
| 2 the direct start and the restricted identify | Opus | the loop's new path through `start_on`; the identify child's filter |
| 3 cap, hold, cooldown, confirm, re-adoption | Opus | timing against synthesised signals |
| 4 known channels and `band_assignments` | Opus | a new knowledge query feeding the ranking |
| 5 the pager row's `subject` and the `access` marker | Sonnet | a row edit, one field, one envelope line |
| 6 set records and `set_list` | Opus | the record every later set task loads |
| 7 `LeaseManager::decide` and `plan_start` in gated pieces | Opus | the lease manager and a refactor of a shipped op |
| 8 `set_start` and `set_stop` | Opus | two gates at once, the instance, the restart |
| 9 members: `null` radios, watch members, unready rows and attaches | Opus | the airband set without plan 30 |
| 10 the agent's gate and own sets | Sonnet | two lists and a record |
| 11 the page: decode across, the watch fold, the merged Subjects | Opus | three modules and an e2e scenario |
| 12 the page: start set, the set fold, nesting | Opus | a picker, a second poll, nested groups, an e2e scenario |
| 13 field craft, per-op notes, RECIPES, README | Sonnet | |
| 14 TODO, UAT, plan series, gate | Sonnet | |

Ledger: `.superpowers/sdd/plan31/progress.md` in the worktree, one line per task with the implementer's and reviewer's verdicts; the rulings made on the operator's behalf go into the final report.

## Review Focus

Inputs the spec implies but no obvious test exercises; each gets a test in the task that owns the code:

1. A pager carrier that pages twice in one detector cycle must not get two decoders: the second `new` overlapping a kept decoder's channel updates it, never starts another. (Task 2: `a_second_new_on_a_decoded_channel_starts_nothing`.)
2. `jobs_stop` of the decoders' kind on the watch's source (the operator clearing the band by hand) must free the watch's slots and let the next signal start, never leave the cap counting dead jobs. (Task 3: `jobs_stop_of_the_decoders_frees_the_cap`.)
3. A set file whose watch member's band is narrower than one of its listed rows must be `broken` at load naming `members[<i>].params.modes[<j>]`, not start a watch that refuses every signal. (Task 6: `a_watch_member_narrower_than_its_row_is_broken_at_load`.)
4. A `set_start` whose `radios` maps two roles to the same source must start when the two bounding bands fit one capture of that source and be `other` naming both roles when they do not; it must never tune the source twice. (Task 8: `two_roles_on_one_source_share_one_capture_or_are_refused`.)
5. A `set_stop` while a member's job is already `stopped` by hand and another is `failed` must stop the rest, list the two as `ended`, drop the instance and fail nothing. (Task 8: `set_stop_with_ended_members_stops_the_rest`.)

---

### Task 1: Watch policy params, their validation, `hold_ms`, `why` and `via`

**Files:**
- Modify: `crates/zeta-dsp/src/detect.rs` (`DetectParams`, its `Default`, `check`), `crates/zeta-core/src/node/watch.rs` (`Policy`, `Policy::of`, `Decoding`, `State::context`, `watch_event`, `decoders`, `verdict`), `crates/zeta-core/src/node/mod.rs` (`check_prefer` becomes `check_watch_policy`), `crates/zeta-core/src/ops.rs` (`DETECT_STAGE` constants block: the four `WATCH_*` defaults)
- Test: `crates/zeta-core/tests/watch.rs`, `crates/zeta-dsp/src/detect.rs` tests, `crates/zeta-dsp/src/lib.rs` (`stage_params_schemas_are_strict`, unchanged, must pass)

**Interfaces:**
- Consumes: `DetectParams` (`#[serde(default, deny_unknown_fields)]`), `Policy::of(&Graph)`, `SILENCE_MS`, `silence_ms()` (the `test_timing` override), `Node::check_prefer` (called from `job_start_gated` and `job_graph_set`), `watch_event(&Run, state, summary, Event)`.
- Produces: in `ops.rs` `pub const WATCH_MAX_DECODERS_DEFAULT: u32 = 16; pub const WATCH_HOLD_MS_DEFAULT: u64 = 120_000; pub const WATCH_COOLDOWN_MS_DEFAULT: u64 = 30_000; pub const WATCH_CONFIRM_MS_DEFAULT: u64 = 0;` (`SILENCE_MS` in `watch.rs` becomes `pub use`-level equal to `WATCH_HOLD_MS_DEFAULT`). In `DetectParams`: `pub modes: Option<Vec<String>>` (watch policy, ignored by the stage: `null` lets identify decide, a non-empty list of mode row ids decodes those directly), `pub max_decoders: u32`, `pub hold_ms: u64`, `pub cooldown_ms: u64`, `pub confirm_ms: u64`, `pub seed_known: bool`, each with the doc comment "Watch policy, ignored by the stage: ..." as the existing ones (these comments are the AGENTS.md stage text); `check` adds `modes: empty list (null lets identify decide)` and `max_decoders: at least 1`. In `watch.rs`: `Policy { modes: Option<Vec<String>>, max_decoders: u32, hold_ms: u64, cooldown_ms: u64, confirm_ms: u64, seed_known: bool, ..today's }` where `hold_ms` reads the param when present in the node's JSON, else `silence_ms()` (so `test_timing` still drives the default); `#[derive(Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "snake_case")] pub enum Via { Identify, Modes }`; `Decoding.via: Via` (`#[serde(default = "Via::identify")]` so persisted contexts load); `Event.via: Option<Via>` and `Event.why: Option<&str>`, both `null` when absent, written by `watch_event` as `via` and `why`; the `lost` events of `decoders()` carry `why: "gone and silent" | "not seen again"` (today's summary text, now a field too); `State::context` adds `"cap": {"max": policy.max_decoders, "decoding": decoding.len(), "waiting": 0}` (waiting counts from Task 3). `Node::check_watch_policy(&self, graph: &Graph, tuning: Tuning) -> Result<(), NodeError>`: today's `prefer` check plus, for a `detect` node with non-null `modes`: `modes[<i>]: unknown mode <id>` (not a loaded row), `modes[<i>]: <id> is <broken|unavailable> (<reason>)`, `modes[<i>]: <id> is <w> Hz wide; the watch's band is <b> Hz` (`band.bandwidth_hz > tuning.bandwidth_hz`), `modes: empty list (null lets identify decide)`, each a `GraphError::BadParams { node, reason }` as `prefer[<i>]` is; called where `check_prefer` is called today (`job_start_gated`, `job_graph_set`), with the record's tuning.

- [ ] **Step 1: Write the failing tests** (`watch.rs` suite: `start_watch(node, params)` and `watch_graph` already build a watch from `detect` params)

```rust
// crates/zeta-core/tests/watch.rs
#[tokio::test] async fn graph_put_with_a_bad_modes_list_is_refused() { /* a watch on a 100 kHz band; graph_put with modes ["no_such_mode"] -> Err containing "modes[0]: unknown mode no_such_mode"; with a quick_modes row made broken (its bandwidth_hz "x") -> "modes[0]: <id> is broken"; modes ["rds_decode"] -> "modes[0]: rds_decode is 200000 Hz wide; the watch's band is 100000 Hz"; modes [] -> "modes: empty list"; after each the stored graph is unchanged (graph_get) and the watch still runs */ }
#[tokio::test] async fn a_lost_event_carries_why_and_a_decoding_event_carries_via() { /* a_lost_signal_stops_its_decoder's shape: the decoding event has via "identify", the lost event has why "gone and silent" */ }
#[tokio::test] async fn hold_ms_on_the_detect_node_replaces_the_silence_constant() { /* test_timing(…, silence 60_000) but hold_ms 1500 on the node: the decoder is lost ~1.5 s after its signal goes (until, within 6 s); job_context.watch.cap reads {max: 16, decoding: 0, waiting: 0} after */ }
// crates/zeta-dsp/src/detect.rs
#[test] fn the_watch_policy_defaults_are_the_spec_values() { let p = DetectParams::default(); assert_eq!((p.modes, p.max_decoders, p.hold_ms, p.cooldown_ms, p.confirm_ms, p.seed_known), (None, 16, 120_000, 30_000, 0, false)); }
#[test] fn check_refuses_an_empty_modes_list_and_a_zero_cap() { /* modes Some(vec![]) -> Err containing "modes: empty list"; max_decoders 0 -> Err containing "max_decoders" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp detect::` and `cargo test -p zeta-core --features test-fixtures --test watch graph_put_with_a_bad_modes_list_is_refused` — Expected: FAIL to compile (`modes`, `max_decoders`).
- [ ] **Step 3: Implement** the fields, the constants, `Policy`, `Via`, `why`, `cap`, `check_watch_policy` (rename `check_prefer`, keep its test `a_watch_preferring_an_unknown_source_is_refused` green).
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp`, `cargo test -p zeta-core --features test-fixtures --test watch`, `cargo run -q -p zeta -- agents-md --check` (regenerate: the `detect` stage's params changed) — Expected: PASS; the whole watch suite unchanged.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp crates/zeta-core AGENTS.md skills
git commit -m "Watch: modes, max_decoders, hold_ms, cooldown_ms, confirm_ms and seed_known on the detect node, checked at start and graph_put; lost events carry why, decoding events via"
```

---

### Task 2: The direct start and the restricted identify

**Files:**
- Modify: `crates/zeta-core/src/node/watch.rs` (`Tracked`, `signals`, `next`, `verdict`, `start_identify`, a new `start_direct`), `crates/zeta-core/src/node/identify.rs` (`Run.candidates`, `launch_identify`, the scoring loop)
- Test: `crates/zeta-core/tests/watch.rs` (new fixtures `pocsag3`, `pocsag_fsk`)

**Interfaces:**
- Consumes: `Policy.modes` (Task 1), `Inner::start_on(source, mode, tuning, sondehub) -> Result<(JobId, SinkOutcome), String>` (unchanged: aimed at `r.prov.source`, it leases inside the watch's capture without a tune), `identify_band`, `held_elsewhere`, `score_rows`, `JobKind::Identify.expand`, `Inner::candidate`.
- Produces: `Tracked.fits: Vec<String>` (the listed rows whose `band.bandwidth_hz` the signal's width fits by `WIDTH_FIT` and that fit the watch's band; empty with `modes: null`) and `Tracked.waiting: bool` (Task 3 sets it; `false` here), both `#[serde(default)]`; `fn fitting_rows(&self, r: &Run, s: &Signal) -> Vec<String>` on `Inner` (the `WIDTH_FIT` test `identify_band` applies, over `policy.modes` only, `Ready` rows only); `async fn start_direct(self: &Arc<Self>, r: &Run, w: &mut State, i: usize, mode: &str) -> Result<Option<JobId>, String>` (`start_on` on `r.prov.source` at the signal's centroid with the row's `bandwidth_hz`, the policy's `sondehub`; `Ok(None)` when cancelled; on `Ok(Some(job))` pushes `Decoding { via: Via::Modes, .. }`, subscribes its events, publishes `decoding` with `via: "modes"` and the summary `"decoding <mode> at <MHz> on <source> (capture, modes)"`; on `Err` the signal counts an unknown try as `next`'s `Err` arm does, summary `"unknown at <MHz>: <mode> did not start: <e>"`); `start_identify(r, w, t, candidates: Option<&[String]>)` writes `{"identify": {"candidates": [...]}}` into the identify record's `context` when `Some`. In `identify.rs`: `Run.candidates: Option<Vec<String>>` read from `rec.context["identify"]["candidates"]` in `launch_identify`; the scoring loop `continue`s past a row not in the list (no `candidate` event for it) when `Some`. `next()`'s dispatch for a due signal with `modes` set: `fits.len() == 0` → publish `unknown` at once with the summary `"unknown at <MHz>: no listed mode fits <width> kHz"` (`khz`), count a try, schedule the retry as today, `capture::on_unknown` as today; `== 1` → `start_direct`; `> 1` → `start_identify` with `Some(&fits)`.

- [ ] **Step 1: Write the failing tests** (fixtures in `fixtures()`: `pocsag3` = three pages at +200, +300 and +400 kHz in a 20 s loop, the third present only in seconds 6-14 (`pocsag_packet_iq` thrice, mixed and summed, `cycle`-style noise); `pocsag_fsk` = the page at +300 kHz beside the suite's GFSK burst at +100 kHz; helper `span_params(modes: &[&str]) -> Value` = `{"modes": [...], "persist_ms": PERSIST_MS, "gone_ms": GONE_MS, "width_hz": [6000, 20000]}`)

```rust
#[tokio::test] async fn a_span_watch_starts_a_decoder_per_signal_without_identify() { /* pocsag3, span_params(["pocsag_decode"]): until 3 entries in decoding (within 30 s); owned(node, watch) stays empty throughout (poll it each 250 ms); every decoding event has via "modes" and mode "pocsag_decode"; each decoder's tuning.center_hz within 1500 Hz of CENTER_HZ + {200,300,400} kHz and bandwidth_hz 12500 */ }
#[tokio::test] async fn a_signal_fitting_no_listed_mode_is_unknown_at_once() { /* pocsag_fsk, span_params(["pocsag_decode"]) with width_hz [2000, 20000]: the page decodes (via modes); an unknown event for the burst's centre whose summary starts "unknown at" and contains "no listed mode fits"; owned stays empty; with on_unknown capture the burst is captured once (captured event, one capture record) */ }
#[tokio::test] async fn two_fitting_modes_run_a_restricted_identify() { /* pocsag fixture, span_params(["pocsag_decode", "aprs_decode"]) over the quick_modes dir: one owned identify runs; its candidate events name only those two rows; the proof is kept with via "identify" */ }
#[tokio::test] async fn a_second_new_on_a_decoded_channel_starts_nothing() { /* pocsag fixture (a page every 2 s), span_params(["pocsag_decode"]), gone_ms 500 so the detector reports gone and new each cycle: after 10 s exactly one decoder was ever started (count decoding events) */ }
#[tokio::test] async fn a_hand_started_decoder_inside_the_band_is_left_alone() { /* pocsag3: job_start pocsag_decode on +300 kHz first, then the span watch: decoding lists two, the hand job's id never in it; stop the watch: the hand job still running */ }
#[tokio::test] async fn stopping_a_span_watch_stops_every_decoder() { /* three decoders; job_stop the watch; until none of the three runs */ }
#[tokio::test] async fn a_replace_of_a_span_watch_stops_its_decoders() { /* job_start measure on the band with replace [watch]: the watch and its three decoders end stopped */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --features test-fixtures --test watch a_span_watch` — Expected: FAIL (no decoder without identify; `decoding` empty).
- [ ] **Step 3: Implement** `fitting_rows`, `start_direct`, the `next()` dispatch, the candidate filter in `identify.rs`.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --features test-fixtures --test watch --test identify` — Expected: PASS, the existing tests included.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core
git commit -m "Watch: with modes, a signal one listed row fits starts that decoder directly in the capture, none fitting is unknown at once, several run a restricted identify"
```

---

### Task 3: The cap, the hold, the cooldown, the confirm and the re-adoption

**Files:**
- Modify: `crates/zeta-core/src/node/watch.rs` (`State`, `Decoding`, `Tracked.waiting`, `signals`, `decoders`, `next`, `run`'s re-adoption, `State::context`)
- Test: `crates/zeta-core/tests/watch.rs`

**Interfaces:**
- Consumes: Tasks 1-2.
- Produces: `#[derive(Clone, Serialize, Deserialize)] struct Cooled { center_hz: u64, bandwidth_hz: u64, until_ns: u64, why: String }`; `State.cooled: Vec<Cooled>` (persisted as `job_context.watch.cooled`, pruned on each tick, cleared at teardown); `Decoding.unproven_by_ns: Option<u64>` (`#[serde(skip)]`; set at a direct start to `since_ns + confirm_ms` when `confirm_ms > 0`, cleared by the decoder's first event); `fn cooled_overlaps(&State, center_hz, bandwidth_hz, now) -> bool`; `fn rank(a: &Tracked, b: &Tracked, known: &[Known]) -> Ordering` (known channel first, then `score`, then `snr_db`; `known` is empty until Task 4); the `waiting` event (`state: "waiting"`, once per signal, summary `"waiting at <MHz>: <n> of <cap> decoding"`), `Tracked.waiting = true`; `decoders()`: `lost` with `why: "unproven"` when `unproven_by_ns` passes while the signal is present, the channel cooled (`why` copied) as a `gone and silent` or `not seen again` loss is; a `new` overlapping a cooled channel is tracked (`fits` computed) but neither started nor identified until `until_ns`; `State::context.cap.waiting` counts `signals[].waiting`; re-adoption in `run()` sets `confirm_by_ns = now + max(gone_ms, hold_ms)`; the hold uses `policy.hold_ms` (Task 1) in place of `silence_ns`. When a slot frees (a `lost`, a decoder whose job no longer runs, which `decoders()` already drops), `next()` picks the present waiting signal by `rank` and starts it as Task 2 does.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn the_cap_queues_and_a_freed_slot_is_taken_by_rank() { /* pocsag3, max_decoders 2: until decoding.len() == 2 and one waiting event (signals[].waiting true for the third, cap {max: 2, decoding: 2, waiting: 1}); job_stop one decoder; until the waiting signal's decoder starts (decoding has 2 again, none waiting); cap reads {2, 2, 0} */ }
#[tokio::test] async fn a_quiet_channel_is_held_then_lost_and_cooled() { /* pocsag3 (the +400 kHz page absent in s 14-20 and 0-6), hold_ms 2000, cooldown_ms 8000, gone_ms GONE_MS: the +400 decoder is lost with why "gone and silent" within hold+gone+margin; cooled[] has its channel with that why; its return at s 6 inside the cooldown starts nothing (no decoding event for +400 for 3 s after the signal's new); after until_ns it starts again */ }
#[tokio::test] async fn an_unproven_decoder_is_stopped_and_its_channel_cooled() { /* a shaped-noise fixture `noise12k` (12 kHz of awgn through a 6 kHz low-pass, mixed to +350 kHz, continuous) beside the page; confirm_ms 3000: the noise's decoder (via modes) is lost why "unproven" within 3 s + margin; cooled names its channel; the page's decoder (events every 2 s) is kept */ }
#[tokio::test] async fn jobs_stop_of_the_decoders_frees_the_cap() { /* pocsag3, max_decoders 2, one waiting; Op::JobsStop {source: rx, kind: Some("pocsag_decode"), plan: None}: both decoders end; the waiting one and the others start again (until decoding.len() == 2 within 20 s) */ }
#[tokio::test] async fn a_span_watch_is_readopted_with_the_hold() { /* a_restart_readopts_or_drops's shape over pocsag3 with hold_ms 12_000 > READOPT_GONE_MS: restart during the +400 page's quiet spell (s 14-20); the resumed watch keeps its three decoders; the +400 one is confirmed when the page returns at s 6 and is never lost not seen again (no lost event for 25 s) */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --features test-fixtures --test watch the_cap_queues` — Expected: FAIL (three decoders start).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p zeta-core --features test-fixtures --test watch`; then `taskset -c 0,1 cargo test -p zeta-core --features test-fixtures --test watch` once — Expected: PASS both.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core
git commit -m "Watch: max_decoders queues signals as waiting and fills a freed slot by rank, hold_ms and cooldown_ms govern lost channels, confirm_ms stops an unproven direct start, re-adoption waits max(gone_ms, hold_ms)"
```

---

### Task 4: Known channels and `band_assignments`

**Files:**
- Modify: `crates/zeta-core/src/knowledge.rs` (`band_assignments`, `BandAssignment`), `crates/zeta-core/src/node/watch.rs` (`State.known`, the start of `run`, `rank`, the `decoding` summary, `seed_known`)
- Test: `crates/zeta-core/src/knowledge.rs` tests, `crates/zeta-core/tests/watch.rs` (the suite's `Register` fake provider)

**Interfaces:**
- Consumes: `KnowledgeProvider::allocations(region, center_hz, bandwidth_hz) -> Vec<Allocation>` (no error channel: an unreachable provider answers an empty list, which this plan cannot tell from "nothing licensed"; see the self-review), `nearest_allocations`, `register_says(&Value)`, `DEFAULT_REGION`, the station's region as `identify.rs` reads it, `Inner.knowledge`.
- Produces: in `knowledge.rs` `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)] pub struct BandAssignment { pub allocation: Allocation, pub sites: usize }` and `pub async fn band_assignments(providers: &[Arc<dyn KnowledgeProvider>], region: &str, center_hz: Hz, bandwidth_hz: Hz, dedupe_hz: Hz) -> Vec<BandAssignment>`: every provider's `kind == "assignment"`, `role == "tx"`, `center_hz` present, inside the band; two within `dedupe_hz` of each other are one (`allocation` the nearer by `distance_km`, a distance-less one losing to any with one; `sites` the count folded); sorted by `center_hz`; uncapped. In `watch.rs`: `#[derive(Clone, Serialize, Deserialize)] struct Known { center_hz: u64, bandwidth_hz: u64, mode: String, from: KnownFrom, licensee: Option<String>, site: Option<String>, distance_km: Option<f64>, sites: usize }`, `enum KnownFrom { Row, Register }` (`snake_case`); `State.known: Vec<Known>` and `State.register: Value` (`{"providers": n, "assignments": m}`), built once at the start of `run()` when `policy.modes` is `Some`: each listed row's `band.centers_hz` inside the band (`from: "row"`, `sites: 0`), then `band_assignments(.., dedupe_hz = half the narrowest listed row's width)` for the watch's band, keeping those whose `emission_decoded.bandwidth_hz` fits a listed row by `WIDTH_FIT` (`mode` that row), `from: "register"`; persisted as `job_context.watch.known = {"providers", "assignments", "channels": [...]}`; `fn known_at(&State, &Signal) -> Option<&Known>` (overlap by the known's width); `rank` (Task 3) puts a signal with a known channel first; the `decoding` summary of a direct start on a known register channel appends `register_says`'s text built from the known's allocation (`"; register says <EMISSION> (...) <licensee>, <site>, <d> km"`); with `seed_known`, after `known` is built, a decoder is started at once on each known channel up to the cap (`start_direct` on a synthetic `Tracked` at the known centre, `present: false`), `Decoding.confirm_by_ns = now + hold_ms`; a later `new` on it updates its signal and clears `confirm_by_ns` as a re-adopted decoder's is (the decoder is not moved).

- [ ] **Step 1: Write the failing tests** (the suite's `Register` fake gains a constructor taking a `Vec<Allocation>`; a helper `assignment(center_hz, bw_hz, emission: &str, licensee, site, km) -> Allocation` with `kind "assignment"`, `role "tx"`, `emission_decoded` from `crate::emission::Emission::parse`)

```rust
// knowledge.rs
#[tokio::test] async fn band_assignments_is_uncapped_and_deduplicates_by_centre() { /* two providers, nine tx assignments on three centres (3 per centre, within 2 kHz of each other) plus an rx row and a plan row: three BandAssignment, sites 3 each, the nearest site named, center_hz ascending; none is the rx or the plan */ }
// watch.rs
#[tokio::test] async fn known_channels_rank_first_and_name_the_licensee() { /* Register answers one 12K5 F1D assignment at CENTER_HZ + 300 kHz (licensee "Telco", site "Mount X", 3 km); pocsag3, max_decoders 1: the first decoder is on +300 kHz (not +200, which the detector sees first) and its decoding summary contains "register says" and "Telco"; job_context.watch.known.channels lists it from "register" and the row's 148637500 from "row" (the band must include it: use the pocsag fixture's band); known.assignments == 1 */ }
#[tokio::test] async fn seed_known_starts_the_known_channel_before_any_signal() { /* same Register, seed_known true, a silent fixture (noise only) for the first 10 s: a decoding event via "modes" on +300 kHz before any signal new; after hold_ms (2000) with no signal it is lost why "not seen again" */ }
#[tokio::test] async fn an_unreachable_register_leaves_known_from_rows_only() { /* a Register whose allocations() sleeps 50 ms and answers nothing: known.channels has only the row centres, known.assignments == 0, the watch decodes as before */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core knowledge::` and `cargo test -p zeta-core --features test-fixtures --test watch known_channels` — Expected: FAIL to compile.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p zeta-core --features test-fixtures --test watch --test identify`, `cargo test -p zeta-core knowledge` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core
git commit -m "Watch: known channels from the listed rows' centres and an uncapped band_assignments query rank first and name the licensee; seed_known starts them ahead of the detector"
```

---

### Task 5: The pager row's `subject` and the `access` marker

**Files:**
- Modify: `modes/pocsag_decode.json`, `crates/zeta-core/src/modes/row.rs` (`ModeRow.access`, `Access`, `validate`, `bad_word_field`), `crates/zeta-core/src/modes/summary.rs` (`envelope`), `crates/zeta-core/src/ops.rs` (`ModeInfo` or whatever `modes_list` answers per row: add `access`), `crates/zeta-core/tests/modes.rs`, `README.md` ("Legal"), `docs/agents/field-craft.md` ("Decoding pagers, aircraft and FM data"), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/modes.rs` (`the_rows_name_the_planned_subjects`, `modes_list_and_show_carry_the_subject` live there), `crates/zeta-core/src/modes/summary.rs` tests

**Interfaces:**
- Consumes: `ModeRow` (`deny_unknown_fields`, `require_all`), `Subject`, `summary::envelope(row, start_ns, fields) -> Value` (called by `zeta_dsp::util` and `zeta_ext::external` for every mode-row event), `the_rows_name_the_planned_subjects`, `modes_list_and_show_carry_the_subject`.
- Produces: `#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)] #[serde(rename_all = "snake_case")] pub enum Access { Restricted }`; `ModeRow.access: Option<Access>` (`#[serde(default)]`, doc: "`restricted` marks a row whose events carry content a deployment may show to fewer people than the spectrum (pager text); copied onto every event as `access`; `null` for the rest. Nothing gates on it yet."); `validate` leaves it; `bad_word_field` names `access` for a word that is not `restricted` (the row is `broken` with `access: ...`); `envelope` inserts `"access": "restricted"` when `row.access` is `Some` and nothing otherwise; `modes_list`'s per-row record and `modes_show` carry `access` (nullable). `pocsag_decode.json` gains `"access": "restricted"` and `"subject": {"key": "address", "name": "addresses", "waterfall": "summary", "columns": [{"field": "kind", "label": "Kind", "unit": null}, {"field": "text", "label": "Text", "unit": null}], "heading": null, "label": null}`. README "Legal" gains one sentence: pager rows (`pocsag_decode`, and `flex_decode` when it ships) are marked `access: restricted` so a later access control can gate their events; nothing gates on it yet. The field craft's pager paragraph says the same in one sentence.

- [ ] **Step 1: Write the failing tests**

```rust
// tests/modes.rs
#[test] fn the_rows_name_the_planned_subjects() { /* add: pocsag_decode -> key "address", columns ["kind", "text"], waterfall summary */ }
#[test] fn the_pager_row_is_restricted_and_the_rest_are_not() { /* pocsag_decode access Some(Restricted); every other embedded row None */ }
#[test] fn a_row_with_a_bad_access_word_is_broken() { /* an operator row with "access": "secret" loads broken, reason starts "access:" */ }
#[tokio::test] async fn modes_list_and_show_carry_the_subject() { /* add: the pocsag row's access reads "restricted", aprs_decode's null */ }
#[tokio::test] async fn a_restricted_rows_events_carry_the_access_marker() { /* a pocsag_decode job over the watch suite's page fixture (or tests/modes.rs's own file source): its first event has access "restricted"; an aprs_decode job on the aprs fixture: no access key */ }
// summary.rs
#[test] fn envelope_adds_access_only_for_a_restricted_row() { /* row(|v| v["access"] = "restricted") -> ev["access"] == "restricted"; the plain row -> ev.get("access").is_none() */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test modes`, `cargo test -p zeta-core summary::` — Expected: FAIL (no `access`, POCSAG has no subject).
- [ ] **Step 3: Implement** the row edit, the field, the envelope line, the `modes_list` field, the two doc sentences; regenerate the generated files.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test modes --test ops`, `cargo test -p zeta-core summary`, `cargo test -p zeta --test modes_check --test agents_md`, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add modes crates/zeta-core README.md docs/agents AGENTS.md skills
git commit -m "Modes: pocsag_decode gets its addresses table (kind, text) and the access: restricted marker, copied onto every event of a restricted row for a later ACL"
```

---

### Task 6: Set records, the loader and `set_list`

**Files:**
- Create: `crates/zeta-core/src/sets.rs`, `sets/airband.json`, `sets/pagers_au.json`, `crates/zeta-core/tests/sets.rs`
- Modify: `crates/zeta-core/src/lib.rs` (`pub mod sets;`), `crates/zeta-core/src/node/mod.rs` (`NodeConfig.sets_dir`, `Inner.sets`, `Node::sets()`), `crates/zeta-core/src/ops.rs` (`Op::SetList`, `OpResult::Sets`, `check_start_params` made `pub(crate)`, the `apply` arm, `Op::name`), `crates/zeta/src/cli.rs` (`--sets-dir`, `Command::Sets`), `crates/zeta/src/node_setup.rs`, `crates/zeta/src/main.rs`, `crates/zeta/src/agents_md.rs` (`RUNNING`: the `--sets-dir` line), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/sets.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Consumes: `ModeRegistry::load`'s shape (`add`, `add_broken`, `stem`, `default_operator_dir`), `is_mode_id`, `JobKind::from_name`, `JobKind::startable`, `ops::check_start_params(kind, deviation_hz, sideband)`, `plans::plan`, `plans::channels`, `DetectParams` (`serde_json::from_value` + `check`), `Node::check_watch_policy`'s row checks (Task 1; factor the per-row test into `pub(crate) fn check_modes_list(modes: &ModeRegistry, list: &[String], band_hz: u64) -> Result<(), String>` in `watch.rs` so the loader reuses it).
- Produces in `sets.rs`: `pub struct SetId(pub String)` (shape `^[a-z][a-z0-9_]*$`, `is_set_id`); `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct SetFile { pub id: SetId, pub name: String, pub notes: String, pub radios: Vec<(String, Radio)>, pub members: Vec<Member> }` (`radios` is a JSON object kept in the file's order through a hand-written `Deserialize` over `MapAccess`, since `serde_json` here has no `preserve_order`; `JsonSchema` describes it as an object of `Radio`); `pub struct Radio { pub note: String }`; `#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)] pub enum Member { JobStart { id: String, radio: String, kind: JobKind, center_hz: Hz, bandwidth_hz: Hz, deviation_hz: Option<u64>, sideband: Option<Sideband>, #[serde(default)] params: Option<Value> }, PlanStart { id: String, radio: String, plan: String, channels: Option<Vec<u32>>, kind: JobKind, deviation_hz: Option<u64>, sideband: Option<Sideband>, attach: Option<JobKind>, language: Option<String> } }` (the nullable fields required-but-nullable as the ops' are); `impl Member { pub fn id(&self) -> &str; pub fn radio(&self) -> &str; pub fn kind(&self) -> &JobKind; pub fn op_name(&self) -> &'static str; pub fn summary(&self) -> String }`; `pub struct SetError { pub field: String, pub error: String }` (Display `<field>: <error>`); `pub fn parse(text: &str) -> Result<SetFile, SetError>` (a `source` or `replace` key anywhere under `members[]` is the error `members[<i>].source: a set file names no source; the request does` / `members[<i>].replace: never in a set file`, read from the raw JSON before serde); `pub fn validate(set: &SetFile, modes: &ModeRegistry, stages: &StageRegistry) -> Result<(), SetError>`: ids and shapes, member ids unique (`members[<i>].id: listed twice`), `radio` a key of `radios` (`members[<i>].radio: unknown role <r>`), `kind` startable and not `spectrum|scan|identify` (a `watch` only as `JobStart`), `kind` a built-in or `is_mode_id` (an unknown row is allowed: a start-time refusal), `check_start_params`, `plan` known and `channels` known, `params` only on `kind: watch` (`members[<i>].params: only a watch member takes params`), a watch member's `params` parse as `DetectParams` and `check()` and `check_modes_list(modes, list, bandwidth_hz)` (`members[<i>].params.<field>: ...`), a `plan_start` member's `attach` attachable and `language` rules as `plan_start`'s; `pub enum SetState { Ready, Broken }`; `pub struct LoadedSet { pub set: SetFile, pub state: SetState, pub reason: Option<String>, pub file: String }`; `pub struct SetRegistry` with `pub fn load(modes: &ModeRegistry, stages: &StageRegistry, operator_dir: Option<&Path>) -> SetRegistry` (the mode loader's rules: embedded first, operator files by name, an operator file with a built-in's id replaces it, a broken file is listed beside the row it meant to replace), `pub fn get(&self, id: &str) -> Option<&LoadedSet>`, `pub fn all(&self) -> &[LoadedSet]`, `pub fn ids(&self) -> Vec<String>`, `pub fn embedded() -> Vec<(String, &'static str)>`, `pub fn default_operator_dir() -> Option<PathBuf>` (`$XDG_CONFIG_HOME/zetasdr/sets`, else `~/.config/zetasdr/sets`). `const EMBEDDED: [(&str, &str); 2]` with `include_str!("../../../sets/airband.json")` and `pagers_au.json`. The two files are the spec's 5.1 JSON (`airband`: roles `wide` and `second`, members `acars` (`plan_start acars_vhf`, `channels: null`, `kind: acars_decode`, `attach: null`, `language: null`), `vdl2` (`job_start vdl2_decode` 136 975 000 Hz, 25 000 Hz), `adsb` (`job_start adsb_decode` 1 090 000 000 Hz, 2 400 000 Hz); `pagers_au`: role `radio`, one `job_start` member `watch` at 148 500 000 Hz, 1 000 000 Hz, `params` the Global Constraints' values plus `modes: ["pocsag_decode"]`). In `ops.rs`: `Op::SetList {}` (doc: "The named sets the node knows..."); `pub struct SetInfo { pub id, pub name, pub notes, pub state: SetState, pub reason: Option<String>, pub file: String, pub radios: BTreeMap<String, Radio>, pub members: Vec<SetMemberInfo { id, radio, op, kind, summary }>, pub running: Option<SetRunning> }` (`running` always `null` until Task 8 defines `SetRunning { radios: BTreeMap<String, SourceId>, since_ns: Timestamp, members: Vec<SetRunningMember { id, job: JobId, state: String }> }`; define the types now, empty); `OpResult::Sets(Vec<SetInfo>)`; `Op::name` → `"set_list"`. `NodeConfig.sets_dir: Option<PathBuf>`; `Node::sets() -> Arc<SetRegistry>`; `zeta --sets-dir PATH` beside `--modes-dir` (default `sets::default_operator_dir`; `Cli::configures_local_node` lists it with `modes_dir`; `agents_md.rs`'s `RUNNING` text gains its line after `--modes-dir`'s); CLI `zeta sets` → `Op::SetList {}`.

- [ ] **Step 1: Write the failing tests** (`tests/sets.rs`: the `plan_start` suite's `Radio` fake copied with `with_max`, `registry()`, `cfg(dir)` taking `sets_dir`; `write_set(dir, json)` helper)

```rust
#[test] fn the_two_builtin_sets_parse_validate_and_are_ready() { /* SetRegistry::load(&modes, &stages, None).ids() == ["airband", "pagers_au"], both Ready, reason None */ }
#[test] fn an_operator_set_replaces_a_builtin_and_a_broken_one_is_listed_broken() { /* airband.json rewritten with name "Mine": get("airband").set.name == "Mine", file the path; a file `bad.json` with id "bad" and members[0].radio "nowhere": state Broken, reason "members[0].radio: unknown role nowhere"; a broken file with id "airband" leaves the built-in in force and is listed as "airband@bad2.json" */ }
#[test] fn a_set_file_never_carries_replace_or_source() { /* a job_start member with "source": "b210" -> Broken, reason "members[0].source: a set file names no source; the request does"; with "replace": [] -> "members[0].replace: never in a set file" */ }
#[test] fn a_watch_members_params_are_checked_at_load() { /* pagers_au with params.hold_ms "ten" -> reason starts "members[0].params"; with params on a measure member -> "members[0].params: only a watch member takes params" */ }
#[test] fn a_watch_member_narrower_than_its_row_is_broken_at_load() { /* a watch member of 10 000 Hz with modes ["pocsag_decode"] -> reason "members[0].params.modes[0]: pocsag_decode is 12500 Hz wide; the watch's band is 10000 Hz" */ }
#[tokio::test] async fn set_list_lists_the_sets_with_their_members_and_no_instance() { /* a node with sets_dir None: two SetInfo, airband.members ids ["acars", "vdl2", "adsb"] with op/kind, running None */ }
#[tokio::test] async fn a_node_without_a_sets_dir_lists_the_builtins() {}
// crates/zeta/tests/cli.rs
#[test] fn zeta_sets_prints_the_two_builtin_sets() { /* `zeta --state-dir D sets` exit 0, stdout JSON with ids airband and pagers_au */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test sets` — Expected: FAIL to compile (`zeta_core::sets`).
- [ ] **Step 3: Implement** the module, the two files, the op, the flag, the CLI command; regenerate.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test sets --test modes`, `cargo test -p zeta --test cli --test agents_md`, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add sets crates AGENTS.md skills
git commit -m "Sets: named set files (airband, pagers_au built in, --sets-dir for the operator's) loaded with the mode registry's rules, listed by set_list and zeta sets"
```

---

### Task 7: `LeaseManager::decide`, and `plan_start` in gated pieces

**Files:**
- Modify: `crates/zeta-core/src/lease.rs` (`decide`, `Decision`), `crates/zeta-core/src/ops.rs` (`plan_start` split: `PlanChannels`, `plan_check`, `plan_existing`, `plan_start_channels`, `plan_attach`), `crates/zeta-core/src/node/mod.rs` (`hold_band` on top of `decide`; `Node::decide`)
- Test: `crates/zeta-core/src/lease.rs` tests, `crates/zeta-core/tests/plan_start.rs` (unchanged, must pass)

**Interfaces:**
- Consumes: `ensure_as`'s decision block (the `fits`, `rigid`, `pinned_in`, `conflict_options` logic), `gate_guard`, `assert_gate`, `plan_start` (steps 1-7 as commented in the source).
- Produces in `lease.rs`: `#[derive(Clone, Debug, PartialEq)] pub enum Decision { Fits(Tuning), Tune, Conflict { holders: Vec<JobId>, current: Tuning, options: Vec<String> } }` and `pub(crate) fn decide(&self, port: &dyn Port, want: Tuning, guard: &OwnedMutexGuard<()>) -> Decision`: `ensure_as`'s rigid-request branch without the lease push or the `port.tune` (`Fits(cur)` inside the current band under rigid holders or elastic ones, `Tune` when only elastic leases or none hold it and the band does not fit, `Conflict` as the `LeaseError::Conflict` fields); `ensure_as` is rewritten to call `decide` first for a non-pin rigid request, so the two cannot drift. `Node::decide(&self, port, want, guard) -> Decision` wraps it. In `ops.rs`: `pub(crate) struct PlanChannels { pub plan: &'static ChannelPlan, pub numbers: Vec<u32>, pub kind: JobKind, pub deviation: Option<Hz>, pub sideband: Option<Sideband> }` with `fn channel_band(&self, n: u32) -> Tuning` and `fn bounding(&self, numbers: &[u32]) -> Tuning`; `pub(crate) fn plan_check(node: &Node, plan: &str, channels: Option<Vec<u32>>, kind: JobKind, deviation_hz: Option<u64>, sideband: Option<Sideband>) -> Result<PlanChannels, OpError>` (today's step 1 without the attach checks and the port lookup); `pub(crate) fn plan_attach_check(node: &Node, attach: &Option<JobKind>, language: &Option<String>) -> Result<(String, Option<Readiness>), OpError>` (the attachable, language and readiness checks: `Err` for a not-ready stage, the message as today); `pub(crate) fn plan_existing(node: &Node, pc: &PlanChannels, source: &SourceId, replace: &[JobId], attach: &Option<JobKind>) -> (Vec<PlanExisting>, Vec<u32>)` (step 2); `pub(crate) async fn plan_start_channels(node: &Node, pc: &PlanChannels, source: &SourceId, to_start: &[u32], granted: Tuning, guard: &OwnedMutexGuard<()>) -> (Vec<PlanStarted>, Vec<PlanRefused>, AfterGate)` (step 6, under a capture the caller already holds); `pub(crate) async fn plan_attach(node: &Node, pc: &PlanChannels, attach: &JobKind, language: &str, readiness: &Option<Readiness>, started: &mut [PlanStarted], existing: &mut [PlanExisting]) -> Vec<String>` (step 7, the warnings). `plan_start` is the composition of these with its gate, `replace_holders` and `hold_band`, behaving exactly as today (every test in `tests/plan_start.rs` passes unchanged).

- [ ] **Step 1: Write the failing test**

```rust
// lease.rs tests (the module's fake port)
#[tokio::test] async fn decide_reports_fits_tune_or_conflict_without_touching_the_port() { /* a port at 100 MHz / 1 MHz with no lease: decide(want inside) -> Tune (nothing holds it, so a rigid start tunes to its own band... assert the spec's rule: with no lease at all a rigid request is Tune); with an elastic lease and the band inside the capture -> Fits(cur); with a rigid holder and the band inside -> Fits(cur); outside -> Conflict {holders [that job], current, options non-empty}; the fake port's tune count stays 0 throughout */ }
#[tokio::test] async fn ensure_as_and_decide_agree() { /* for the same four states, ensure_locked's outcome matches decide's (Ok((cur,false)) <-> Fits, Ok((_, true)) <-> Tune, Err(Conflict) <-> Conflict) */ }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p zeta-core lease::decide` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** `decide`, rewrite `ensure_as` over it, split `plan_start`.
- [ ] **Step 4: Run** — `cargo test -p zeta-core lease`, `cargo test -p zeta-core --test plan_start --test pin --test replace --test elastic --test retune` — Expected: PASS, unchanged.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core
git commit -m "Lease: decide answers fits, tune or conflict without touching the port, and ensure is built on it; plan_start split into check, existing, start-inside-a-capture and attach pieces for set_start to compose"
```

---

### Task 8: `set_start` and `set_stop`

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`Op::SetStart`, `Op::SetStop`, `OpResult::SetStart`, `OpResult::SetStopped`, `set_start`, `set_stop`, `summary_json`), `crates/zeta-core/src/node/mod.rs` (`Inner.set_instances`, `sets_dir`, the `open` read-back, `Node::set_instances()`), `crates/zeta-core/src/node/persist.rs` (the instance file read/write), `crates/zeta-core/src/sets.rs` (`SetInstance`), `crates/zeta/src/cli.rs` (`Command::Set(SetCmd::Start | Stop)`), `crates/zeta/src/agents_md.rs` (per-op notes for the three ops; `RULES` gains "For a named set use `set_start`, never its members by hand; set ids come from `set_list`"), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/sets.rs`, `crates/zeta/tests/cli.rs`

**Interfaces:**
- Consumes: Tasks 6-7 (`SetRegistry`, `Member`, `Decision`, `plan_check`, `plan_existing`, `plan_start_channels`, `plan_attach`), `gate_guard`, `replace_holders`, `hold_band`, `duplicate_of`, `job_start_gated`, `after_gate`, `checked_port`, `jobs_stop`'s stop order (demodulators then attached), `OPS_RESULT_LIMIT_BYTES`.
- Produces: `Op::SetStart { set: String, radios: BTreeMap<String, Option<SourceId>>, members: Option<Vec<String>>, replace: Vec<JobId> }` (doc text: the spec's 5.2 description, the eight steps in one paragraph); `Op::SetStop { set: String }`; `pub struct SetRadio { pub source: SourceId, pub capture: Tuning }`, `pub struct SetStarted { pub member: String, pub channel: Option<u32>, pub center_hz: Hz, pub job: JobId, pub attached: Option<JobId>, pub attach_error: Option<OpError> }`, `pub struct SetExisting { pub member: String, pub channel: Option<u32>, pub center_hz: Hz, pub job: JobId, pub attached: Option<JobId> }`, `pub struct SetRefused { pub member: String, pub channel: Option<u32>, pub error: OpError }`; `OpResult::SetStart { set: String, radios: BTreeMap<String, SetRadio>, started: Vec<SetStarted>, existing: Vec<SetExisting>, refused: Vec<SetRefused>, warnings: Vec<String> }` (`summary_json` keeps `counts` and `warnings` as `PlanStart`'s does); `OpResult::SetStopped { set: String, stopped: Vec<JobId>, ended: Vec<JobId>, failed: Vec<JobStopFailed> }`; in `sets.rs` `#[serde(deny_unknown_fields)] pub struct SetInstance { pub format: u32, pub set: String, pub radios: BTreeMap<String, SourceId>, pub since_ns: Timestamp, pub members: Vec<InstanceMember { pub id: String, pub jobs: Vec<JobId> }> }`, written to `state_dir/set-instances/<set id>.json`, read back in `Node::open` (an unreadable file is a `persist_errors` line, not a failure), dropped by `set_stop` or when every job it names has ended (checked on `set_list` and `set_start`); `SetInfo.running` filled from it with each job's `state` (`ended` for a member whose jobs all ended). `async fn set_start(node, set, radios, members, replace) -> Result<OpResult, OpError>` in the spec's order: (1) checks (`unknown` on `set` with the known ids; `other` for a broken set with its reason; `unknown` on `members[<i>]`; `other` "radios.<role>: unmapped" for a role a chosen member uses with no entry; `unknown_source` on `radios.<role>`; a member whose `kind` is a mode row not `Ready` → `refused` `other` with the row's reason (`unavailable: <reason>`); one whose kind is neither a built-in nor a loaded row → `refused` `unknown` `{field: "members[<i>].kind", value, known: modes ids}`; `plan_check` per plan member; `plan_attach_check` with a not-ready attach → the member starts without it and a warning `"members[<i>]: <attach>: <reason>; started without attach"`; an instance running with another mapping → `other` "set <id> runs on {...}; set_stop it first"); (2) per source in use the bounding band of its members (`checked_port` → `out_of_range` with `field: "radios.<role>"`); (3) `gate_guard` for every source in use, in source-id order, held to the end; (4) `existing` via `duplicate_of` per member (a plan member via `plan_existing`); (5) `replace_holders` per source (each id must hold one of the sources in use, else `other` "job <id> does not hold any source of the request"); (6) `decide` per source first, a `Conflict` anywhere → `OpError::Conflict` with `source` the role's source and `options` prefixed `"radios.<role> (<source>) is held by ..."`, nothing tuned; then `hold_band` per source (a `Port` error is that source's failure as `plan_start` words it, the sources already tuned stay tuned, nothing has started); (7) starts source by source in role order (the file's `radios` order, Task 6's `Vec<(String, Radio)>`), member by member, each inside its capture: a `job_start` member through `kind.expand` + `job_start_gated` (`capture_rate_hz` the granted width), a `plan_start` member through `plan_start_channels`; (8) the instance written, the guards dropped, `after_gate` per source, `plan_attach` per plan member, `attach`ed job ids added to the member's `jobs`. `async fn set_stop(node, set)`: the instance's jobs that still run, radio jobs first then attached ones, through `Node::job_stop`; `stopped`, `ended` (not running when asked), `failed`; the instance file removed. CLI: `zeta set start --set ID --radio ROLE=SOURCE[,ROLE=SOURCE] [--radio ROLE=] [--members a,b] [--replace ID,..]` (`ROLE=` with nothing after is `null`), `zeta set stop --set ID`.

- [ ] **Step 1: Write the failing tests** (`tests/sets.rs`: two `Radio`s `a` and `b`; a sets dir the test writes with `two_radio.json`: role `a` a `plan_start` member `cb` (`uhf_cb_au`, channels `[1, 2, 3]`, `fm_demod`, `deviation_hz` 2500), role `b` a `job_start` member `m` (`measure` at 200 MHz, 20 kHz) and role `b` a watch member `w` (`watch` at 300 MHz, 1 MHz, params `{modes: ["pocsag_decode"]}`); helper `set_op(set, radios: &[(&str, Option<&str>)], members: Option<&[&str]>, replace: &[&JobId]) -> Op` and `started_set(node, op) -> SetStartParts`)

```rust
#[tokio::test] async fn a_set_starts_its_members_under_one_tune_per_radio() { /* the test set's role b members both lie inside 300 ± 0.5 MHz (m at 300.2 MHz / 20 kHz, w at 300 MHz / 1 MHz): a.tunes() == 1, b.tunes() == 1; the result's radios a and b carry captures; started has the 3 cb channels (member "cb", channels 1-3), m and w; set_list.running names every job with state running */ }
#[tokio::test] async fn a_conflict_on_either_radio_starts_nothing_and_names_the_role() { /* a rigid measure on b at 900 MHz: Err Conflict with options[0] starting "radios.b (b) is held by"; a has no holder and a.tunes() unchanged; repeat with the ids swapped (radios named "z" and "b" so the gate order flips) */ }
#[tokio::test] async fn two_roles_on_one_source_share_one_capture_or_are_refused() { /* radios {a: a, b: a}: bands that fit one capture (cb channels 1-3 and measure moved near 476.5 MHz) start with a.tunes() == 1; with the watch at 300 MHz too: Err other naming "radios.a" and "radios.b", nothing tuned */ }
#[tokio::test] async fn a_second_start_adds_only_what_is_missing() { /* run twice: the second answers every member existing, started empty, tunes unchanged */ }
#[tokio::test] async fn a_different_mapping_while_running_is_refused() { /* second start with {a: b, b: a}: Err other containing "set_stop it first" */ }
#[tokio::test] async fn members_narrows_and_unmapped_roles_may_be_omitted() { /* members ["m"], radios {b: b} only: one start, a untouched */ }
#[tokio::test] async fn set_stop_stops_the_instances_jobs_only() { /* a hand measure inside a's capture survives; attached jobs (fm_attach-style plan member with attach transcribe on an attach_registry node) end with their inputs; set_list.running None after */ }
#[tokio::test] async fn set_stop_with_ended_members_stops_the_rest() { /* job_stop m and w by hand; set_stop lists both in ended, the three cb jobs in stopped, failed empty; set_list.running is None and the instance file is gone */ }
#[tokio::test] async fn a_restart_keeps_the_instance() { /* a_restart_resumes_the_plan_jobs_as_ordinary_jobs's shape: reopen; set_list.running names the resumed jobs; one stopped before the restart reads ended */ }
#[tokio::test] async fn gates_are_taken_in_source_order_and_a_dropped_start_leaves_no_holder() { /* b slow; drop the set_start future mid-way (the plan suite's shape): afterwards neither source has a holder, no instance file */ }
#[tokio::test] async fn a_truncated_set_result_keeps_its_counts() { /* OpResult::SetStart with 2000 started entries: summary_json has truncated true, counts {started, existing, refused}, warnings */ }
// cli.rs
#[test] fn zeta_set_start_and_stop_map_to_the_ops() { /* `zeta set start --set airband --radio wide=b210 --radio second=` parses to Op::SetStart with second None; `zeta set stop --set airband` to Op::SetStop (the CLI's op-mapping test style in cli.rs, not a live node) */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test sets a_set_starts` — Expected: FAIL to compile (`Op::SetStart`).
- [ ] **Step 3: Implement**; regenerate the generated files.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test sets --test plan_start --test resume`, `cargo test -p zeta --test cli --test agents_md`, `cargo run -q -p zeta -- agents-md --check`; then `taskset -c 0,1 cargo test -p zeta-core --test sets` once — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates AGENTS.md skills
git commit -m "Sets: set_start starts a set's members across radios under one lease decision per source with every gate held, writes the instance; set_stop stops that instance's jobs and nothing else; zeta set start/stop"
```

---

### Task 9: Members: `null` radios, watch members, rows this host lacks, unready attaches

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (`set_start`: the pick, the watch member, the refusals), `crates/zeta-core/src/node/watch.rs` (`pub(crate) fn policy_of(graph) -> Value`: the `detect` node's params as JSON for the identity test), `crates/zeta-core/src/node/mod.rs` (`job_graph_set` reachable under a held gate: `pub(crate) async fn job_graph_put_gated(&self, id, graph, guard)`)
- Test: `crates/zeta-core/tests/sets.rs`

**Interfaces:**
- Consumes: Task 8; `free_sources(ports, center_hz, bandwidth_hz)` (a suggestion, not a reservation); `Node::job_graph_put`'s restart path (`job_graph_set`); `Policy::of`; `JobKind::Watch.expand`; the `airband` and `pagers_au` files (Task 6).
- Produces: in `set_start` step (1), before the gates: for each role mapped to `null`, in the file's role order, the first source of `source_list` order that `free_sources` lists for the role's bounding band and that no other role of this request uses; none → `other` `"radios.<role>: no free source covers <center> Hz ± <half> Hz; busy: <id> (holders [...]), ..."` (`source_free`'s wording); the result's `radios` names the pick. A watch member: `job_start_gated` of the expanded watch graph with the member's `params` merged into its `detect` node before the start (`graph.nodes[detect].params = params`; `check_watch_policy` runs in `job_start_gated`), so no restart is needed (the spec's `graph_put` after the start is replaced by setting the params before it; `job_graph_put_gated` is then not needed and is not added); its identity for `existing`: a running `watch` on the same source and band whose `policy_of(graph) == member.params` (keys the member leaves out compared at their defaults) is `existing`; one with another policy → `other` `"watch <id> holds this band with other policy; stop it or graph_put it"` before anything starts. A member whose `kind` is a mode row not `Ready` → `refused` with `other { message: "<state>: <reason>" }`; one whose kind is unknown to this node (`vdl2_decode` until plan 30) → `refused` with `unknown { field: "members[<i>].kind", value: "<kind>", known: <mode ids> }`; neither blocks the rest. A plan member whose `attach` stage is not ready → started without it, the warning of Task 8.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn a_null_radio_picks_the_first_free_source_in_role_order() { /* radios {a: None, b: None} with a (20 MHz max) and c (1 MHz max, id sorts first): role a's 3 cb channels (75 kHz) picks c, role b's 1 MHz watch picks a; the result's radios say so; a second set_start with the same explicit mapping is all existing */ }
#[tokio::test] async fn no_free_source_is_other_naming_the_role() { /* b held rigid at 900 MHz, radios {b: None}, members ["w"]: Err other starting "radios.b: no free source covers 300000000 Hz ± 500000 Hz; busy: b" */ }
#[tokio::test] async fn a_watch_member_on_a_band_another_watch_holds_is_other_unless_the_policy_matches() { /* a hand watch on b at 300 MHz / 1 MHz with default params: set_start -> Err other containing "holds this band with other policy"; stop it, set_start, set_start again: the second answers w existing, and the watch's created_ns and context run count are unchanged (no restart) */ }
#[tokio::test] async fn a_watch_member_runs_with_its_params() { /* after set_start the watch's detect node has modes ["pocsag_decode"] (graph_get) and job_context.watch.cap.max == 16; the watch's loop ran once: no second launch (count the ops stream's job_start/graph_put entries: none for it beyond the set_start) */ }
#[tokio::test] async fn the_airband_set_starts_what_this_host_has_and_refuses_the_rest() { /* the node's modes_dir holds an operator adsb_decode row whose programs name a binary that does not exist ("zeta-test-missing-decoder"), so the row is unavailable whatever the host has; the built-in airband on radios {wide: a, second: b}: started has 7 acars channels (member "acars", channels 1-7); refused has vdl2 (error unknown, field "members[1].kind", known the mode ids) and adsb (error other, message starting "unavailable"); b.tunes() == 0 and the result's radios has no "second" (call 8) */ }
#[tokio::test] async fn an_unready_attach_starts_the_member_without_it_and_warns() { /* attach_registry(not ready) node, a plan member with attach transcribe: the channels start, attached None, warnings[0] == "members[0]: transcribe: <reason>; started without attach" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test sets a_null_radio` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test sets --test watch --features test-fixtures` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core
git commit -m "Sets: a null role picks the first free source in role order, a watch member starts with its params and is existing only on the same policy, a member this host lacks is refused and the rest start"
```

> The `vdl2` member of `airband` depends on plan 30's `vdl2_decode` row. Until that row lands the member is `refused` (`unknown` naming `members[1].kind`); when the row exists but `dumpvdl2` does not, `refused` `other` "unavailable: ..."; with both, it starts. `the_airband_set_starts_what_this_host_has_and_refuses_the_rest` pins the first two states and needs rewording (not rewriting) when plan 30 merges.

---

### Task 10: The agent's gate and its own sets

**Files:**
- Modify: `crates/zeta-agent/src/agent.rs` (`GATED_REPLACE_OPS`, `gate`, `OwnJobs`)
- Test: `crates/zeta-agent/tests/agent.rs`

**Interfaces:**
- Consumes: `Allow`, `gate(op, allow) -> Option<Value>`, `OwnJobs::{allows_stop, record}`, `gate_covers_every_replace_taking_op`.
- Produces: `GATED_REPLACE_OPS` gains `"set_start"`; `gate` treats `set_stop` as it treats `job_stop` and `jobs_stop` (`allow.stop_jobs`); `OwnJobs.sets: HashSet<String>`; `record("set_start", ..)` adds every `started[].job` and `started[].attached` to `jobs` and the result's `set` to `sets`; `allows_stop` also passes `{"op": "set_stop", "set": s}` when `sets` contains `s`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn set_start_with_a_replace_needs_replace_allowed() { /* gate(set_start with replace ["x"], default) -> not_allowed field "replace"; with replace true -> None; empty replace -> None */ }
#[test] fn set_stop_needs_stops_allowed_unless_the_run_started_the_set() { /* gate(set_stop, default) -> not_allowed; OwnJobs after record("set_start", args, {value: {set: "airband", started: [{job: "01A", attached: null}]}}) allows_stop(set_stop airband) and allows_stop(job_stop 01A); not set_stop pagers_au */ }
#[test] fn set_list_passes_the_gate() {}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-agent --test agent set_` — Expected: FAIL (`gate_covers_every_replace_taking_op` also fails until `set_start` is listed).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p zeta-agent` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-agent
git commit -m "Agent: set_start's replace and set_stop pass the Allow gate as plan_start's and job_stop's do; a run may stop the set it started"
```

---

### Task 11: The page: "Decode across", the watch fold, the merged Subjects

**Files:**
- Modify: `crates/zeta-ui/web/menu.js` (`ENTRIES`, a `decode_across` entry with a submenu, `jobTuning`, `choose`), `crates/zeta-ui/web/agent.js` (the group rows: a `groupOf` predicate beside `foldPlan`, `groupText` per kind), `crates/zeta-ui/web/subjects.js` (`SubjectStore.groupOf`, sections keyed by group, the `Channel` column), `crates/zeta-ui/web/style.css` (`.ctx-sub`, `.job-group[data-kind="watch"]`), `crates/zeta-ui/tests/e2e/ui.py`
- Test: `crates/zeta-ui/tests/e2e/ui.py` (`scenario_decode_across_entry_starts_a_watch_with_modes`)

**Interfaces:**
- Consumes: `job_list` records with `context` (`context.watch.decoding[].job`, `context.watch.cap`, `.signals[].waiting`), the source feed's `watch` events (`decoding`, `lost`, `waiting` with `job`), `jobGroup(key)`, `placeChildren`, `storedOpen`, `JOB_FOLD_KEY`, `SubjectStore.jobs` (job id → key → subject), `SubjectsPane.sections` (per job), `columns(row)`, `modes_list`/`modes_show` as `subjects.js` reads them, the menu's `JOBS` map and `graph_get`/`graph_put` as `scenario_owned_jobs_are_hidden_from_the_jobs_pane` does them.
- Produces: `menu.js`: entry `["decode_across", "Decode across this band…"]` after `watch`; opening the menu fills a submenu `#ctx .ctx-sub[data-for="decode_across"]` with one `.ctx-item[data-mode="<id>"]` per `ready` row of `modes_list` (read once per page, cached) whose `band.bandwidth_hz` fits the selection (`row.band.bandwidth_hz <= selection.bandwidth_hz`), text `"Decode <row.name> across this band"`; choosing one sends `job_start` `kind: "watch"` on the selection, then `graph_get`, sets `modes: [<id>]`, `persist_ms: 500`, `width_hz: [round(0.5 × bw), round(1.5 × bw)]` on the `detect` node, `graph_put`; the transcript gets both op lines (the usual `applyOp` path). `agent.js`: `const groupOf = (j, jobs) => ({key, kind: "watch", title} | null)`: a running, unowned job whose id appears in some running watch's `context.watch.decoding[].job` is under `{key: watch.id, kind: "watch"}`; `fold` uses `groupOf` first and `foldPlan` second, draws a group from one member (no `JOBS_FOLD_MIN` for `watch` groups), the `details` row gets `data-kind`; `groupText` for a watch: `"<watch kind> <id tail> · <mode> × <n> decoding, <m> waiting"` (`m` from `context.watch.cap.waiting`); the watch's own row stays where it is and the group row carries its decoders directly under it (the group `summary` is the watch row's text, so the watch row becomes the group's `summary`); a decoder's Stop is its own `job_stop`; `QUIET_OPS` unchanged. `subjects.js`: `groupOf(job, jobs)` (the same rule, computed from the last `sync(jobs)`), the store's map becomes group id → key → subject where group is the watch id for a decoder and the job id otherwise (`store.list(groupId)`, `held`, `select`, `drop` take the group id; `subject.job` stays the decoder's id and gains `subject.group`); the pane draws one `section.subject-job[data-job="<group>"]` per group titled by the watch (`watch <tail> · <row.subject.name>`) with the columns `key, Channel, ...row columns, Last seen`, `Channel` = `(decoder.tuning.center_hz / 1e6).toFixed(4)`; a merged section goes when its watch stops (`sync`).

- [ ] **Step 1: Write the failing scenario**

```python
def scenario_decode_across_entry_starts_a_watch_with_modes(page, url, secret):
    # select the pocsag source, draw a box over its 48 kHz capture (set_band / drag_menu as the pocsag ribbon scenario does), open the menu
    # expect '#ctx .ctx-item[data-action="decode_across"]' to have text "Decode across this band…"; hover/click it: the submenu lists '.ctx-item[data-mode="pocsag_decode"]' with text "Decode POCSAG pagers across this band"; click it
    # wait_until a running watch on "pocsag" whose graph_get detect node has modes == ["pocsag_decode"], persist_ms 500
    # wait_until (20 s) a running pocsag_decode job on "pocsag" listed in that watch's context.watch.decoding
    # page.evaluate("window.zetaTest.poll()"); expect '#jobs .job-group[data-plan="<watch id>"][data-kind="watch"]' count 1, containing '.job[data-id="<decoder id>"]' and a summary matching r"pocsag_decode × 1 decoding, 0 waiting"
    # open_subjects(page); expect '#subjects section.subject-job[data-job="<watch id>"]' count 1 with a 'th[data-col="channel"]' and 'tr.subject[data-key="1234567"]' whose channel cell reads "148.6375"
    # finally: stop the watch (its decoders go with it); assert no pocsag_decode runs
```

- [ ] **Step 2: Run to verify it fails** — `ZETA_UI_ONLY=decode_across_entry_starts_a_watch_with_modes cargo test -p zeta --test ui -- --nocapture` (the harness's scenario filter) — Expected: FAIL (no `decode_across` entry).
- [ ] **Step 3: Implement** the three modules and the style.
- [ ] **Step 4: Run** — the scenario alone, then `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped`; note the suite's total against the 690 s deadline) — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui
git commit -m "Page: Decode across this band starts a watch with modes from the box menu, a watch's decoders fold under its row, and their subjects merge into one table per watch with a Channel column"
```

---

### Task 12: The page: "Start set…", the set fold, nesting

**Files:**
- Modify: `crates/zeta-ui/web/menu.js` (`set` entry, the role → source picker), `crates/zeta-ui/web/agent.js` (`set_list` polled with `job_list`, `groupOf` for sets, two-level nesting, the group's stop as `set_stop` with a confirm), `crates/zeta-ui/web/style.css`, `crates/zeta/tests/ui.rs` (`fake_sets(dir) -> PathBuf`, `--sets-dir` on the main node), `crates/zeta-ui/tests/e2e/ui.py`
- Test: `crates/zeta-ui/tests/e2e/ui.py` (`scenario_set_menu_starts_a_set_and_groups_its_jobs`)

**Interfaces:**
- Consumes: Task 11's `groupOf` and group rows; `set_list` (`running.members[].job`, `running.radios`), `set_start`, `set_stop`; the e2e node's sources `tone` (48 kHz at its recording's centre) and `cb` (the tone recording at 476.9 MHz = `uhf_cb_au` ch20); `ServeProc::start_env` in `ui.rs`; the Clear all button's second-click confirm pattern in `agent.js`.
- Produces: `ui.rs`: `fake_sets(dir)` writes `two_sources.json` (`id: "two_sources"`, `name: "Two sources (UI tests)"`, roles `a` and `b`, members `m` = `job_start measure` on `a` at the tone recording's centre, 20 kHz, and `cb20` = `plan_start uhf_cb_au channels [20] fm_demod deviation_hz 2500` on `b`), passed as `--sets-dir` to the main node. `agent.js`: the 2 s poll also sends `{op: "set_list"}` (added to `QUIET_OPS`); `groupOf` returns `{key: set.id, kind: "set", title: set.name}` for a job listed in a running instance's `members[].job`, and a job under a watch that is itself a set member nests: the watch group inside the set group (two levels at most); `groupText` for a set: `"<set name> · <n> on <shown source>, +<m> on <other source>"`; the set group's Stop button sends `set_stop` on a second click within 3 s (first click: the button reads "Stop set?"), a watch row's Stop is `job_stop` of the watch, a member row's Stop its own `job_stop`. `menu.js`: entry `["set", "Start set…"]` beside `plan`, hidden when `set_list` has no `ready` set; choosing it opens `#ctx-set` (a small form): a select of ready sets, then one row per role of the chosen set with a select of every source in `source_list` plus "pick" (`null`), the first role defaulting to the shown source; Start sends `set_start` with `members: null`, `replace: []`; the transcript line reports `started`, `existing`, `refused` and `warnings` counts as the plan entry does.

- [ ] **Step 1: Write the failing scenario**

```python
def scenario_set_menu_starts_a_set_and_groups_its_jobs(page, url, secret):
    # open the menu on tone; click '[data-action="set"]'; in '#ctx-set' choose "two_sources", role a = tone (default), role b = cb; click Start
    # wait_until set_list shows running two_sources with 2 members running
    # poll; expect '#jobs .job-group[data-plan="two_sources"][data-kind="set"]' count 1 with summary matching r"Two sources \(UI tests\) · 1 on tone, \+1 on cb" and containing the measure row
    # select_source cb; poll; the group's summary matches r"1 on cb, \+1 on tone" and contains the fm_demod row
    # click the group's Stop twice; wait_until set_list running is None and no measure or fm_demod runs on tone/cb
```

- [ ] **Step 2: Run to verify it fails** — `ZETA_UI_ONLY=set_menu_starts_a_set_and_groups_its_jobs cargo test -p zeta --test ui -- --nocapture` — Expected: FAIL (no `set` entry).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p zeta --test ui -- --nocapture` — Expected: PASS, no `ui e2e skipped`, total under the deadline (drop the optional third scenario of spec 7.3 if it was added and the suite nears 600 s).
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui crates/zeta/tests/ui.rs
git commit -m "Page: Start set… picks a radio per role and sends set_start, a set's jobs fold under its name with the other radio counted, set stop confirms, folds nest set → watch → decoders"
```

---

### Task 13: Field craft, per-op notes, RECIPES, README

**Files:**
- Modify: `docs/agents/field-craft.md` ("Decoding every channel of a band" after "Watching a band for sondes"; "Starting a named set" after "Starting a whole plan"; the ACARS + ADS-B example in "Decoding pagers, aircraft and FM data" points at `airband`), `crates/zeta/src/agents_md.rs` (per-op notes for `set_list`, `set_start`, `set_stop` finished; the `watch` note under `job_start` mentions `modes`), `RECIPES.md` (the ACARS and ADS-B recipes become one section "Airband data links as a set" with `set_start airband` and a pointer to `sets/airband.json`; the convention paragraph says sets are the stored form of a recipe the node can check), `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: Write the field craft** — "Decoding every channel of a band": the two-op form (`job_start watch`, `graph_get`, `modes` + `persist_ms` + `width_hz` on `detect`, `graph_put`), the set form (`set_start pagers_au`), what `decoding via modes`, `waiting`, `lost why gone and silent | not seen again | unproven` mean, `job_context.watch.{cap, known, cooled}`, why `decode_on: free` is wrong for a span, that a hand-started decoder inside the band is left alone, that stopping the watch stops its decoders, the pager rows' `access: restricted` sentence. "Starting a named set": `set_list` first (ids come from it), `set_start` with `radios` (a role mapped to `null` is picked), a `conflict` names the role and starts nothing, `members` narrows the next request, never `replace` unnamed, `set_stop` is the operator's, read the four lists. The per-op notes: `set_start`'s mirrors `plan_start`'s paragraph; `set_stop`'s mirrors `jobs_stop`'s.
- [ ] **Step 2: Regenerate and check** — `cargo run -q -p zeta -- agents-md > AGENTS.md && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md && cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check`, `cargo test -p zeta --test agents_md`, `bash scripts/check-doc-links.sh` — Expected: clean.
- [ ] **Step 3: Commit**

```bash
git add docs/agents RECIPES.md README.md AGENTS.md skills crates/zeta/src/agents_md.rs
git commit -m "Guide: decoding every channel of a band and starting a named set in the field craft; set_* per-op notes; the airband recipe is the set"
```

---

### Task 14: TODO, UAT, the plan series, the gate

**Files:**
- Modify: `TODO.md`, `UAT.md` (a "Plan 31" section), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (a "Sprint 2026-11-06" table if none exists, with row 31; a "Plan 31 status" section)

- [ ] **Step 1: TODO.md** — In "Feature: decoder wave 2" strike the "Whole-span POCSAG and APRS" bullet. In "Modes: VDL Mode 2 beside ACARS, and an airband capture plan" strike the airband-set sentence (delivered as `sets/airband.json`; the VDL2 half stays for plan 30). In "Architecture: a mode registry and a generic external-decoder adaptor" narrow "Span decoders and trunking" to trunking (P25, DMR Tier III, MPT1327: a control-channel follower that plugs into the watch's known-channels seam). Narrow "Architecture: recipes the node stores and runs by name" to what spec 5.6 leaves (pins, mixes, waits, verify steps, ordered arbitrary ops; `set_start` as one step). Under "Subjects: leftovers" strike "POCSAG has no subject". Add "Access: ACLs on restricted rows" (spec 4.8: the marker, what gates on it later: per-secret or per-transport levels, an event filter on the feeds and `stream_tail`, the Subjects text column, the `events.log` reader; `pocsag_decode` and `flex_decode` carry it). Add to the architecture gists one line recording the operator's direction: long-running jobs move to additional narrowband tuners (HackRF, LimeSDR) freeing the wideband radio; the long-term goal is a tap off an FPGA polyphase channel bank over a very wide band. Add "Watch: deferred minors from plan 31": `known.register` cannot say `unreachable` (no error channel on `KnowledgeProvider::allocations`); `cooled[]` is pruned per tick; a seeded decoder's centre is the known centre until the first `new`; a set file's `radios` order is the file's (not sorted); the menu's `modes_list` is cached per page load.
- [ ] **Step 2: Plan series** — a row for 31 (the sprint table of 2026-11-06, created if absent, with the sibling plans 29, 30, 32 as "spec written" rows if their specs exist in `docs/superpowers/specs/`) saying built on this branch with the commit range and "see Plan 31 status"; the status section lists what shipped (watch policy, direct start, cap/hold/cooldown/confirm, known channels, the pager row and `access`, sets and the three ops, `decide`, the agent gate, the page), what the gate proved, the calls made (below, in one paragraph), and what waits for the field host (spec 6 UAT a-c).
- [ ] **Step 3: UAT.md** — "Plan 31" with spec 6's three checks as checkboxes: (a) `set_start pagers_au` on the B210 (decoders per carrier within five minutes, licensee in the summaries, the fold, the pager table with Channel, `access: "restricted"` on the events and in `zeta events tail`, no churn after an hour); (b) `set_start airband` with `{wide: b210, second: hackrf}` (seven ACARS, one ADS-B, `vdl2` refused until plan 30, the group, `existing` on a re-run, `set_stop`); (c) `set_start airband` with `second: null` picks the HackRF.
- [ ] **Step 4: Gate** — `cargo fmt -p <each crate> -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta-core --features test-fixtures --test watch`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped`), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Expected: all green.
- [ ] **Step 5: Commit**

```bash
git add TODO.md UAT.md docs
git commit -m "Docs: plan 31 (span decoding and named sets) in TODO, the plan series status and UAT"
```

---

## Calls made in this plan (the builder's, on the spec's silence)

1. A watch member's `params` are set on the expanded graph's `detect` node before `job_start_gated`, not applied by a `graph_put` after the start (spec 5.3 described the restart): one launch, no restart, the same validation path (`check_watch_policy`), and the identity test for `existing` compares the stored `detect` params. The page's "Decode across" entry keeps the two-op form (it has no access to the expansion).
2. The restricted candidate list rides in the identify record's `context["identify"]["candidates"]`, read by `launch_identify` into `Run.candidates`; `start_on` is reused unchanged for the direct start (aimed at the watch's own source it leases inside the capture without a tune).
3. `job_context.watch.known` is `{providers, assignments, channels}`: `KnowledgeProvider::allocations` has no error channel, so "unreachable" cannot be told from "nothing licensed" (spec 4.3's `known.register: "unreachable"`); the counts say what was asked and answered.
4. `band_assignments` returns `BandAssignment { allocation, sites }` rather than adding `sites` to `Allocation` (whose schema is `deny_unknown_fields` and shared with `knowledge_lookup`); `dedupe_hz` is half the narrowest listed row's width.
5. The watch and set folds are membership folds drawn from one member, sharing the plan group's DOM and fold memory and nothing of `foldPlan`'s idleness predicate (spec 9.22); the watch's own row is the group's summary.
6. Membership on the page comes from `job_list`'s `context` (already in every poll) and from `set_list` polled beside it; no `job_context` seed reads.
7. A set file's `radios` keep the file's order (`Vec<(String, Radio)>`), since the `null` pick and the start order are "roles in the file's order"; the ops' JSON shows them as an object.
8. A source whose every member was refused at check time (the `airband` `second` role on a host without dump1090) is left out of the gates, the decisions and the result's `radios`, and is never tuned.
9. A member whose kind is unknown to this node is `refused` `unknown` naming `members[<i>].kind` with the mode ids as `known`; a loaded but not-`Ready` row is `refused` `other` "<state>: <reason>". Both leave the rest to start.
10. `two_roles_on_one_source` is allowed when the bounding bands fit one capture of that source, refused `other` naming both roles otherwise (the spec's step 1 wording).
11. `ensure_as` is rebuilt on `decide` so the two cannot drift; `hold_band` stays as it is.
12. `set_stop` lists `ended` for members the operator already stopped and fails nothing on them; the instance is also dropped when `set_list` or `set_start` finds every job of it ended.
13. The `access` marker is inserted by `summary::envelope`, so both in-crate stages (`zeta_dsp::util`) and external rows (`zeta_ext::external`) carry it with no further wiring; events of unmarked rows carry no `access` key.
14. The e2e Subjects check rides on the one decoder the "Decode across" scenario starts (spec 7.3's budget note); the two-decoder APRS scenario is optional.

## Self-review notes

- Spec coverage: 4.1 → Task 1; 4.2 → Task 2; 4.3 → Task 4; 4.4 → Task 3; 4.5 → Tasks 2, 3 (`held_elsewhere`, `jobs_stop`, stop and replace); 4.6 → Tasks 1, 3, 4 (events and context); 4.7 → Task 11; 4.8 → Task 5 (+ README and field craft sentences, finished in Task 13); 5.1 → Task 6; 5.2 → Task 8; 5.3 → Task 9 (call 1); 5.4 → Task 9; 5.5 → Tasks 8 (restart, status) and 12 (the page); 5.6 → Task 13 (RECIPES, TODO in 14); 6 → Tasks 13, 14; 7.1 → Tasks 1-5; 7.2 → Tasks 6-10; 7.3 → Tasks 11, 12; 8 → folded (8.1 non-goal line in Task 14's TODO, 8.2 and 8.3 Task 5, 8.4 Task 9, 8.5 none, 8.6 Task 4); 9 → the calls above; 10 → calls 1, 2, 3, 5, 7, 11.
- Names used across tasks: `Via`, `Event.via/why`, `WATCH_*_DEFAULT`, `check_watch_policy`, `check_modes_list` (1 → 2, 3, 6, 9); `fitting_rows`, `start_direct`, `Tracked.fits/waiting`, `Run.candidates` (2 → 3, 4); `Cooled`, `rank`, `unproven_by_ns` (3 → 4); `Known`, `band_assignments`, `BandAssignment` (4); `Access`, `ModeRow.access`, `envelope` (5 → 11, 13); `SetFile`, `Member`, `SetRegistry`, `LoadedSet`, `SetInfo`, `SetRunning`, `SetError`, `is_set_id` (6 → 8, 9, 12); `Decision`, `decide`, `PlanChannels`, `plan_check`, `plan_attach_check`, `plan_existing`, `plan_start_channels`, `plan_attach` (7 → 8, 9); `SetInstance`, `SetStarted`/`SetExisting`/`SetRefused`/`SetRadio`, `OpResult::SetStart/SetStopped` (8 → 9, 10, 12); `groupOf`, `groupText` (11 → 12).
- Where the spec no longer matches the code (reported to the caller too): `foldPlan` is an idleness fold over more than `JOBS_FOLD_MIN` channels, not a mechanism a membership fold can take a key on (spec 4.7 as first written; fixed in the spec, call 5); `job_list` records already carry `context`, so the `seedSquelch`-style read the spec proposed is unnecessary (fixed, call 6); `menu.js`'s entries are a static `ENTRIES` list and `modes_list` is read by `subjects.js`, not the menu (fixed); `prefer[<i>]` is checked by `Node::check_prefer` at `job_start_gated` and `job_graph_set`, not by `graph::validate` (spec 4.1 says "as `prefer[<i>]` ... today", which holds, the path is just named here); `KnowledgeProvider::allocations` returns a plain `Vec`, so `known.register: "unreachable"` is not implementable without a trait change (call 3, a deferred minor); `plan_start` refuses the whole op on a mode row that is not `Ready` (`kind.mode_row`), so the set's per-member refusal must run before the plan pieces (Task 9); the `SILENCE_MS` test override (`test_timing`) is kept as the default of `hold_ms`, which the spec did not foresee; the e2e `pocsag` fixture is one page in a 48 kHz capture, so the merged-Subjects proof is one decoder, not two.
