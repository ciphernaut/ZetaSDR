# Plan 29: Decoder wave 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Five rows on the plan 16/17/23 frameworks (AIS in-crate, FLEX through multimon-ng, M17 digital voice with Codec 2 through the external `c2dec`, DFM and M20 frame synthesis so `--strict` passes every embedded row, APRS positions on the map), carried by five small generic pieces (`expect` tolerances, handlers on graph rows, a row's `outputs`, row `notes` in AGENTS.md, an `access` marker) and a fifth quick attempt.

**Architecture:** Every decoder is a mode row (`modes/*.json`, embedded through `zeta-core/src/modes/mod.rs`); the in-crate stages live in `zeta-dsp` (`ais.rs`, `m17_decode.rs`, the `aprs_info.rs` parser) and are registered in `zeta_dsp::register_all`, since an embedded graph row may name only the stages the core tests' registry holds; the program rows run through `zeta-ext`'s `external_decode` and its `multimon` parser. Each new row has a synthesiser family in `zeta-dsp/src/synth.rs` (`ais`, `flex`, `m17`, `dfm`, `m20`) written from public descriptions with the external program as the oracle only, and a `test` block `zeta modes check` runs. The one cross-crate move is `find_handler` from `zeta-ext/src/proc.rs` into `zeta-core/src/modes/handlers.rs`, so the probe (`crates/zeta/src/node_setup.rs`) and the M17 stage share one lookup. The page changes are the audio-row rule in `agent.js` and nothing else; the Subjects tab and the map take the rows as they are.

**Tech Stack:** Rust (tokio with `process` added to `zeta-dsp`, serde, schemars; `zeta_dsp::synth`, `ax25::HdlcDeframer`, `pocsag::BitSync`, `resample::Resampler`), vanilla JS page, the Playwright e2e suite in `crates/zeta-ui/tests/e2e/ui.py`, multimon-ng and `codec2` (`c2dec`) as host programs, dfm09mod and m20mod on the field host only.

**Spec:** `docs/superpowers/specs/2026-11-06-decoder-wave2-design.md` (all of it; section 14 holds the operator's decisions of 2026-11-06, section 15 the calls to check while reading). The order of work is the spec's section 13: shared pieces, APRS, AIS, DFM/M20, FLEX, M17 last. Tasks 13 and 14 (M17) may split off as plan 29b if the wave's reviews run long; the plan is complete without them, with `m17_decode` absent rather than half-present.

## Global Constraints

- Every row field is required unless defaulted and unknown fields are rejected (`crates/zeta-core/src/modes/row.rs`: `deny_unknown_fields`, `require_all`); a new row field is `#[serde(default)]` so every existing embedded and operator row keeps loading. `_hz`/`_ms`/`_ns`/`_baud` fields are integers. Every field a row's `summary`, `position`, `time` or `subject` names must be in `event.schema.properties`, else the row is `broken` naming the field.
- A stage an embedded graph row names lives in `zeta-dsp` and is registered in `zeta_dsp::register_all` (`crates/zeta-core/tests/modes.rs` builds its registry from that alone). Stage params that depend on nothing have `#[serde(default)]` so `stage_params_schemas_are_strict` passes.
- One stage per new row from IQ to its outputs; no inner event edge at frame rate (spec section 2 anti-goal: plan 27 feeds every inner event edge to the source's feed).
- No copyleft dependency: `deny.toml` forbids GPL, LGPL and AGPL and CI runs `cargo deny check licenses bans sources`. Codec 2 is the external `c2dec` (the operator's decision). No generator is written from a GPL decoder's source (`rs41mod.c`, `dfm09mod.c`, `m20mod.c`, multimon-ng's `flex.c`/`flex_next.c`): public descriptions only, the program as oracle, the module's doc comment citing its sources first, as `crates/zeta-dsp/src/rs41_frame.rs` does.
- Numbers from the spec are named constants, never literals in logic: `AIS_BAUD = 9600`, `AIS_DEVIATION_HZ = 2400`, `AIS_LOWPASS_FACTOR = 0.6`, `AIS_TRAINING_BITS = 24`, `AIS_SENTENCE_CHARS = 60`, `AIS_CHANNEL_A_HZ = 161_975_000`, `AIS_CHANNEL_B_HZ = 162_025_000`, `FLEX_DEVIATION_HZ = 4800`, `FLEX_FRAME_MS = 1875`, `M17_BAUD = 4800`, `M17_DEVIATION_HZ = 2400`, `M17_RRC_ALPHA = 0.5`, `C2_FRAME_BYTES = 8`, `C2_SAMPLES_3200 = 160`, `C2_SAMPLES_1600 = 320`, `C2_RATE_HZ = 8000`, `AUDIO_RATE_HZ = 48_000`, `APRS_TRACK_POINTS = 100`, `APRS_TRACK_MIN_M = 50.0`.
- No recording enters the repository (the operator's decision): every fixture is synthesised; a sonde or FLEX row whose synthesiser misses its task budget keeps `test: null`.
- Pager text stays on disk, unfiltered, as POCSAG does today; the `access` marker is added and nothing reads it (the operator's decision).
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: after any op note, row note, stage or field-craft change run `cargo run -q -p zeta -- agents-md --check`, regenerate (`cargo run -q -p zeta -- agents-md > AGENTS.md`, `cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and commit the result in the same commit. Never edit either by hand. Per-op prose goes in `crates/zeta/src/agents_md.rs` (`note()`), the rest in `docs/agents/field-craft.md`.
- Gates (CLAUDE.md): `cargo fmt -p <crate> -- --check` per crate (the nested worktree breaks `cargo fmt --all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped` line; the suite runs about 474 s of its 600 s deadline, so this wave adds exactly one scenario), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Use a private `CARGO_TARGET_DIR`. Tests under `required-features` need `--features test-fixtures` (`zeta-dsp`, `zeta-core`'s `watch`/`identify`).
- The CI runner has no sonde decoders: `dfm_decode`, `m20_decode` and `rs41_decode` stay `skipped` in CI and `--strict` on the field host is their gate. CI installs `multimon-ng` and `codec2` (apt, both in Ubuntu 24.04) in the task that lands each row's passing test, and the strict step for a row lands in that same task, never before it; a row on the `test: null` fallback is left out of the strict step.
- The repository is public: roles, never private host names or LAN addresses ("the field host", "the local KiwiSDR"), in code, tests, docs and commit messages. The demo station is Brisbane Airport (-27.3842, 153.1175, 4 m): every test position is it or a published specification example. No real licensee's traffic as a fixture.
- Commits go on this branch; one commit per task, message in the repo's style (`Area: what changed`), no trailer.

## Models per task

Opus where a task tunes a demodulator or a code against synthesised signals, writes a generator against an external oracle, or defines an interface several later tasks consume; Sonnet where the brief is complete and the tests pin the behaviour.

| Task | Model | Why |
|---|---|---|
| 1 `expect` tolerances | Sonnet | one function, pinned diff lines |
| 2 the `access` marker | Sonnet | a field, an envelope key, two rows |
| 3 handlers on graph rows | Opus | a cross-crate move and the probe/stage lookup every M17 task relies on |
| 4 a row's `outputs`, the page's audio rule | Sonnet | derived data and a one-line rule |
| 5 row notes in the guide | Sonnet | a rendered block |
| 6 APRS positions | Opus | three position formats against the specification's examples; the e2e scenario |
| 7 AIS messages and the `ais` family | Opus | bit fields, 6-bit armouring, the bit-order self-check |
| 8 the `ais_decode` stage, quick attempt and row | Opus | GMSK recovery from the discriminator |
| 9 DFM frame synthesis | Opus | a generator against an oracle, with a fallback |
| 10 M20 frame synthesis | Opus | as 9 |
| 11 FLEX parser and row | Sonnet | two line formats and a row |
| 12 the `flex` family | Opus | a generator against multimon-ng, with a fallback |
| 13 M17 codes and the `m17` family (29b candidate) | Opus | Golay, Viterbi, the LSF, base-40 |
| 14 the `m17_decode` stage with `c2dec` (29b candidate) | Opus | 4FSK recovery, a child process in a stage, two outputs |
| 15 docs, TODO, UAT, plan series, gate | Sonnet | |

Ledger: `.superpowers/sdd/plan29/progress.md` in the worktree, one line per task with the implementer's and reviewer's verdicts; the rulings made on the operator's behalf go into the final report.

## Review Focus

Inputs the spec implies but no obvious test exercises; each gets a test in the task that owns the code:

1. A type 5 AIS static report is 424 bits, 71 armoured characters: `encode_sentences` must split it at 60 characters into a two-fragment `!AIVDM,2,1,..` / `!AIVDM,2,2,..` pair with the fill bits on the last, as receivers print it, or the `nmea` field lies. (Task 7: `a_type_5_payload_spans_two_sentences`.)
2. An APRS packet whose info starts with a position marker but carries no valid position (`!garbage`, a truncated `=4903.5`) must still be an event with `src`, `dst`, `info` and `data_type: position`, `lat`/`lon` null; never a dropped packet, never a fix at 0,0. (Task 6: `a_malformed_position_gives_no_fix_and_keeps_the_packet`.)
3. `c2dec` exiting mid-stream must fail nothing but that stream's audio: the LSF and EOT events keep coming, `job_context.m17.vocoder` says why once, and the next LSF restarts it. (Task 14: `a_dead_vocoder_fails_no_event`.)
4. A graph row whose handler is missing must be refused at `job_start` as `unavailable` with the probe's reason, exactly as a program row is, never launched to fail inside the stage. (Task 3: `job_start_refuses_an_unavailable_graph_row`.)
5. A tolerance `expect` with a non-number `near`, a negative `within`, or an event field that is a string must fail the row with a diff line naming the field, never panic the check. (Task 1: `compare_rejects_a_bad_tolerance_and_a_non_number`.)

---

### Task 1: `expect` tolerances in `modes check`

**Files:**
- Modify: `crates/zeta/src/modes_check.rs` (`compare`, about line 300)
- Test: `crates/zeta/src/modes_check.rs` tests module (new)

**Interfaces:**
- Consumes: `compare(expect: &Value, event: &Value) -> Outcome` (private, exact equality per key today).
- Produces: `compare` accepts, for any key, the object `{"near": N, "within": D}` (both numbers, `D >= 0`): the event's value must be a number with `|got - N| <= D`; every other expected value stays exact equality. `fn expected_text(want: &Value) -> String`: `within 0.0001 of -27.3842` for a tolerance, the compact JSON otherwise. The diff line is `<key>: expected <expected_text>, got <got|missing>`; a malformed tolerance (`near` not a number, `within` not a number or negative) is `<key>: expect.near must be a number` / `<key>: expect.within must be a non-negative number`. Row test blocks then write `"lat": {"near": -27.3842, "within": 0.0001}`.

- [ ] **Step 1: Write the failing tests** (a `#[cfg(test)] mod tests` in `modes_check.rs`; `Outcome` gains `#[derive(Debug, PartialEq)]`)

```rust
#[test] fn compare_accepts_a_value_within_the_tolerance() { /* expect {"lat": {"near": -27.3842, "within": 0.0001}, "id": "X"}, event {"lat": -27.38417, "id": "X", "t_ns": 1} → Outcome::Ok */ }
#[test] fn compare_rejects_outside_with_the_diff_line() { /* event lat -27.39 → Fail("lat: expected within 0.0001 of -27.3842, got -27.39") */ }
#[test] fn compare_rejects_a_bad_tolerance_and_a_non_number() { /* event lat "x" → Fail("lat: expected within 0.0001 of -27.3842, got \"x\""); expect {"near": "a", "within": 1} → Fail("lat: expect.near must be a number"); {"near": 1, "within": -1} → Fail("lat: expect.within must be a non-negative number"); a missing key → "lat: expected within 0.0001 of -27.3842, got missing" */ }
#[test] fn compare_keeps_exact_equality_for_other_objects() { /* expect {"path": ["A","B"]} against event path ["A","B"] → Ok; {"near": 1} alone (no within) is an ordinary object compared exactly → Fail naming the key */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta --bin zeta modes_check::tests` — Expected: FAIL (the tolerance form compares unequal; `Outcome` lacks `PartialEq`).
- [ ] **Step 3: Implement** `expected_text` and the per-key match in `compare`; a tolerance object is exactly the two keys `near` and `within`.
- [ ] **Step 4: Run** — `cargo test -p zeta --bin zeta modes_check::tests` and `cargo test -p zeta --test modes_check` — Expected: PASS (the integration tests are unchanged: no embedded row uses a tolerance yet; Task 6 adds the first).
- [ ] **Step 5: Commit**

```bash
git add crates/zeta/src/modes_check.rs
git commit -m "Modes check: an expect field may be {near, within} for values that never round-trip exactly"
```

---

### Task 2: The `access` marker on a row and its events

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`ModeRow`), `crates/zeta-core/src/modes/summary.rs` (`envelope`), `crates/zeta-core/src/modes/mod.rs` (`placeholder`), `crates/zeta-core/src/ops.rs` (`ModeInfo` and its `From`), `modes/pocsag_decode.json`, `crates/zeta/src/agents_md.rs` (`note("modes_list")`, one sentence), `docs/agents/field-craft.md` ("What the events look like": one sentence), `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated)
- Test: `crates/zeta-core/src/modes/summary.rs` tests, `crates/zeta-core/tests/modes.rs`

**Interfaces:**
- Produces: `ModeRow.access: Option<String>` (`#[serde(default)]`, doc: "`null` (the default) or `'restricted'`: the row's events carry content a deployment may want to show to fewer people than the spectrum (pager text); `'restricted'` adds `access: 'restricted'` to each event"). `summary::envelope` inserts `"access": "restricted"` into the fields when `row.access` is `Some("restricted")`, nothing otherwise (not `null`). `ModeInfo.access: Option<String>` (from `m.row.access`, skipped when `None`). `pocsag_decode.json` gains `"access": "restricted"` (Task 11's `flex_decode` is the other). The `modes_list` note gains: "`access` is `'restricted'` on a row whose events carry content a later access control may gate (pager text); such events carry `access: 'restricted'`." The field craft's "What the events look like" paragraph gains the same sentence in the operator's voice. Plan 31's Task 5 defines the same marker (same field, same shape, same wording); whichever plan lands on main second reuses the field as landed and drops its own duplicate, so only the row edits remain.

- [ ] **Step 1: Write the failing tests**

```rust
// summary.rs tests
#[test] fn a_restricted_row_marks_its_events() { /* row(|v| v["access"] = "restricted".into()); envelope(.., json!({"address": 1})) has "access": "restricted" */ }
#[test] fn a_plain_row_adds_no_marker() { /* the default row: ev.get("access").is_none() */ }
// tests/modes.rs
#[test] fn only_the_pager_rows_are_restricted() { /* ModeRegistry::load(&stages(), None): the ids with row.access == Some("restricted") are ["pocsag_decode"] (Task 11 adds flex_decode) */ }
#[tokio::test] async fn modes_list_carries_access() { /* serde_json::to_value(pocsag item)["access"] == "restricted"; aprs item has no `access` */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core modes` — Expected: FAIL to compile (no field `access`).
- [ ] **Step 3: Implement** the field, the envelope key, `ModeInfo`, the row file, the two sentences; regenerate `AGENTS.md` and the skill.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test modes`, `cargo test -p zeta-core summary`, `cargo test -p zeta --test agents_md`, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core modes docs/agents/field-craft.md crates/zeta/src/agents_md.rs AGENTS.md skills
git commit -m "Modes: an access marker on a row and its events (pocsag_decode first), the hook for a later access control"
```

---

### Task 3: Handlers on graph rows, and one handler lookup in `zeta-core`

**Files:**
- Create: `crates/zeta-core/src/modes/handlers.rs`
- Modify: `crates/zeta-core/src/modes/row.rs` (`Decoder::Graph`, `validate_graph`'s caller), `crates/zeta-core/src/modes/mod.rs` (`pub mod handlers;`, re-exports, `placeholder`), `crates/zeta-ext/src/proc.rs` (the lookup moves out; `pub use zeta_core::modes::handlers::{find_handler, find_program, handler_vars};` keeps callers), `crates/zeta/src/node_setup.rs` (`probe_handlers`), `crates/zeta/src/modes_check.rs` (`check_row`'s skip rule and the `checked` record), `crates/zeta-dsp/Cargo.toml` (tokio `process`, `io-util`), `docs/agents/field-craft.md` ("Decoding modes": one sentence on `handlers`)
- Test: `crates/zeta-core/src/modes/row.rs` tests, `crates/zeta-core/src/modes/handlers.rs` tests (moved from `proc.rs`), `crates/zeta-core/tests/modes.rs`, `crates/zeta/src/node_setup.rs` tests, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Consumes: `proc.rs`'s `handler_vars`, `find_handler`, `find_program`, `is_executable` and their three tests (standard library only).
- Produces in `handlers.rs`: the four functions as they are; `pub fn resolve(row: &ModeRow, index: usize) -> Result<PathBuf, String>`: the `index`th handler of a graph row found with `find_handler`, else `Err("<id>: handler <programs[0]> not found (tried <programs joined>)")`, `Err("<id>: no handler <index>")` when the row has none.
- Produces in `row.rs`: `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] #[schemars(deny_unknown_fields, rename = "ModeHandler", transform = require_all)] pub struct Handler { pub programs: Vec<String>, pub install: Install }`; `Decoder::Graph { nodes, edges, #[serde(default)] handlers: Vec<Handler> }`; `validate` refuses an empty `handlers[i].programs` naming `decoder.handlers[<i>].programs`; `impl Decoder { pub fn handler_blocks(&self) -> Vec<(&[String], &Install)> }`: a program row's one block, a graph row's handlers in order (empty for a plain graph row).
- Produces in `node_setup.rs`: `probe_handlers` groups by `programs` across both kinds of row (`handler_blocks`), one report line per distinct list as today, `set_program` for each row in the group (a graph row with two handlers is unavailable when either is missing; its `program` is its first handler's path). The report line format is unchanged: `zeta: handlers: c2dec: missing (apt install codec2; https://github.com/drowe67/codec2) — modes: m17_decode`.
- Produces in `modes_check.rs`: `check_row` skips any `Unavailable` row as `skipped: <first program of handler_blocks()[0]> missing` (the `Decoder::Program` test goes); a pass is recorded in `modes-checked.json` for any row with a non-empty `handler_blocks()`.

- [ ] **Step 1: Write the failing tests**

```rust
// row.rs tests (edited(POCSAG, ..) helper as the file uses)
#[test] fn a_graph_row_may_name_handlers() { /* decoder.handlers = [{"programs": ["c2dec"], "install": {"debian": "codec2", "url": "https://github.com/drowe67/codec2"}}]: parse+validate Ok; row.decoder.handler_blocks().len() == 1; a row without the key has handlers == [] */ }
#[test] fn an_empty_handler_program_list_is_broken_naming_the_field() { /* handlers [{"programs": [], "install": {"url": "x"}}] → RowError field "decoder.handlers[0].programs" */ }
#[test] fn a_handler_with_an_unknown_key_is_broken() { /* {"programs": ["a"], "install": {"url": "x"}, "bogus": 1} → field contains "bogus" */ }
// handlers.rs tests: the three moved from proc.rs unchanged, plus
#[test] fn resolve_names_the_missing_handler() { /* a pocsag row with handlers [{"programs": ["zeta-no-such-handler"], ..}] and PATH "/nonexistent": Err("pocsag_decode: handler zeta-no-such-handler not found (tried zeta-no-such-handler)"); index 1 → Err("pocsag_decode: no handler 1") */ }
// tests/modes.rs
#[tokio::test] async fn job_start_refuses_an_unavailable_graph_row() { /* registry loaded, set_program("pocsag_decode", None, Some("c2dec: missing (apt install codec2; u) — modes: pocsag_decode")), Node::open_with_modes(cfg, stages, ports, Some(Arc::new(reg))): job_start kind pocsag_decode → OpError::Other whose message contains "unavailable" and "c2dec: missing" */ }
// node_setup.rs tests
#[tokio::test] async fn probe_marks_a_graph_row_with_a_missing_handler_unavailable() { /* an operator dir with pocsag_decode copied as "pocsag_voc" plus handlers [{"programs": ["zeta-no-such-handler"], "install": {"debian": "nothing", "url": "https://example.invalid"}}]; ModeRegistry::load, probe_handlers: the row is Unavailable, reason "zeta-no-such-handler: missing (apt install nothing; https://example.invalid) — modes: pocsag_voc", program None; the report has one line for it; pocsag_decode stays Ready */ }
// tests/modes_check.rs
#[test] fn a_graph_row_with_a_missing_handler_is_skipped() { /* write_row-style operator row "pocsag_voc" as above with pocsag's test block: `modes check pocsag_voc` prints "pocsag_voc: skipped: zeta-no-such-handler missing", exit 0; --strict → "fail: zeta-no-such-handler missing", exit 1 */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core modes`, `cargo test -p zeta` — Expected: FAIL to compile (`handlers`, `Handler`, `handler_blocks`, `resolve`).
- [ ] **Step 3: Implement** the move (git shows `proc.rs` shrinking, `handlers.rs` appearing), the row field, the validation, the probe, the check's skip rule, the tokio features, the field-craft sentence ("A graph row may list `handlers`, programs a stage of it needs on the host; a missing one makes the row `unavailable` like a program row's program."); regenerate.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test modes`, `cargo test -p zeta-core modes::`, `cargo test -p zeta-ext`, `cargo test -p zeta --test modes_check --test agents_md`, `cargo test -p zeta --bin zeta node_setup`, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core crates/zeta-ext crates/zeta crates/zeta-dsp/Cargo.toml Cargo.lock docs/agents/field-craft.md AGENTS.md skills
git commit -m "Modes: handlers on graph rows (programs a stage needs, probed like a program row's), find_handler moves into zeta-core so the probe and a stage share one lookup"
```

---

### Task 4: What a row's job emits (`outputs`), and the page's audio-row rule

**Files:**
- Modify: `crates/zeta-core/src/modes/template.rs` (`pub fn outputs`), `crates/zeta-core/src/modes/mod.rs` (`LoadedMode.outputs`, set in `add`), `crates/zeta-core/src/ops.rs` (`ModeInfo.outputs`; the `job_attach` doc text), `crates/zeta/src/agents_md.rs` (`note("modes_list")`: `outputs`), `crates/zeta-ui/web/agent.js` (`isAudioJob`, the `audioKind` option of `startJobs`), `crates/zeta-ui/web/app.js` (passes `audioKind`), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-core/tests/modes.rs`; the page rule in Task 6's scenario (one `page.evaluate`)

**Interfaces:**
- Produces: `template::outputs(row: &ModeRow, stages: &StageRegistry) -> Vec<StreamKind>`: for a graph row the stream kinds of its terminal nodes (nodes with no outgoing edge) from each stage's `spec().outputs`, in node order, deduplicated; for a program row `[StreamKind::Event]`. `LoadedMode.outputs: Vec<StreamKind>` (`#[serde(default)]`; empty for a broken row), filled in `ModeRegistry::add` after validation. `ModeInfo.outputs: Vec<StreamKind>`. The `job_attach` doc on `Op::JobAttach` says "a job whose kind emits audio: the voice demodulators and any mode row whose `outputs` lists `audio`". `agent.js`: `export function isAudioJob(job, rowOf) -> boolean` (`AUDIO_KINDS.includes(job.kind) || !!rowOf(job.kind)?.outputs?.includes("audio")`); `startJobs(..., { audioKind = null })` passes `rowOf` through to `jobRow`, which uses `isAudioJob(job, rowOf)` where it tests `AUDIO_KINDS.includes(job.kind)` today (`agent.js:546`); `app.js` passes `audioKind: (kind) => state.subjects?.rowOf(kind) ?? null` (the store's rows are the `modes_list` items, which carry `outputs`).

- [ ] **Step 1: Write the failing tests**

```rust
// tests/modes.rs
#[test] fn every_embedded_row_emits_events_today() { /* ModeRegistry::load(&stages(), None): each m.outputs == [StreamKind::Event] (Task 14 makes m17_decode [Event, Audio]) */ }
#[tokio::test] async fn modes_list_reports_outputs() { /* the aprs item: v["outputs"] == ["event"] */ }
#[test] fn outputs_of_a_graph_row_are_its_terminal_stages_kinds() { /* an operator row: pocsag's nodes plus a second terminal node {"id": "fm", "kind": "nbfm_demod", "params": {"deviation_hz": 3000, "audio_rate_hz": 48000, "squelch": false}} fed from channel by an iq edge: outputs == [Event, Audio] (pocsag_decode's [Event], then nbfm_demod's spec().outputs [Audio, Event], deduplicated in order) */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core --test modes outputs` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** `outputs`, the field, `ModeInfo`, the two doc texts, `isAudioJob` and the wiring; regenerate.
- [ ] **Step 4: Run** — `cargo test -p zeta-core --test modes`, `cargo test -p zeta --test agents_md`, `cargo run -q -p zeta -- agents-md --check`; `ZETA_UI_ONLY=aprs_ribbon_appears cargo test -p zeta --test ui` (the page still renders) — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core crates/zeta/src/agents_md.rs crates/zeta-ui/web AGENTS.md skills
git commit -m "Modes: a row's outputs (the stream kinds its job emits) in modes_list; the page gives play and Transcribe to any row that emits audio"
```

---

### Task 5: Row notes rendered into the guide

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` (`render`: a `## Mode rows` block after `## Stage kinds`, before `## Field craft`), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta/tests/agents_md.rs`

**Interfaces:**
- Produces: `render` emits `\n## Mode rows\n\nThe built-in decoder rows with notes (`modes_show` has every row in full; an operator row's notes reach `modes_show` only):\n` then one line `- \`<id>\`: <notes>\n` per embedded row (`ModeRegistry::embedded()`, each parsed with `zeta_core::modes::parse`) that has `notes`, in file order. The heading is `Mode rows`, not `Decoding modes`, so it does not collide with the field craft's `### Decoding modes` anchor.

- [ ] **Step 1: Write the failing test**

```rust
#[test] fn guide_lists_the_embedded_rows_notes() { /* md contains "## Mode rows\n"; contains "- `rs41_decode`: " and "- `dfm_decode`: " and "- `m20_decode`: "; the Mode rows heading sits after "## Stage kinds\n" and before "## Field craft\n" */ }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p zeta --test agents_md guide_lists` — Expected: FAIL.
- [ ] **Step 3: Implement** the block; regenerate.
- [ ] **Step 4: Run** — `cargo test -p zeta --test agents_md`, `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta/src/agents_md.rs AGENTS.md skills
git commit -m "Agents guide: the embedded rows' notes rendered as a Mode rows section (the row schema promised it)"
```

---

### Task 6: APRS positions on the map

**Files:**
- Create: `crates/zeta-dsp/src/aprs_info.rs`
- Modify: `crates/zeta-dsp/src/lib.rs` (`pub mod aprs_info;`), `crates/zeta-dsp/src/aprs.rs` (the event's fields), `crates/zeta-ext/src/parsers/kiss.rs` (the same fields), `modes/aprs_decode.json` (schema, `position`, `subject`, `test`, `notes`), `crates/zeta-core/tests/modes.rs` (`the_rows_thin_their_tracks_as_planned`, `the_rows_name_the_planned_subjects`), `crates/zeta/tests/ui.rs` (the `aprs` fixture's info), `crates/zeta-ui/tests/e2e/ui.py` (one scenario), `docs/agents/field-craft.md` ("Decoding APRS and reading measurements": the position sentence), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-dsp/src/aprs_info.rs` tests, `crates/zeta-dsp/src/aprs.rs` tests, `crates/zeta-ext/src/parsers/kiss.rs` tests, `crates/zeta/tests/modes_check.rs` (the embedded check now proves the position), `ui.py`

**Interfaces:**
- Produces in `aprs_info.rs`: `#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)] #[serde(rename_all = "snake_case")] pub enum DataType { Position, MicE, Object, Item, Message, Status, Telemetry, Weather, Other }`; `#[derive(Clone, Debug, Default, PartialEq)] pub struct AprsInfo { pub data_type: DataType (default Other), pub lat: Option<f64>, pub lon: Option<f64>, pub alt_m: Option<f64>, pub course_deg: Option<f64>, pub speed_kt: Option<f64>, pub symbol: Option<String>, pub comment: Option<String>, pub object: Option<String>, pub ambiguity: u8 }`; `pub fn parse_info(dst: &str, info: &[u8]) -> AprsInfo` (never fails: an unreadable position is its `data_type` with the fix fields null); `pub fn fields(&self) -> serde_json::Map<String, Value>` (the nine new event fields: `lat`, `lon`, `alt_m`, `course_deg`, `speed_kt`, `symbol`, `comment`, `data_type`, `object`; `ambiguity` is not an event field). Formats per spec 9.2: uncompressed (`!`, `=`, `/`+7-char timestamp, `@`+timestamp; `ddmm.hhN` / `dddmm.hhW`; spaces as ambiguity, the fix at the cell's centre; `ccc/sss` course/speed right after the symbol code; `/A=aaaaaa` feet anywhere in the comment, converted with `0.3048`), compressed (Base-91 `YYYYXXXX`, symbol, `cs` and `T` bytes: course/speed when the compression type's bits 3-4 say so, altitude when they say altitude, else nothing), Mic-E (destination digits and bits for latitude, N/S, longitude offset, E/W; the first eight info bytes for longitude, speed, course, symbol). Objects `;` and items `)` set `data_type` and `object` (the name trimmed) and no fix. `:` message, `>` status, `T#` telemetry, `_` weather set `data_type` only.
- Produces in `aprs.rs`: the event is the existing six fields plus `parse_info(&f.dst, &f.info).fields()`; the `summary` is unchanged.
- Produces in the row: schema properties for the nine fields (`lat`, `lon`, `alt_m`, `course_deg`, `speed_kt`: `["number", "null"]`; `symbol`, `comment`, `object`: `["string", "null"]`; `data_type`: string enum of the nine names), `"position": {"lat": "lat", "lon": "lon", "alt_m": "alt_m"}`, `subject.heading: "course_deg"`, `subject.track_points: 100`, `subject.track_min_m: 50`, `notes` ("APRS at 144 800 000 Hz in AU and EU, 144 390 000 Hz in the US, 12 500 Hz band; one job hears every station on the channel. Positions in the three common formats (uncompressed, compressed, Mic-E) land on the map; objects and items are named, not placed."), and `test.info` `"!2723.05S/15307.05E>ZetaSDR"` with `expect` gaining `"lat": {"near": -27.3842, "within": 0.0001}, "lon": {"near": 153.1175, "within": 0.0001}` (Task 1's form).
- Produces in `ui.rs`: the `aprs` fixture's info becomes `"!2723.05S/15307.05E>ZetaSDR"` (the `rebuild` fixture too; `scenario_aprs_ribbon_appears` asserts the roster line `aprs 1 stations, last VK3ABC`, which stands). `cli.rs`'s own fixture keeps `>hi`.

- [ ] **Step 1: Write the failing tests**

```rust
// aprs_info.rs (the APRS 1.0.1 specification's examples; confirm each digit against the specification before pinning)
#[test] fn uncompressed_position_with_symbol_and_comment() { /* parse_info("APRS", b"!4903.50N/07201.75W-Test 001234"): Position, lat 49.058333 ±1e-6, lon -72.029167 ±1e-6, symbol "/-", comment "Test 001234", course/speed/alt None */ }
#[test] fn uncompressed_with_timestamp_course_speed_and_altitude() { /* b"/092345z4903.50N/07201.75W>088/036/A=001234": course 88.0, speed 36.0, alt_m 376.1 ±0.1 */ }
#[test] fn ambiguity_centres_the_cell() { /* b"!4903.5 N/07201.7 W-": ambiguity 1, lat 49.058333+0.000833 (the cell 49°03.50'-03.59' centred at 03.55') ±1e-5 */ }
#[test] fn brisbane_airport_round_trips() { /* b"!2723.05S/15307.05E>ZetaSDR": lat -27.3842 ±0.0001, lon 153.1175 ±0.0001 */ }
#[test] fn compressed_position() { /* b"=/5L!!<*e7>7P[": lat 49.5 ±0.0001, lon -72.75 ±0.0001, symbol "/>" */ }
#[test] fn mic_e_from_the_destination_and_info() { /* parse_info("S32U6T", b"`(_fn\"Oj/"): MicE, lat 33.427333 ±1e-4, lon -112.1275 ±1e-4, speed 20.0 ±0.5, course 251.0 ±1 (chapter 10's example) */ }
#[test] fn objects_and_items_are_named_not_placed() { /* b";LEADER   *092345z4903.50N/07201.75W>088/036": Object, object "LEADER", lat None; b")AID #2!4903.50N/07201.75WA": Item, object "AID #2", lat None */ }
#[test] fn messages_status_telemetry_weather_set_the_type_only() { /* ":WU2Z     :Testing{003" Message; ">status" Status; "T#005,..." Telemetry; "_10090556c220s004g005t077" Weather; all fix fields None */ }
#[test] fn a_malformed_position_gives_no_fix_and_keeps_the_packet() { /* b"!garbage" and b"=4903.5": Position, lat None, lon None (the stage test below asserts the event still arrives with src/dst/info) */ }
// aprs.rs tests: the existing synthesised-frame test asserts the new fields: a frame with info "!2723.05S/15307.05E>ZetaSDR" gives lat/lon within 0.0001 and data_type "position"; info ">hi" gives data_type "status" and lat null.
// kiss.rs: a_tnc_frame_reads_as_aprs gains the position fields for a positioned info.
// tests/modes.rs: the_rows_thin_their_tracks_as_planned has ("aprs_decode", 100, 50.0); the_rows_name_the_planned_subjects has heading Some("course_deg") for aprs.
```

```python
# ui.py (after the map scenarios; uses OSM_TILES, page_errors, select_source, start_job, stop_job, run_cmd, view_line, job_row)
def scenario_map_tracks_an_aprs_station(page, url, secret):
    page.route(OSM_TILES, lambda r: r.abort())
    errors = page_errors(page)
    select_source(page, url, secret, "aprs")
    run_cmd(page, "view set map on")
    expect(view_line(page, "map = on")).to_be_visible()
    mp = page.locator("#map")
    job = start_job(url, secret, "aprs_decode", "aprs", 144_800_000, 12_500)
    try:
        job_row(page, job)
        expect(mp.locator(".leaflet-interactive")).to_have_count(1, timeout=15_000)
        mp.locator(".leaflet-interactive").click()
        expect(mp.locator(".leaflet-popup-content")).to_contain_text("VK3ABC")
        page.locator('#panes .pane-tab[data-pane="subjects"]').click()
        row = page.locator('#subjects tr.subject[data-key="VK3ABC"]')
        expect(row).to_be_visible()
        expect(row).to_contain_text("-27.38")
        # The audio-row rule (Task 4): a row whose outputs list audio counts as an audio job.
        audio = page.evaluate(
            """() => import('./agent.js').then((m) => [
                 m.isAudioJob({kind: 'm17_decode'}, () => ({outputs: ['event', 'audio']})),
                 m.isAudioJob({kind: 'aprs_decode'}, () => ({outputs: ['event']})),
                 m.isAudioJob({kind: 'fm_demod'}, () => null)])""")
        assert audio == [True, False, True], f"isAudioJob {audio}"
        run_cmd(page, "view set map off")
        assert not errors, f"console errors: {errors}"
    finally:
        stop_job(url, secret, job)
```

(Adapt the Subjects tab selector to what `scenario_subjects_tab_appears_only_with_a_subject_job` clicks; the pane tab's attribute is whatever `panes.js` sets today.)

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures aprs` — Expected: FAIL to compile (no module `aprs_info`); `ZETA_UI_ONLY=map_tracks_an_aprs_station cargo test -p zeta --test ui` — Expected: FAIL (no marker).
- [ ] **Step 3: Implement** `aprs_info.rs`, the stage and `kiss` merges, the row, the fixture, the scenario, the field-craft sentence ("A packet with a position in any of the three common formats carries `lat`, `lon`, `alt_m`, `course_deg`, `speed_kt` and `position`, so the station lands on the map and in the Subjects table; `data_type` says what the packet was."); regenerate.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures aprs`, `cargo test -p zeta-ext kiss`, `cargo test -p zeta-core --test modes`, `cargo test -p zeta --test modes_check --test cli --test agents_md`, `cargo test -p zeta --test ui` (whole suite, under 600 s) — Expected: PASS; `aprs_decode: ok` with the position proved.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp crates/zeta-ext crates/zeta-core/tests crates/zeta/tests crates/zeta-ui/tests modes/aprs_decode.json docs/agents/field-craft.md AGENTS.md skills
git commit -m "APRS: positions parsed from info (uncompressed, compressed, Mic-E), the row maps them so stations land on the map and in the Subjects table"
```

---

### Task 7: AIS messages and the `ais` synthesiser family

**Files:**
- Create: `crates/zeta-dsp/src/ais_msg.rs`
- Modify: `crates/zeta-dsp/src/lib.rs` (`pub mod ais_msg;`), `crates/zeta-dsp/src/synth.rs` (`SynthSpec::Ais`, `validate`, `rate_hz`, `synthesise`, `ais_iq`), `crates/zeta-ext/src/parsers/nmea.rs` (decodes with `ais_msg`), `docs/agents/field-craft.md` ("Checking a row": the family list gains `ais`)
- Test: `crates/zeta-dsp/src/ais_msg.rs` tests, `crates/zeta-dsp/src/synth.rs` tests, `crates/zeta-ext/src/parsers/nmea.rs` tests

**Interfaces:**
- Produces in `ais_msg.rs`: `pub fn unarmour(payload: &str) -> Option<Vec<u8>>` (moved from `nmea.rs`: six bits per character `0`-`W`, `` ` ``-`w`) and `pub fn armour(bits: &[u8]) -> (String, u8)` (the payload text and its fill bits); `#[derive(Clone, Debug, Default, PartialEq)] pub struct AisMsg { pub msg_type: u8, pub repeat: u8, pub mmsi: u32, pub lat: Option<f64>, pub lon: Option<f64>, pub sog_kt: Option<f64>, pub cog_deg: Option<f64>, pub heading_deg: Option<u16>, pub nav_status: Option<u8>, pub name: Option<String>, pub callsign: Option<String>, pub destination: Option<String>, pub imo: Option<u32>, pub ship_type: Option<u8>, pub aton_type: Option<u8>, pub virtual_aton: Option<bool> }`; `pub fn decode(bits: &[u8]) -> Option<AisMsg>` (None under 38 bits; types 1, 2, 3, 4, 5, 18, 21, 24A/B read; any other type gives `msg_type`, `repeat`, `mmsi` and the rest None; 91/181/102.3/360/511 "not available" become None; strings trimmed of trailing `@` and spaces); `pub fn fields(&self) -> serde_json::Map<String, Value>` (every field of spec 5.2 but `protocol`, `channel`, `nmea`); `pub fn encode_sentences(bits: &[u8], channel: Option<char>) -> Vec<String>` (`!AIVDM,<n>,<i>,<seq>,<ch>,<payload>,<fill>*<cs>`, split at `AIS_SENTENCE_CHARS = 60` payload characters, `seq` empty for one fragment and `0` for several, the NMEA XOR checksum as two upper-case hex digits); encoders for the family: `pub fn position_report_bits(mmsi: u32, lat: f64, lon: f64, sog_kt: f64, cog_deg: f64) -> Vec<u8>` (type 1, 168 bits) and `pub fn static_report_bits(mmsi: u32, name: &str) -> Vec<u8>` (type 5, 424 bits, callsign and destination `@`-filled). The payload bit order is MSB first; the frame on air sends each byte LSB first, so `pub fn payload_to_hdlc_bytes(bits: &[u8]) -> Vec<u8>` reverses each byte's bits (padding the last to eight).
- Produces in `synth.rs`: `SynthSpec::Ais { mmsi: u32, lat: f64, lon: f64, sog_kt: f64, cog_deg: f64, #[serde(default)] name: Option<String>, repeat: u32, #[serde(default = "default_gap_ms")] gap_ms: u64 }`; `validate`: `mmsi` nine digits (`100_000_000..=999_999_999`), lat in -90..=90, lon in -180..=180, `sog_kt` under 102.2, `cog_deg` under 360, `name` at most 20 characters of the AIS 6-bit set (upper-case letters, digits, space and `!"#$%&'()*+,-./:;<=>?`), `repeat` 1..=60; `rate_hz` 48 000; `fn ais_iq(spec, rate_hz) -> Vec<Complex<f32>>`: per repeat, the type 1 frame then (with `name`) the type 5 frame, each `AIS_TRAINING_BITS = 24` of `0101…` then `hdlc_frame_bits(&payload_to_hdlc_bytes(bits), 1, 1)` (flags, stuffing and the X.25 CRC come from the helper), `nrzi_encode`, `gfsk_modulate(&levels, 2, AIS_BAUD, AIS_DEVIATION_HZ as f64, rate_hz)`, frames `gap_ms` apart (50 by default), half a second of silence first (as `aprs_packet_iq` does). Builder's call: `gfsk_modulate` keeps BT 0.5 unless Task 8's stage needs the transmitter's 0.4.
- Produces in `nmea.rs`: `parse` calls `ais_msg::unarmour` and `ais_msg::decode`, and its event is `{channel, payload, fill_bits}` plus `fields()` (so `msg_type` and `mmsi` stay; the doc comment's "Full AIS fields arrive in plan 17" goes).

- [ ] **Step 1: Write the failing tests**

```rust
// ais_msg.rs
#[test] fn the_published_sentence_decodes_to_the_full_fields() { /* unarmour("177KQJ5000G?tO`K>RA1wUbN0TKH"): decode: type 1, mmsi 477553000, lat/lon/sog/cog Some and in range (pin the values the decode gives after checking them against a public decoder's reading of the same sentence, e.g. the one in nmea.rs's test comment) */ }
#[test] fn a_type_5_payload_spans_two_sentences() { /* static_report_bits(503000001, "ZETA TEST"): 424 bits; encode_sentences(.., Some('A')) → 2 sentences, the first "!AIVDM,2,1,0,A," + 60 chars + ",0*", the second 11 chars with fill 2; decode of the re-joined unarmoured bits gives name "ZETA TEST", mmsi 503000001 */ }
#[test] fn a_position_report_round_trips_through_sentences() { /* position_report_bits(503000001, -27.3842, 153.1175, 12.3, 45.0) → 168 bits; encode_sentences gives one "!AIVDM,1,1,,A,…,0*CS"; unarmour+decode: lat within 1e-4, lon within 1e-4, sog 12.3, cog 45.0 */ }
#[test] fn not_available_values_are_null() { /* a type 1 built with lat 91, lon 181, sog 102.3, cog 360, heading 511 → all None */ }
#[test] fn an_unread_type_keeps_mmsi_and_type() { /* bits with type 8 and mmsi 123456789 → msg_type 8, lat None, name None */ }
#[test] fn hdlc_byte_order_round_trips() { /* payload_to_hdlc_bytes then reverse each byte back → the original bits (padded) */ }
// synth.rs tests
#[test] fn ais_family_validates_and_is_48_khz() { /* parse_params of {"family": "ais", "mmsi": 503000001, "lat": -27.3842, "lon": 153.1175, "sog_kt": 12.3, "cog_deg": 45.0, "name": "ZETA TEST", "repeat": 3}: Ok, rate 48000; mmsi 1 → Err containing "mmsi"; sog 150 → Err containing "sog_kt"; name "x".repeat(21) → Err containing "name" */ }
#[test] fn ais_frames_deframe_to_their_parameters() { /* the bit-reversal self-check: ais_iq → nbfm_demod(dev 2400) → BitSync(48000, 9600) → NRZI decode → HdlcDeframer: at least one frame; reverse each byte's bits, decode: mmsi 503000001, lat within 1e-4 (this is the chain Task 8's stage runs; it proves the convention before the stage exists) */ }
// nmea.rs: a_position_report_gives_type_and_mmsi also asserts v["lat"] is a number and v["name"] is Null.
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures ais` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** `ais_msg.rs`, the family, the parser change, the field-craft family list.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures ais synth`, `cargo test -p zeta-ext nmea` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp crates/zeta-ext docs/agents/field-craft.md
git commit -m "AIS: message decoding and sentence encoding (types 1-5, 18, 21, 24), the ais synthesiser family, the nmea parser reads the full fields"
```

---

### Task 8: The `ais_decode` stage, the `ais` quick attempt, the row and the channel plan

**Files:**
- Create: `crates/zeta-dsp/src/ais.rs`, `modes/ais_decode.json`
- Modify: `crates/zeta-dsp/src/lib.rs` (`pub mod ais;`, `register_all`), `crates/zeta-dsp/src/identify.rs` (`quick_attempt`: `"ais" => Some(ais_attempt)`), `crates/zeta-core/src/modes/row.rs` (`QUICK_ATTEMPTS: [&str; 5]`), `crates/zeta-core/src/modes/mod.rs` (`EMBEDDED` 9), `crates/zeta-core/src/plans.rs` (`AIS_VHF`, `PLANS`), `crates/zeta-core/tests/modes.rs` (`EMBEDDED`, counts, `the_rows_thin_their_tracks_as_planned`, `the_rows_name_the_planned_subjects`), `crates/zeta-core/tests/plan_start.rs` (the plan list), `crates/zeta/tests/modes_check.rs` (`modes_check_passes_the_four_in_crate_rows` → `..._the_in_crate_rows` with `ais_decode: ok`; `an_unknown_id_lists_the_known_ones` → `"acars_decode, adsb_decode, ais_decode, aprs_decode"`), `crates/zeta/src/agents_md.rs` (`note("job_start")` one sentence; `note("classify")` and `note("channel_plans")` name five attempts and the `ais_vhf` plan), `docs/agents/field-craft.md` ("Where to look": an AIS line; "Decoding pagers, aircraft and FM data": an AIS bullet; the "(APRS, POCSAG, ACARS, RDS)" list gains AIS; the ordinary-use list in "Exploring an unknown signal" gains AIS), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-dsp/src/ais.rs` tests, `crates/zeta-dsp/tests/classify.rs`, the core and zeta tests above

**Interfaces:**
- Consumes: Task 7's `ais_msg`, `pocsag::BitSync`, `ax25::HdlcDeframer`, `nbfm::nbfm_demod`, `util::{lowpass_taps, StreamingFir}`, `run_map`, `StageContext.band` (the job's `[lo, hi]`, `Some` for a rigid job on a radio), `summary::envelope` through the `event(&ctx, ..)` helper `aprs.rs` uses.
- Produces: `pub const AIS_DECODE_KIND: &str = "ais_decode"`, the constants of the global list; `pub struct AisDecoder { .. }` with `pub fn new(rate_hz: u64) -> Result<Self, String>` (refuses under `MIN_SAMPLES_PER_SYMBOL × AIS_BAUD` naming the rate) and `pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<AisMsg>` (`nbfm_demod` at `AIS_DEVIATION_HZ`, a `StreamingFir` low-pass at `AIS_LOWPASS_FACTOR × AIS_BAUD`, `BitSync::new(rate, AIS_BAUD)`, NRZI decode, `HdlcDeframer`, `payload` bit reversal, `ais_msg::decode`); `pub fn decode_ais_iq(iq: &[Complex<f32>], rate_hz: u64) -> Vec<AisMsg>` (one-shot, for the quick attempt); `#[derive(Default, JsonSchema, Serialize, Deserialize)] #[serde(deny_unknown_fields)] pub struct AisParams {}`; `pub struct AisDecodeStage` (inputs `[Iq]`, outputs `[Event]`): a rate change or a non-IQ frame fails the stage as `aprs_decode` does; a lag resets the decoder; each message is one event of `fields()` plus `channel` (`"A"` when the band's centre is within 12 500 Hz of `AIS_CHANNEL_A_HZ`, `"B"` for `AIS_CHANNEL_B_HZ`, else null) and `nmea` (`encode_sentences(bits, channel)`). `fn ais_attempt(chan, rate) -> Option<String>` in `identify.rs`: `"<n> AIS frames CRC-clean; first MMSI <mmsi> type <t>"`. `plans.rs`: `pub const AIS_VHF: ChannelPlan { id: "ais_vhf", name: "AIS VHF", region: "world", bandwidth_hz: 25_000, segments: [ch 1 at 161_975_000, ch 2 at 162_025_000], notes: [(1, "channel A, 87B"), (2, "channel B, 88B")], no_voice: [1, 2] }` in `PLANS`. The row is spec 5.3 verbatim (`ais_decode`, family `maritime`, band 25 000 Hz at the two centres with `min_hz` 12 500, `modulation ["fsk", "fm"]`, `decoder graph: channel → ais_decode` with the three `$` placeholders, the event schema of spec 5.2 with `mmsi`/`msg_type`/`repeat` integers, `summary "{name?mmsi} {lat?:.4},{lon?:.4} {sog_kt?:.1}"` (the summary template's `{a?b}` fallback form `summary.rs` already parses), `position {lat, lon, alt_m: null}`, `identify {proof: event, min_events 1, dwell_ms 10000, min_span_ms 1000, quick "ais", priority 0}`, catalogue `["ais", "automatic-identification-system-ais"]`, `subject {key mmsi, name "vessels", waterfall summary, columns name/Name, callsign/Callsign, sog_kt/Speed (unit kt), cog_deg/Course (unit deg), destination/Destination, heading cog_deg, label name, track_points 200, track_min_m 50}`, the `notes` of spec 5.3, `test {kind synth, family ais, mmsi 503000001, lat -27.3842, lon 153.1175, sog_kt 12.3, cog_deg 45.0, name null, repeat 3, snr_db 20, expect {mmsi 503000001, msg_type 1, lat {near -27.3842, within 0.0001}, lon {near 153.1175, within 0.0001}}, within_ms 5000}`).

- [ ] **Step 1: Write the failing tests**

```rust
// ais.rs
#[tokio::test] async fn the_synthesised_frame_gives_an_event() { /* ais_iq (Task 7) in IqBlocks through the stage with a mode() row of ais_decode and band Some([161_962_500, 161_987_500]): one event with mmsi 503000001, msg_type 1, channel "A", nmea an array of 1 string starting "!AIVDM,1,1,,A,", protocol "ais", summary "503000001 -27.3842,153.1175 12.3" */ }
#[tokio::test] async fn a_type_5_gives_the_name() { /* name "ZETA TEST": the second event has msg_type 5, name "ZETA TEST", nmea 2 strings */ }
#[test] fn a_low_rate_is_refused_naming_it() { /* AisDecoder::new(24_000) → Err containing "24000" */ }
#[tokio::test] async fn a_rate_change_and_a_non_iq_input_fail_the_stage() { /* as aprs.rs's tests */ }
#[test] fn decode_ais_iq_on_noise_gives_nothing() { /* awgn-only block → empty */ }
#[test] fn the_stage_survives_snr_10_db_with_offset() { /* Impairments snr 10, offset 500 Hz: at least one of 3 repeats decodes */ }
// identify.rs tests (or tests/classify.rs): classify_proves_ais_on_its_channel: a 2 s span of ais_iq at 48 kHz on a 25 kHz band: tried has ais_decode proved with a layer-2 label "AIS".
// tests/modes.rs: EMBEDDED has "ais_decode" (9 ids; modes_list all == 10 with junk); thin-tracks plan gains ("ais_decode", 200, 50.0); subjects plan gains ("ais_decode", "mmsi", "vessels", Summary, &["name","callsign","sog_kt","cog_deg","destination"], Some("cog_deg"), Some("name")).
// tests/plan_start.rs: channel_plans lists "ais_vhf" with 2 channels 25_000 Hz wide; bounding(ais_vhf, all) covers 161_962_500..162_037_500.
// tests/modes_check.rs: "ais_decode: ok" in the in-crate list.
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures ais` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the decoder, the stage, the attempt, the row, the plan, the five-name texts (`note("classify")`: "(APRS, POCSAG, ACARS, RDS, AIS)"; `note("channel_plans")`: "`ais_vhf`: the two AIS channels, 161.975 and 162.025 MHz, 25 kHz each"; `note("job_start")`: "`ais_decode` hears one AIS channel (A or B) and emits one event per message, vessels as subjects by MMSI; `plan_start ais_vhf` runs both."), the field-craft lines; regenerate. Tune the low-pass and the bit clock until the SNR test passes; if `BitSync` cannot hold 9600 baud at 48 kHz (5 samples per symbol), the fallback is a Gardner loop as `clock_recover` runs, called as a library function.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures ais classify identify`, `cargo test -p zeta-core --test modes --test plan_start`, `cargo test -p zeta --test modes_check --test agents_md --test cli`, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS; `cargo run -q -p zeta -- modes check ais_decode` prints `ais_decode: ok`.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp crates/zeta-core crates/zeta modes/ais_decode.json docs/agents/field-craft.md AGENTS.md skills
git commit -m "AIS: the ais_decode stage (GMSK 9600 from IQ, HDLC, the message parser), the ais quick attempt, the row with vessels as subjects, the ais_vhf plan"
```

---

### Task 9: DFM frame synthesis (proven on the field host)

**Files:**
- Create: `crates/zeta-dsp/src/dfm_frame.rs`
- Modify: `crates/zeta-dsp/src/lib.rs`, `crates/zeta-dsp/src/synth.rs` (`SynthSpec::Dfm`, `dfm_iq`), `modes/dfm_decode.json` (`test`), `crates/zeta/tests/modes_check.rs` (`dfm_decode: skipped: dfm09mod missing` in CI; `dfm_decode_passes_its_synthesised_check` when `dfm09mod` is on the host), `docs/agents/field-craft.md` ("Audio-input rows": the "no synthesised test yet" sentence), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-dsp/src/dfm_frame.rs` tests, `crates/zeta-dsp/src/synth.rs` tests, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Produces: `dfm_frame.rs` opens with the doc comment the RS41 rule demands, written before any code: the public descriptions followed (candidates: the brmlab weather-sonde telemetry page, the radiosonde_auto_rx wiki's DFM notes, Graw's data sheets), the conventions they leave open (bit order, Manchester phase, the sync as sent, the Hamming(8,4) nibble coding, where the serial sits in the configuration block and over how many frames) and how each was settled (a published example frame, or the oracle), `dfm09mod` named as the oracle only. `pub struct DfmParams { pub serial: String, pub lat: f64, pub lon: f64, pub alt_m: f64, pub vel_v: f64, pub frame: u32 }`; `pub fn dfm_frame_bits(p: &DfmParams) -> Vec<u8>` (one frame, line-coded as sent); `pub fn manchester_encode(bits: &[u8]) -> Vec<u8>` and `pub fn hamming84_encode(nibble: u8) -> u8` as shared helpers (in `dfm_frame.rs`; the M20 task uses what it needs). `SynthSpec::Dfm { serial, lat, lon, alt_m, vel_v, frames }` (serial as `dfm09mod` prints the id; `frames` 1..=60), rate 48 000, 2-FSK over NBFM at the DFM line rate and deviation the description gives, frames at the real frame period. The row's `test`: `{kind synth, family dfm, serial <a plausible id>, lat -27.3842, lon 153.1175, alt_m 5000, vel_v 4.5, frames 20, expect {id: <serial>, lat: {near: -27.3842, within: 0.001}}, within_ms 15000}` (the id spreads over several frames; builder tunes `within_ms`).

- [ ] **Step 1: Write the doc comment and the failing tests** (the comment first; the tests check codes with in-tree encoders only)

```rust
#[test] fn hamming84_corrects_one_bit() { /* every nibble: encode, flip each of 8 bits, a decode helper in the test recovers the nibble */ }
#[test] fn manchester_doubles_and_round_trips() { /* encode then a test decoder gives the input */ }
#[test] fn a_frame_has_the_documented_layout() { /* dfm_frame_bits length == the frame's bit count ×2 (Manchester); the sync word at the start as documented */ }
// synth.rs: dfm_family_is_bounded_and_at_the_line_rate (frames 0 → Err; 61 → Err; rate 48000; iq length ≈ frames × period)
// modes_check.rs: dfm_decode_passes_its_synthesised_check (skips with a note when `which dfm09mod` is None; asserts "dfm_decode: ok" when present); the in-crate list test asserts "dfm_decode: skipped: dfm09mod missing" (the row now has a test, so the skip reason changes from "no test").
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures dfm` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the generator and the family; prove it on the field host: `ZETA_HANDLER_DFM09MOD=$(which dfm09mod) cargo run -q -p zeta -- modes check --strict dfm_decode` → `dfm_decode: ok`. Record in the doc comment what the oracle settled. The field craft's sentence becomes "`zeta modes check --strict` passes `rs41_decode`, `dfm_decode` and `m20_decode` against the real decoders on a host that has them."
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures dfm synth`, `cargo test -p zeta --test modes_check --test agents_md`, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS.
- [ ] **Step 5: Fallback, only if Step 3 does not reach `ok` within this task's budget:** keep `dfm_frame.rs` and its unit tests, set the row's `test` back to `null`, make the check test assert `dfm_decode: skipped: no test`, leave the field-craft sentence as it was, write what was learned in the task report and in `TODO.md` ("DFM frame generator does not yet satisfy dfm09mod: …"), and commit as `Synth: a DFM frame generator (not yet accepted by dfm09mod; row test deferred)`.
- [ ] **Step 6: Commit**

```bash
git add crates/zeta-dsp crates/zeta/tests/modes_check.rs modes/dfm_decode.json docs/agents/field-craft.md AGENTS.md skills
git commit -m "Synth: a DFM frame generator from the public descriptions, proven against dfm09mod; dfm_decode has a test"
```

---

### Task 10: M20 frame synthesis (proven on the field host)

**Files:**
- Create: `crates/zeta-dsp/src/m20_frame.rs`
- Modify: `crates/zeta-dsp/src/lib.rs`, `crates/zeta-dsp/src/synth.rs` (`SynthSpec::M20`, `m20_iq`), `modes/m20_decode.json` (`test`), `crates/zeta/tests/modes_check.rs`, `AGENTS.md`, `skills/zetasdr/SKILL.md` (if the field craft changed in Task 9's fallback)
- Test: as Task 9

**Interfaces:**
- Produces: `m20_frame.rs` with the same doc-comment-first rule (candidates: the radiosonde_auto_rx wiki's M10/M20 notes, Meteomodem's data sheet; the line: 2-FSK at the M10 rate, a sync header, one frame per second with a CRC-16, settled against the oracle where the descriptions are silent). `pub struct M20Params { serial, lat, lon, alt_m, vel_v, frame }`, `pub fn m20_frame_bits(p: &M20Params) -> Vec<u8>`, `pub fn crc16_m20(data: &[u8]) -> u16` (delegating to `crc::crc` with the spec the description names, or a `CrcSpec` added to `crc.rs` if it is none of the six). `SynthSpec::M20 { serial, lat, lon, alt_m, vel_v, frames }`, rate 48 000. The row's `test`: `expect {id: <serial>, lat: {near: -27.3842, within: 0.001}}`, `within_ms 8000`.

- [ ] **Step 1: Write the doc comment and the failing tests** — `a_frame_has_the_documented_layout`, `the_crc_matches_the_in_tree_table` (never the decoder), `m20_family_is_bounded_and_one_frame_a_second`; `m20_decode_passes_its_synthesised_check` in `modes_check.rs` (host-gated as Task 9); the in-crate list asserts `m20_decode: skipped: m20mod missing`.
- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures m20` — Expected: FAIL to compile.
- [ ] **Step 3: Implement**; prove on the field host: `ZETA_HANDLER_M20MOD=$(which m20mod) cargo run -q -p zeta -- modes check --strict m20_decode` → `m20_decode: ok`.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures m20 synth`, `cargo test -p zeta --test modes_check` — Expected: PASS.
- [ ] **Step 5: Fallback** as Task 9 Step 5, for M20.
- [ ] **Step 6: Commit**

```bash
git add crates/zeta-dsp crates/zeta/tests/modes_check.rs modes/m20_decode.json AGENTS.md skills
git commit -m "Synth: an M20 frame generator from the public descriptions, proven against m20mod; m20_decode has a test"
```

---

### Task 11: FLEX: the multimon parser's FLEX lines and the `flex_decode` row

**Files:**
- Create: `modes/flex_decode.json`
- Modify: `crates/zeta-ext/src/parsers/multimon.rs`, `crates/zeta-core/src/modes/mod.rs` (`EMBEDDED` 10), `crates/zeta-core/tests/modes.rs` (`EMBEDDED`, counts: pagers ready 2 in a node that never probes, `only_the_pager_rows_are_restricted` → `["flex_decode", "pocsag_decode"]`), `crates/zeta/tests/modes_check.rs` (`ZETA_HANDLER_MULTIMON_NG` pinned to `/nonexistent/zeta-test/multimon-ng` in `check()`; `flex_decode: skipped: no test` until Task 12), `crates/zeta/src/agents_md.rs` (`note("job_start")` one sentence), `docs/agents/field-craft.md` (a FLEX bullet under "Decoding pagers, aircraft and FM data"; the handler sentence names multimon-ng beside dump1090; the ordinary-use list gains FLEX), `README.md` ("Legal": FLEX beside POCSAG), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-ext/src/parsers/multimon.rs` tests, the core and zeta tests above

**Interfaces:**
- Produces in `multimon.rs`: `fn flex(line: &str) -> Option<Value>` reading both forms: FLEX_NEXT `FLEX|<time>|<baud>/<levels>/<K|C>/<phase>|<cycle>.<frame>|<capcode>|<ALN|NUM|TON|BIN>|<text>` (split with `splitn(8, '|')` so a `|` in the text survives) and the older `FLEX: <time> <baud>/<levels>/<K> <cycle>.<frame> [<capcode>] <ALN> <text>`; fields `capcode` (integer), `baud`, `levels`, `cycle`, `frame` (integers), `phase` (string or null), `kind` (`alpha`, `numeric`, `tone`, `binary` or null), `text` (string or null; the timestamp dropped). `parse` tries `pocsag`, then `flex`, then `{text}`. The exact lines are pinned against multimon-ng's output on the field host during Task 12's `--strict`; the parser's existing FLEX_NEXT sample is the starting point.
- Produces the row of spec 6.2 verbatim: `flex_decode`, "FLEX pagers", family `pagers`, band `{bandwidth_hz 25000, centers_hz [], min_hz 12500, range_hz null}`, `modulation ["fm", "fsk"]`, label `FLEX`, protocol `flex`, program decoder `programs ["multimon-ng"]`, `args ["-t", "raw", "-a", "FLEX_NEXT", "-q", "-"]`, `demod {kind nbfm, deviation_hz 4800, audio_rate_hz 22050}`, `input {stream audio, format s16, rate_hz 22050, via stdin, container raw}`, `output {via stdout, port null, parser multimon}`, `install {debian "multimon-ng", url "https://github.com/EliasOenal/multimon-ng"}`, the event schema of the eight fields, `summary "{capcode}: {text?}"`, `identify {proof event, min_events 1, dwell_ms 15000, min_span_ms 1000, quick null, priority 0}`, catalogue `["flex"]`, `test: null` (Task 12 fills it), `notes` as spec 6.2, `access: "restricted"`, no `subject`, no `position`.

- [ ] **Step 1: Write the failing tests**

```rust
// multimon.rs
#[test] fn flex_next_lines_give_the_fields() { /* "FLEX|2026-10-28 12:00:00|1600/2/K/A|05.120|001234567|ALN|HELLO ZETA" → {capcode 1234567, baud 1600, levels 2, phase "A", cycle 5, frame 120, kind "alpha", text "HELLO ZETA"}; "...|NUM|0412 555" kind numeric; "...|TON|" kind tone, text null */ }
#[test] fn older_flex_lines_give_the_fields() { /* "FLEX: 2026-10-28 12:00:00 1600/2/K 05.120 [001234567] ALN HELLO ZETA" → the same, phase null */ }
#[test] fn text_with_a_pipe_survives() { /* text "A|B" stays "A|B" */ }
#[test] fn a_flex_line_that_does_not_parse_stays_text() { /* "FLEX|garbled" → {"text": ..} */ }
// other_lines_are_text: the FLEX sample moves out of it (it parses now); POCSAG tests unchanged.
// tests/modes.rs: EMBEDDED 10 ids; pagers ready == 2 (unprobed node); only_the_pager_rows_are_restricted == ["flex_decode", "pocsag_decode"].
// tests/modes_check.rs: "flex_decode: skipped: no test" in the in-crate list; an_unknown_id's known list unchanged (ais precedes aprs; flex is later).
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-ext multimon`, `cargo test -p zeta-core --test modes` — Expected: FAIL.
- [ ] **Step 3: Implement** the parser branch, the row, the prose (`note("job_start")`: "`flex_decode` runs multimon-ng on the node's host over a 25 000 Hz channel at the measured centre, one event per page like POCSAG; its events carry `access: 'restricted'`."; the field-craft bullet: "FLEX (`flex_decode`): 1600, 3200 or 6400 bps pagers, 929-932 MHz in the US, 151.8-153.4 MHz elsewhere per the catalogue; no built-in centre, start it on the measured one. 25 000 Hz band. It needs multimon-ng on the node's host."; README Legal: "APRS, AIS, radiosondes, ADS-B, ACARS, RDS, POCSAG and FLEX pagers, and M17 voice" once Tasks 8 and 14 land, "pager traffic in particular" stays); regenerate.
- [ ] **Step 4: Run** — `cargo test -p zeta-ext`, `cargo test -p zeta-core --test modes`, `cargo test -p zeta --test modes_check --test agents_md`, `cargo run -q -p zeta -- agents-md --check` — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ext crates/zeta-core crates/zeta modes/flex_decode.json docs/agents/field-craft.md README.md AGENTS.md skills
git commit -m "FLEX: the flex_decode row over multimon-ng, the multimon parser reads FLEX lines (both forms); pager events carry access: restricted"
```

---

### Task 12: The `flex` synthesiser family, proven against multimon-ng (CI too)

**Files:**
- Create: `crates/zeta-dsp/src/flex_frame.rs`, `crates/zeta-dsp/src/bch.rs`
- Modify: `crates/zeta-dsp/src/lib.rs`, `crates/zeta-dsp/src/synth.rs` (`pocsag_codeword` → `bch::bch3121_word`; `SynthSpec::Flex`, `flex_iq`), `modes/flex_decode.json` (`test`), `crates/zeta/tests/modes_check.rs` (`flex_decode: skipped: multimon-ng missing`; `flex_decode_passes_its_synthesised_check` host-gated on `which multimon-ng`), `.github/workflows/ci.yml` (`test` job: `sudo apt-get install -y multimon-ng` before the tests, then `cargo run -q -p zeta -- --state-dir "$RUNNER_TEMP/zeta-check" modes check --strict flex_decode`), `docs/agents/field-craft.md` ("Checking a row": the family list gains `flex`)
- Test: `crates/zeta-dsp/src/flex_frame.rs` tests, `crates/zeta-dsp/src/bch.rs` tests, `crates/zeta-dsp/src/synth.rs` tests, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Produces: `bch.rs`: `pub fn bch3121_word(data21: u32) -> u32` (the 32-bit word: 21 data bits, 10 BCH(31,21) check bits from generator `0x769`, even parity; today's `pocsag_codeword` moved and renamed) with `pub fn bch3121_ok(word: u32) -> bool` delegating to `pocsag::bch_ok`. `flex_frame.rs` with the doc-comment-first rule (sources: the sigidwiki FLEX page, the Motorola FLEX patents' frame descriptions, the published protocol specification if available; never multimon-ng's sources): `pub struct FlexParams { pub capcode: u32, pub text: String, pub cycle: u8, pub frame: u8 }`; `pub fn flex_frame_bits(p: &FlexParams) -> Vec<u8>` (one 1.875 s frame at 1600 bps 2-level: bit sync, the A/B sync words for 1600/2, the frame information word, the second sync, the 11 blocks of 8 BCH(31,21) words interleaved as the protocol sends them, holding one short address, its vector word and the alphanumeric message words for `text`, idle words elsewhere). `SynthSpec::Flex { capcode: u32, text: String, baud: u32, repeat: u32 }`: `baud` must be 1600 ("this wave synthesises 1600 bps"), `capcode` in the short-address range the description gives, `text` ASCII at most the words one frame holds, `repeat` 1..=10; rate 48 000; `flex_iq`: 2-FSK at 1600 baud, `±FLEX_DEVIATION_HZ` over NBFM (`pocsag_modulate`-style NRZ then `nbfm_modulate`), `repeat` frames back to back. The row's `test`: `{kind synth, family flex, capcode 1234567, text "HELLO ZETA", baud 1600, repeat 2, expect {capcode 1234567, text "HELLO ZETA"}, within_ms 6000}`.

- [ ] **Step 1: Write the doc comment and the failing tests**

```rust
// bch.rs
#[test] fn bch3121_words_pass_the_pocsag_check() { /* for data in [0, 1, 0x1F_FFFF, 0x12345]: bch3121_ok(bch3121_word(data)); POCSAG's SYNC 0x7CD215D8 and IDLE 0x7A89C197 are words of their own data */ }
// flex_frame.rs
#[test] fn a_frame_is_1875_ms_of_bits_at_1600() { /* flex_frame_bits(..).len() == 3000 */ }
#[test] fn every_block_word_passes_bch() { /* de-interleave in the test, each 32-bit word bch3121_ok */ }
#[test] fn the_address_and_vector_point_at_the_message() { /* the short address word carries capcode + 32768 (the documented offset) and the vector's start/length cover the message words */ }
// synth.rs: flex_family_refuses_other_rates ("baud: this wave synthesises 1600 bps"), is 48 kHz, iq length ≈ repeat × 1.875 s.
// modes_check.rs: flex_decode_passes_its_synthesised_check (host-gated; "flex_decode: ok"); the in-crate list asserts "flex_decode: skipped: multimon-ng missing".
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures flex bch` — Expected: FAIL to compile.
- [ ] **Step 3: Implement**; prove against multimon-ng on the field host and locally (`apt install multimon-ng`): `cargo run -q -p zeta -- modes check --strict flex_decode` → `flex_decode: ok`. Pin the parser's line forms (Task 11) against what multimon-ng actually printed. Add the CI apt and strict steps.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures flex bch pocsag synth`, `cargo test -p zeta --test modes_check` — Expected: PASS.
- [ ] **Step 5: Fallback, only if Step 3 does not reach `ok` within this task's budget:** keep `flex_frame.rs`, `bch.rs` and their tests, the row keeps `test: null` (`skipped: no test`), no CI strict step for FLEX, the findings in the task report and `TODO.md` ("FLEX frame generator does not yet satisfy multimon-ng: …"); commit as `Synth: a FLEX frame generator (not yet accepted by multimon-ng; row test deferred)`.
- [ ] **Step 6: Commit**

```bash
git add crates/zeta-dsp crates/zeta/tests/modes_check.rs modes/flex_decode.json .github/workflows/ci.yml docs/agents/field-craft.md
git commit -m "Synth: a FLEX 1600 bps frame generator (BCH(31,21) shared with POCSAG), flex_decode proven against multimon-ng here and in CI"
```

---

### Task 13: M17 codes, the LSF and the `m17` synthesiser family (29b candidate)

**Files:**
- Create: `crates/zeta-dsp/src/m17.rs`
- Modify: `crates/zeta-dsp/src/lib.rs`, `crates/zeta-dsp/src/synth.rs` (`SynthSpec::M17`, `m17_iq`, `rrc_modulate_4fsk`), `crates/zeta-dsp/src/crc.rs` (`CRC16_M17`), `docs/agents/field-craft.md` ("Checking a row": `m17`)
- Test: `crates/zeta-dsp/src/m17.rs` tests, `crates/zeta-dsp/src/crc.rs` tests, `crates/zeta-dsp/src/synth.rs` tests

**Interfaces:**
- Produces in `crc.rs`: `pub const CRC16_M17: CrcSpec` (`"crc16-m17"`, width 16, poly 0x5935, init 0xFFFF, no reflection, xorout 0, check 0x772B: the M17 specification's check value for `123456789`; confirm it against the specification before pinning) in `CRCS`.
- Produces in `m17.rs` (the M17 Project specification, CC BY-SA, is the source; the MIT crate `m17core` may serve as a test oracle for vectors as a dev-dependency, never as the implementation): `pub const SYNC_LSF: u16 = 0x55F7`, `SYNC_STREAM: u16 = 0xFF5D`, `SYNC_PACKET: u16 = 0x75FF`, `SYNC_BERT: u16 = 0xDF55`, `EOT: u16 = 0x555D`; `pub fn golay24_encode(data: u16) -> u32` and `pub fn golay24_decode(word: u32) -> Option<u16>` (up to 3 errors corrected); `pub fn conv_encode(bits: &[u8]) -> Vec<u8>` (K=5, rate 1/2, the specification's polynomials, 4 flush bits), `pub fn puncture(bits: &[u8], pattern: &[u8]) -> Vec<u8>` with `pub const P1: &[u8]`, `P2`, `P3` from the specification, `pub fn viterbi_decode(bits: &[u8], pattern: &[u8]) -> Vec<u8>` (hard decisions, depunctured with erasures); `pub fn interleave(bits: &[u8; 368]) -> [u8; 368]`, `deinterleave`; `pub fn decorrelate(bits: &mut [u8; 368])` (XOR with the specification's 46-byte sequence; its own inverse); `pub fn encode_callsign(call: &str) -> Option<[u8; 6]>` (base-40, at most nine characters; `ALL` is the broadcast value), `pub fn decode_callsign(bytes: &[u8; 6]) -> String`; `#[derive(Clone, Debug, PartialEq)] pub struct Lsf { pub dst: String, pub src: String, pub stream: bool, pub data_type: u8, pub encryption: u8, pub encryption_subtype: u8, pub can: u8, pub meta: [u8; 14] }` with `pub fn to_bytes(&self) -> [u8; 30]` (CRC appended) and `pub fn parse(bytes: &[u8; 30]) -> Option<Lsf>` (CRC checked); `pub fn lsf_frame_symbols(lsf: &Lsf) -> Vec<u8>` and `pub fn stream_frame_symbols(lsf: &Lsf, frame_number: u16, payload: &[u8; 16], last: bool) -> Vec<u8>` (the LICH chunk from `frame_number % 6`, Golay-coded; the frame number with its EOT bit; the payload conv-coded and punctured with P2; interleaved, decorrelated, the sync word first; symbols 0..4); `pub fn lich_assemble(chunks: &[Option<[u8; 5]>; 6]) -> Option<[u8; 30]>`; `pub fn gnss_position(meta: &[u8; 14]) -> Option<(f64, f64)>` (the specification's GNSS META form, else None). The `type` names of spec 7.3: `pub fn type_name(lsf) -> &'static str` (`voice`, `voice_data`, `data`, `packet`) and `pub fn encryption_name(lsf) -> Option<&'static str>` (`None` for none, `scrambler`, `aes`, `other`).
- Produces in `synth.rs`: `pub fn rrc_modulate_4fsk(symbols: &[u8], symbol_rate_baud: u32, deviation_hz: f64, rate_hz: u32, alpha: f64) -> Vec<Complex<f32>>` (root-raised-cosine pulses spanning 8 symbols, 0..3 → -3, -1, +1, +3 × deviation/3, FM-modulated; builder's call per spec 13 whether `gfsk_modulate` with order 4 suffices instead); `SynthSpec::M17 { src: String, dst: String, can: u8, frames: u32 }` (callsigns at most nine base-40 characters, `frames` 1..=250, `can` 0..=15), rate 48 000; `m17_iq`: the preamble (`0x77` repeated), the LSF frame (TYPE voice 3200, META zero), `frames` stream frames with a zero payload (which `c2dec` renders as a low hum: audio enough), the last with the EOT bit, then the EOT marker; `M17_BAUD` 4800, `M17_DEVIATION_HZ` 2400, `M17_RRC_ALPHA` 0.5.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn crc16_m17_has_the_specifications_check_value() { /* by_name("crc16-m17").unwrap().check == 0x772B; crc(&CRC16_M17, b"") == 0xFFFF */ }
#[test] fn golay_round_trips_and_corrects_three_errors() { /* for data in [0, 0xFFF, 0xABC, 0x123]: decode(encode(d)) == d; with any 3 bits flipped still d */ }
#[test] fn the_specifications_golay_vector() { /* the encode of the data word the specification tabulates equals its listed codeword (take it from the spec's Golay section) */ }
#[test] fn viterbi_recovers_a_punctured_codeword_with_two_errors() { /* 240 random bits → conv_encode → puncture(P1) → flip 2 → viterbi_decode(P1) == input */ }
#[test] fn callsigns_round_trip_in_base_40() { /* "VK4ZET", "ALL", "N0CALL/P": decode(encode(c)) == c; a 10-character callsign → None */ }
#[test] fn an_lsf_round_trips_with_its_crc() { /* Lsf{dst "ALL", src "VK4ZET", stream true, data_type voice3200, ..}: parse(to_bytes) == Some(lsf); a flipped byte → None */ }
#[test] fn a_stream_frame_decodes_to_its_lich_chunk_and_payload() { /* stream_frame_symbols → (test) symbols to bits, decorrelate, deinterleave, split: Golay-decode the LICH to chunk frame%6 of the LSF bytes; viterbi(P2) gives frame number and payload */ }
#[test] fn six_chunks_rebuild_the_lsf() { /* lich_assemble of the six chunks == lsf.to_bytes() */ }
#[test] fn gnss_meta_gives_a_position_and_zero_meta_none() { /* a META built per the spec's GNSS form for Brisbane Airport → (-27.3842 ±0.001, 153.1175 ±0.001); zeros → None */ }
// synth.rs: m17_family_validates (callsign too long → Err "src"; frames 0/251 → Err; rate 48000); m17_iq_has_the_expected_length (preamble + (frames+1) × 40 ms + EOT ± one symbol).
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures m17 crc` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the module, the CRC, the modulator, the family, the field-craft family list.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures m17 crc synth bit_view` (the CRC table grew: `every_spec_matches_its_check_value` covers it) — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp docs/agents/field-craft.md
git commit -m "M17: the codes (Golay, convolutional with puncturing, interleaver, decorrelator, CRC-16/M17), the LSF and callsigns, the m17 synthesiser family"
```

---

### Task 14: The `m17_decode` stage with `c2dec`, the row, the digital-voice field craft (29b candidate)

**Files:**
- Create: `crates/zeta-dsp/src/m17_decode.rs`, `crates/zeta-dsp/tests/fake_c2dec` (an executable shell script), `modes/m17_decode.json`
- Modify: `crates/zeta-dsp/src/lib.rs` (`pub mod m17_decode;`, `register_all`), `crates/zeta-core/src/modes/mod.rs` (`EMBEDDED` 11), `crates/zeta-core/tests/modes.rs` (`EMBEDDED`; `every_embedded_row_emits_events_today` → `m17_decode` is `[Event, Audio]`; thin-tracks `("m17_decode", 100, 50.0)`; subjects plan `("m17_decode", "src", "stations", Fold, &["dst", "type", "can", "encryption"], None, None)`), `crates/zeta/tests/modes_check.rs` (`ZETA_HANDLER_C2DEC` pinned to `/nonexistent/zeta-test/c2dec` in `check()`; `m17_decode: skipped: c2dec missing`; `m17_decode_passes_its_synthesised_check` host-gated on `which c2dec`), `.github/workflows/ci.yml` (`codec2` in the apt line; `--strict m17_decode` in the strict step), `crates/zeta/src/agents_md.rs` (`note("job_start")` and `note("job_attach")` one sentence each), `docs/agents/field-craft.md` (a "Decoding digital voice" paragraph under "Transcribing a channel", replacing the line "Digital voice decoders (DMR, D-STAR, M17) will be transcribed the same way…"; the ordinary-use list gains M17), `README.md` ("Legal": M17), `AGENTS.md`, `skills/zetasdr/SKILL.md`
- Test: `crates/zeta-dsp/src/m17_decode.rs` tests, the core and zeta tests above

**Interfaces:**
- Consumes: Task 13's `m17` module, Task 3's `zeta_core::modes::handlers::resolve(row, 0)`, `nbfm::nbfm_demod`, `pocsag::BitSync`-style timing or a Gardner loop (builder's call: 4FSK at 10 samples per symbol; the `fsk_demod`/`clock_recover` library pieces of plan 19.2 may be called as functions), `resample::Resampler::new(C2_RATE_HZ, AUDIO_RATE_HZ)`, `tokio::process::Command` (the `process` feature from Task 3), `StageContext.note` (`ctx.note(json!({"m17": {"vocoder": <text>}}))`), `run_map`.
- Produces: `pub const M17_DECODE_KIND: &str = "m17_decode"`; `#[derive(Default, JsonSchema, Serialize, Deserialize)] #[serde(deny_unknown_fields)] pub struct M17Params {}`; `pub struct M17DecodeStage` (inputs `[Iq]`, outputs `[Event, Audio]`); `pub struct M17Demod` with `pub fn new(rate_hz: u64) -> Result<Self, String>` and `pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<M17Frame>` (`nbfm_demod` at `M17_DEVIATION_HZ`, symbol timing, 4-level slicing to symbols, sync-word hunt at the four sync values (correlation over 16 symbols, at most one symbol error), 192-symbol frames, decorrelate, deinterleave, the LSF parsed whole or from six LICH chunks); `pub enum M17Frame { Lsf(Lsf), Stream { frame_number: u16, last: bool, payload: [u8; 16] }, Eot }`. Events (two per stream): at the LSF (whole, or when the LICH completes) `{protocol "m17", src, dst, can, type, encryption, codec (3200|1600|null), frame, eot false, lat, lon}` and at EOT the same with `eot true` and the last frame number; `summary "{src} > {dst}: {type} {encryption?}"`. The vocoder: `struct Vocoder { child: tokio::process::Child, .. }` spawned per stream from `resolve(&row, 0)` with `.env_clear()` and args `["3200", "-", "-"]` or `["1600", "-", "-"]` by the LSF's type (`voice`: 3200; `voice_data`: 1600, its 8 data bytes per frame dropped); each stream frame's 16 payload bytes written to its stdin (two Codec 2 frames at 3200, one at 1600), the s16 8 kHz speech read from its stdout, `Resampler` to 48 kHz, published as `Payload::Audio { sample_rate_hz: 48000, channels: 1, samples }` with `peak` as the voice demodulators do and spans from the frames' times; an encrypted stream (`encryption` not null) feeds nothing to it; a child that exits sets `job_context.m17.vocoder` to `"c2dec exited: <status>"` once, the events continue, and the next LSF spawns again. The row of spec 7.3 verbatim: `m17_decode`, "M17 digital voice", family `amateur_voice`, band `{bandwidth_hz 12500, centers_hz [], min_hz null, range_hz null}`, `modulation ["fsk", "fm"]`, label `M17`, protocol `m17`, decoder graph `channel → m17_decode` with `handlers [{"programs": ["c2dec"], "install": {"debian": "codec2", "url": "https://github.com/drowe67/codec2"}}]`, the event schema (`can`, `frame` integers; `eot` boolean; `type` enum of four; `encryption` enum of three or null; `codec` enum `["3200", "1600"]` or null as strings; `lat`, `lon` number or null), `position {lat, lon, alt_m null}`, `identify {proof event, min_events 1, dwell_ms 5000, min_span_ms 1000, quick null, priority 0}`, catalogue `["m17-rf-protocol"]`, `subject {key src, name "stations", waterfall fold, columns dst/Destination, type/Type, can/CAN, encryption/Encryption, heading null, label null, track_points 100, track_min_m 50}`, the `notes` of spec 7.3, `test {kind synth, family m17, src "VK4ZET", dst "ALL", can 0, frames 25, expect {src "VK4ZET", dst "ALL", type "voice"}, within_ms 5000}`. `fake_c2dec`: `#!/bin/sh` reading stdin in 8-byte pieces and writing 320 zero bytes per piece (160 s16 samples) with `dd`/`head -c`, exit 0 at EOF; `ZETA_HANDLER_C2DEC` points at it in the stage tests.

- [ ] **Step 1: Write the failing tests**

```rust
// m17_decode.rs (ZETA_HANDLER_C2DEC = tests/fake_c2dec, set per test with a process-wide lock as the external tests do)
#[tokio::test] async fn a_stream_gives_two_events_and_audio() { /* m17_iq(VK4ZET → ALL, 25 frames) through the stage: event 1 {src "VK4ZET", dst "ALL", type "voice", encryption null, codec "3200", eot false}; event 2 eot true, frame 24; audio frames at 48 kHz mono whose spans sum to 25 × 40 ms ± 40 ms */ }
#[tokio::test] async fn joining_mid_stream_finds_the_lsf_from_the_lich() { /* iq cut after the LSF frame and 2 stream frames: the first event arrives within 6 frames with the callsigns */ }
#[tokio::test] async fn an_encrypted_stream_is_reported_and_not_decoded() { /* an LSF with encryption scrambler: one event encryption "scrambler", no audio frames */ }
#[tokio::test] async fn a_dead_vocoder_fails_no_event() { /* ZETA_HANDLER_C2DEC at a script that exits 1 at once: both events arrive, no audio, the note m17.vocoder starts "c2dec exited" */ }
#[tokio::test] async fn a_missing_vocoder_fails_the_stage_naming_it() { /* ZETA_HANDLER_C2DEC=/nonexistent and the stage run directly (no probe): Err containing "handler c2dec not found" (the node never reaches this: Task 3's probe makes the row unavailable first) */ }
#[test] fn a_low_rate_is_refused() { /* M17Demod::new(16_000) → Err containing "16000" */ }
// tests/modes.rs: EMBEDDED 11; m17_decode outputs [Event, Audio]; the row's handlers[0].programs == ["c2dec"]; thin-tracks and subjects plans as above.
// tests/modes_check.rs: "m17_decode: skipped: c2dec missing" in the in-crate list; m17_decode_passes_its_synthesised_check (host-gated): "m17_decode: ok".
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-dsp --features test-fixtures m17_decode` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the demodulator, the stage, the vocoder pipe, the row, the fake, the CI steps, the prose (`note("job_start")`: "`m17_decode` decodes M17 digital voice (4FSK 4800 baud) on any 2 m or 70 cm channel at the measured centre, 12 500 Hz band; its audio is ordinary job audio (play it, or `job_attach` transcribe), its events name the stream's callsigns; it needs `c2dec` (package codec2) on the node's host."; `note("job_attach")`: "a job whose kind emits audio: the voice demodulators and any mode row whose `outputs` lists `audio` (`m17_decode`)"; the field-craft paragraph: how to start it, that an encrypted stream is reported and never decoded, that `job_context.m17.vocoder` says when `c2dec` failed); regenerate. Prove with the real `c2dec`: `apt install codec2`, `cargo run -q -p zeta -- modes check --strict m17_decode` → `m17_decode: ok`.
- [ ] **Step 4: Run** — `cargo test -p zeta-dsp --features test-fixtures m17`, `cargo test -p zeta-core --test modes`, `cargo test -p zeta --test modes_check --test agents_md`, `cargo run -q -p zeta -- agents-md --check`; on the page (manual, or the Task 6 scenario's `isAudioJob` check): an `m17_decode` job row shows ▶ and Transcribe — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-dsp crates/zeta-core crates/zeta modes/m17_decode.json .github/workflows/ci.yml docs/agents/field-craft.md README.md AGENTS.md skills
git commit -m "M17: the m17_decode stage (4FSK from IQ, LSF and LICH, two events per stream, Codec 2 speech through the c2dec handler as ordinary job audio), the row, digital voice in the field craft"
```

---

### Task 15: README, guide, TODO, UAT, the plan series, the gate

**Files:**
- Modify: `README.md` ("Legal" lists AIS, FLEX and M17 if Tasks 11 and 14 did not; the feature line "decoders (APRS…)" gains AIS and M17), `docs/guide.md` ("Decoder modes": one paragraph naming the new rows, `handlers`, `outputs`, `access`, the `ais_vhf` plan), `TODO.md`, `UAT.md` (a "Plan 29" section), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (a row 29 in the order table and a "Plan 29 status" section), `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated if anything above changed the field craft)

- [ ] **Step 1: TODO.md** — In "Feature: decoder wave 2 (rows on the sonde frameworks)" strike what shipped (AIS, FLEX, M17, DFM/M20 synthesis, APRS positions) and keep DMR voice, whole-span POCSAG/APRS (now plan 31's), stereo. Add "Decoder wave 2: leftovers from plan 29": APRS objects and items as subjects of their own; an in-crate FLEX decoder (the slot; the row turns into a graph row with the same schema and test); M17 packet-mode content and the 1600 mode's data bytes; AIS message types beyond 1-5, 18, 21, 24; a `vocoder` handler for AMBE/IMBE on the M17 stage's shape (ties to the install-script gist); AIS and APRS feeds out (the Feeds gist); the FLEX synthesiser at 3200 and 6400 bps; `gfsk_modulate`'s BT if the AIS stage wanted 0.4; any Task 9/10/12 fallback findings. Add "Access: ACLs on restricted rows" (the operator, 2026-11-06): pager events and the rows that produce them carry `access: "restricted"` now and nothing reads it; the design to come: an access level per transport caller (the secret today is one level), `events.log` and `--then-tail`/`stream_tail` of a `restricted` row gated to the higher level, the page's ribbons and the agent's view likewise, the level stated in `GET /health`; until then the README's Legal paragraph is the guard. In "Architecture: a mode registry and a generic external-decoder adaptor" the Vocoders bullet says Codec 2 runs as `c2dec` (the operator's decision), AMBE/IMBE through a handler.
- [ ] **Step 2: UAT.md** — "## Plan 29: decoder wave 2" with checkboxes: (1) AIS on the field host's B210 with the discone: `plan_start ais_vhf` with `ais_decode`; within a few minutes vessels appear in the Subjects tab with names, markers move on the map, `summary` lines read `<name> <lat>,<lon> <speed>`, `classify` on channel A lists `ais_decode proved`. (2) APRS: `aprs_decode` on 144 800 000 Hz: stations with positions get markers; a Mic-E tracker decodes to a fix. (3) FLEX (when the operator's FLEX source is back up with an antenna): find a centre (`knowledge_lookup` on the pager bands or a `scan`), start `flex_decode` on the measured centre, read one page with `capcode` and `text`; until then the synthesised `--strict` check is the proof, and this line says so. (4) M17: first `knowledge_lookup` (the ACMA provider) for amateur assignments or repeaters near the station an M17 station could be heard on; with one, `m17_decode` on that channel until a stream decodes to callsigns and audio plays on the page (and `job_attach transcribe` yields text); without one, `zeta modes check --strict m17_decode` on the field host with `c2dec` installed is the acceptance, recorded here. (5) DFM/M20: `zeta modes check --strict` passes every embedded row on the field host (the wave's acceptance line); a live DFM or M20 is not expected over Brisbane. (6) `access`: a POCSAG event in `zeta events tail` carries `access: "restricted"`; nothing else changed.
- [ ] **Step 3: Plan series** — the row `| 29 | Decoder wave 2 | AIS in-crate, FLEX over multimon-ng, M17 with c2dec, DFM/M20 synthesis, APRS positions; expect tolerances, handlers on graph rows, outputs, row notes in the guide, the access marker. Spec …, plan …. | 16, 17, 23 |` and a "Plan 29 status" section after the newest status section: what shipped, what the gate proved (every in-crate row `ok` in CI, FLEX and M17 strict in CI, the APRS map scenario), what waits for the field host (`--strict` on the sonde rows, the UAT list), the rulings made (BT, the AIS clock, the M17 modulator, `within_ms` values, any fallback taken, and that M17 was or was not split into 29b). `README.md`'s plan list gains the line where 27's is.
- [ ] **Step 4: Gate** — `cargo fmt -p <each crate> -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped`; note the suite's time, under 600 s), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `cargo run -q -p zeta -- modes check` (every in-crate row `ok`; `flex_decode` and `m17_decode` `ok` with their programs installed, else `skipped: <program> missing`; the sonde rows `skipped`), `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Expected: all green.
- [ ] **Step 5: Commit**

```bash
git add README.md docs TODO.md UAT.md AGENTS.md skills
git commit -m "Docs: plan 29 (decoder wave 2) in the guide, README, TODO leftovers and the access-levels gist, UAT and the plan series"
```

---

## Calls made in this plan (the builder's, on the spec's silence)

1. The `access` marker (`null` or `"restricted"`, as plan 31 defines it) is a row field plus an envelope key (`summary::envelope`) plus `ModeInfo.access`; nothing filters on it (spec 4.7, the operator's "chosen cheaply").
2. `probe_handlers` groups by `programs` across program rows and graph rows' handlers; a graph row is unavailable when any handler is missing and `program` is its first handler's path; a pass of any row with handlers is recorded in `modes-checked.json`.
3. `outputs` is computed once at load into `LoadedMode.outputs` from the terminal nodes' `spec().outputs` (not `outputs_for`, which depends on params), so a demodulator's optional event output counts; the page's rule is `AUDIO_KINDS` or a row whose `outputs` has `audio`, through `subjects.js`'s `rowOf(kind)`.
4. The guide's rendered block is `## Mode rows`, not `## Decoding modes`, to keep the field craft's anchor unique.
5. APRS: `aprs_info::parse_info` never fails; objects and items are named, not placed; the `kiss` parser merges the same fields; the e2e `aprs` fixture's info becomes a Brisbane Airport position (the ribbon scenario's roster line stands).
6. The AIS bit clock is `pocsag::BitSync` at 9600 baud (5 samples per symbol at 48 kHz); a Gardner loop is the fallback. `channel` comes from `StageContext.band`'s centre. `nmea` sentences of a type 5 split at 60 characters with sequence id `0`.
7. The `ais_vhf` plan enters `plans.rs` in this wave (twenty lines; the acceptance wants both channels).
8. FLEX: the BCH(31,21) encoder moves from `synth.rs` into `bch.rs` as `bch3121_word`; the `multimon` parser splits FLEX_NEXT with `splitn(8, '|')`.
9. The M17 CRC enters the CRC table as `crc16-m17`; the modulator is RRC alpha 0.5 unless the demodulator slices Gaussian order-4 as well (then `gfsk_modulate`); `c2dec` is spawned per stream with `env_clear`, restarted on the next LSF after an exit, and never fed an encrypted stream.
10. CI installs `multimon-ng` and `codec2` in the `test` job and runs `modes check --strict flex_decode m17_decode` with a `--state-dir` under `$RUNNER_TEMP`; a row on the `test: null` fallback is left out of that step.
11. The e2e suite's one new scenario is the APRS map scenario, which also carries the `isAudioJob` unit check so M17's play/Transcribe rule is tested without an M17 fixture.

## Answered by the operator (2026-11-06)

1. Codec 2: the external `c2dec` stands; no `deny.toml` exception (Tasks 3, 14).
2. FLEX: synthesised-only, no live centre; a UAT task for the live check (Tasks 11, 12, 15).
3. M17: look in the register for nearby assignments first, else the synthesised strict check is the acceptance; a UAT task either way (Task 15).
4. Pager text: enabled, marked `access: "restricted"` for a later access control; the ACL gist in TODO (Tasks 2, 11, 15).
5. No recording in the repository: a sonde or FLEX synthesiser that misses its budget leaves `test: null` (Tasks 9, 10, 12).

## Self-review notes

- Spec coverage: 4.1 → Task 1; 4.2 → Task 3; 4.3 → Task 4; 4.4 → Task 5; 4.5 → Task 8; 4.6 → Task 11; 4.7 → Task 2; 5 → Tasks 7, 8; 6 → Tasks 11, 12; 7 → Tasks 13, 14; 8 → Tasks 9, 10; 9 → Task 6; 10 (the prose column) → each row's task plus Task 15; 11 → Tasks 11, 14, 15 (README Legal); 12 → the tests named in each task, CI in Tasks 12 and 14, the e2e scenario in Task 6, UAT in Task 15; 13 → the order above, the builder's calls listed here, the leftovers in Task 15; 14 → "Answered by the operator"; 15 → each call has its task.
- Names used across tasks: `compare`'s `{near, within}` (1 → 6, 8, 9, 10); `ModeRow.access`/`ModeInfo.access` (2 → 11); `Handler`, `Decoder::handler_blocks`, `handlers::resolve`, `handlers::find_handler` (3 → 14, and `proc.rs` callers); `LoadedMode.outputs`/`ModeInfo.outputs`/`isAudioJob` (4 → 6, 14); `aprs_info::parse_info`/`AprsInfo::fields` (6); `ais_msg::{decode, encode_sentences, unarmour, position_report_bits, static_report_bits, payload_to_hdlc_bytes}`, `SynthSpec::Ais` (7 → 8); `AisDecoder`, `decode_ais_iq`, `AIS_VHF` (8); `dfm_frame::{manchester_encode, hamming84_encode}` (9 → 10 if M20 needs them); `bch::bch3121_word` (12); `m17::{Lsf, lsf_frame_symbols, stream_frame_symbols, lich_assemble, gnss_position, type_name, encryption_name}`, `CRC16_M17`, `rrc_modulate_4fsk`, `SynthSpec::M17` (13 → 14).
- Where the spec no longer matches the code (reported to the caller too): the "Decoding modes" part of the guide is the hand-written field craft included verbatim, and `agents_md.rs` renders no modes section at all, so Task 5 adds one (`## Mode rows`); the quick attempts live in `zeta-dsp/src/identify.rs` (`quick_attempt`), not in `zeta-core`'s `node/identify.rs`; `modes_list` items are the `ModeInfo` struct in `ops.rs`; the APRS row today runs `channel → nbfm_demod → aprs_decode` over an audio edge (the one-stage-from-IQ rule applies to the new rows only, and the spec says as much); the page's job rows (`agent.js`) do not hold the subjects store, so Task 4 threads a `rowOf` callback from `app.js`; the `EMBEDDED` list is eight in both `mod.rs` and `tests/modes.rs`, and `an_unknown_id_lists_the_known_ones` pins the known-id prefix, which `ais_decode` changes; a node that never probes (every `zeta-core` test) lists every program row `ready`, so the "unavailable without `c2dec`" tests belong in the `zeta` crate where the probe runs; `nmea.rs` already carries its own `unarmour`, which moves to `ais_msg`.
