# Plan 34: Positions from ACARS text Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An aircraft whose ACARS, VDL2 or HFDL message text carries a position report lands on the map and in the Subjects table, through a row-level `derive` hook and one `acars_position` extractor, downlinks only.

**Architecture:** A new optional row field `derive: [{when, from, extract, into}]` is applied inside `summary::envelope` (`crates/zeta-core/src/modes/summary.rs`), the one function every decoder's event passes through (in-process stages via `zeta-dsp`'s `util::event`, program rows via `zeta-ext`'s `Publisher::publish`), before the summary and the position map. Extractors are a fixed enum in core, the first being `acars_position` (`crates/zeta-core/src/modes/derive.rs`), a hand-written scanner for four report forms. The in-process ACARS block gains `downlink`; the three aviation rows gain the derive and the ACARS row a position map. No page change.

**Tech Stack:** Rust (serde, schemars, serde_json), the `modes/*.json` rows, `zeta modes check`, the generated AGENTS.md.

**Spec:** `docs/superpowers/specs/2026-11-10-acars-text-positions-design.md` (all of it; section 7 lists the calls made, section 6 what is left to this plan).

## Global Constraints

- A row is `deny_unknown_fields`, every field required unless `#[serde(default)]`; a bad row is `broken` with `<field>: <reason>` (`RowError`), never a dead node. The row's JSON schema is generated from the structs (`schemars`), and `cargo run -q -p zeta -- agents-md --check` plus `--skill --check` must pass after any row, schema or prose change (regenerate with `cargo run -q -p zeta -- agents-md > AGENTS.md` and `... --skill > skills/zetasdr/SKILL.md`; never edit either by hand).
- `derive` entries run in order, inside `envelope`, before `summary`, `t_ns`, `position` and `access`. An entry runs only when every `when` pair holds (JSON equality), `from` holds a string, and no `into` target already holds a non-null value (spec 4.1: a fix is never assembled half from the decoder and half from the text). A row without `derive` behaves exactly as today.
- Validation (spec 4.2): `derive[i].when.<key>`, `derive[i].from`, `derive[i].into.<key>` (value) must be `event.schema.properties` keys; an `into` key must be an output of the extractor, else `derive[i].into.<key>: not an output of acars_position (lat, lon, alt_ft)`; an unknown extractor name is reported at `derive[i].extract`. The program-row rule "every schema property outside `ENVELOPE_FIELDS` is mapped by `decoder.output.fields`" admits a property any `derive[i].into` writes.
- The extractor (spec 4.3): forms tried in order, first match wins; output `lat`/`lon` in decimal degrees, south and west negative, rounded to 5 decimals; `alt_ft` an integer, only where the form states it; every rejection in spec 4.3 holds. No `regex` dependency in `zeta-core`.
- Downlinks only: ACARS `when: {"downlink": true}`, VDL2 `when: {"addr_type": "Aircraft"}`, HFDL `when: {"src_type": <the aircraft value dumphfdl prints>}`.
- Fixtures and tests use synthetic texts at the demo station (Brisbane Airport, -27.3842, 153.1175), registration `VH-ZET`, invented waypoints `ALPHA`, `BRAVO`, `CHARL`, flight `ZET1`. Nothing observed on the field host (registrations, waypoints, fixes) is committed. The repository is public: roles, never host names.
- Gates (CLAUDE.md) before the branch is offered: `cargo fmt -p <crate> -- --check` per touched crate, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, the two `agents-md --check`s, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Use a private `CARGO_TARGET_DIR`.
- One commit per task, message in the repo's style (`Plan 34: <what changed>`), on this branch.

## Models per task

Opus for Tasks 1 and 2 (they define the interface and the scanner every later task relies on) and Task 4 (the rows and the end-to-end check); Sonnet for Tasks 3 and 5.

## Review Focus

Inputs the spec implies but no task's tests exercised until the lines below were added; each has its test in the owning task.

1. A text with both hemisphere letters but the `POS` token missing (`S27231E153071` alone, e.g. inside a waypoint list) must not become a fix (form 1 is anchored on `POS`). Task 2.
2. An ADS-C frame whose `lat` is a number but `lon` is null (a malformed structured fix) must not be completed from the text: the entry is skipped because one target is non-null. Task 2.
3. An event with no `downlink` key at all (a bare graph without a row never reaches `envelope`; but a program row's event missing the `when` field) must not run the entry: a missing field is not equal to `true`. Task 2.
4. A latitude of exactly 90 degrees or a longitude of exactly 180 with non-zero minutes (`S90001E153071`) is out of range and rejected; `S89599E179599` is accepted. Task 2.
5. An H1 report whose third comma field is a two-digit or four-digit number yields a fix with no `alt_ft`, never an altitude of 1 500 or 165 000 ft. Task 2.

---

### Task 1: The `derive` row field and its validation

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`ModeRow` after `subject`; a `Derive` struct and `Extractor` enum near `PositionMap`; `validate` beside the `decoder.output.fields` rule at the `ENVELOPE_FIELDS` check; `bad_word_field`)
- Modify: `crates/zeta-core/src/modes/mod.rs` (re-export `Derive`, `Extractor`)
- Test: `crates/zeta-core/src/modes/row.rs` tests module (uses `edited(ADSB, |v| ...)` and `parse`)

**Interfaces:**
- Produces: `pub struct Derive { pub when: BTreeMap<String, serde_json::Value>, pub from: String, pub extract: Extractor, pub into: BTreeMap<String, String> }` (`when` `#[serde(default)]`, the rest required), `#[serde(deny_unknown_fields)]`, schemars `rename = "ModeDerive", transform = require_all` as `PositionMap` does; `pub enum Extractor { AcarsPosition }` with `#[serde(rename_all = "snake_case")]`, `impl Extractor { pub fn word(self) -> &'static str; pub fn outputs(self) -> &'static [&'static str] }` (`acars_position` → `["lat", "lon", "alt_ft"]`); `ModeRow.derive: Vec<Derive>` with `#[serde(default)]`, schema nullable via the existing pattern for defaulted fields.

- [ ] **Step 1: Write the failing tests** in the row tests module:

```rust
#[test] fn derive_round_trips_and_defaults_empty() { /* ADSB parses with derive empty; edited to add one entry {"when": {"downlink": true}, "from": "text", "extract": "acars_position", "into": {"lat": "lat", "lon": "lon", "alt_ft": "alt_ft"}} (adding text, downlink, lat, lon, alt_ft to the schema) parses to that Derive */ }
#[test] fn an_unknown_extractor_is_broken_at_its_field() { /* "extract": "cubits" → parse error field == "derive[0].extract" */ }
#[test] fn derive_names_schema_fields() { /* each of: a `when` key, `from`, an `into` value not in the schema → validate error field "derive[0].when.nope" / "derive[0].from" / "derive[0].into.lat"; an `into` key "speed" → "derive[0].into.speed" with reason starting "not an output of acars_position" */ }
#[test] fn a_derived_property_counts_as_mapped() { /* a program row (edited ADSB with fields map) whose schema lists `lat` mapped only by derive validates; one listing `wind` mapped by neither still fails "event.schema.properties.wind" */ }
```

- [ ] **Step 2: Run them**: `cargo test -p zeta-core --lib modes::row::tests::derive -- --nocapture` and the two others by name. Expected: compile errors (`derive` field, `Derive` type missing).

- [ ] **Step 3: Implement** `Derive`, `Extractor`, the `ModeRow.derive` field with its doc comment ("Fields filled from another field's text by a named extractor, only on events matching `when` and only when none of the `into` targets is set; empty for none"), the `validate` arms, the `ENVELOPE_FIELDS` rule's exemption (collect every `into` value across `derive` before the loop), and extend `bad_word_field` to walk `/derive/<i>/extract` against `["acars_position"]`, returning `derive[<i>].extract`.

- [ ] **Step 4: Run** the four tests and the whole row module: `cargo test -p zeta-core --lib modes::row`. Expected: PASS. Then `cargo run -q -p zeta -- agents-md --check`: it fails because the schema prose changed; regenerate both files (Global Constraints) and re-run both checks: PASS.

- [ ] **Step 5: Commit**: `Plan 34: a row's derive entries (when, from, extract, into) parse and validate; the Extractor enum with acars_position; a derived property counts as mapped`.

---

### Task 2: The `acars_position` extractor and its application in `envelope`

**Files:**
- Create: `crates/zeta-core/src/modes/derive.rs`
- Modify: `crates/zeta-core/src/modes/mod.rs` (`pub mod derive;`)
- Modify: `crates/zeta-core/src/modes/summary.rs` (`envelope`, first thing after the fields object is known)
- Test: `derive.rs` tests module; `summary.rs` tests module (uses `row(|v| ...)` and `positioned()`)

**Interfaces:**
- Consumes: `Derive`, `Extractor` (Task 1).
- Produces: `pub fn extract(which: Extractor, text: &str) -> BTreeMap<&'static str, serde_json::Value>` (only the outputs found); `pub fn apply(derive: &[Derive], fields: &mut serde_json::Map<String, serde_json::Value>)` (the entry rules of Global Constraints); `pub const FT_PER_FL: i64 = 100`.

- [ ] **Step 1: Write the failing extractor tests** in `derive.rs`, one `#[test]` per line, each asserting the map's `lat`, `lon` (to 1e-5) and `alt_ft` presence/value:

```text
h1_fms_report_at_the_demo_station        "#M1BPOSS27231E153071,ALPHA,025131,165,BRAVO,025351,CHARL,M9,252035,168" → -27.385, 153.11833, alt_ft 16500
h1_without_the_sublabel_prefix           "POSS27231E153071,ALPHA,025131,165,BRAVO" → same fix, alt 16500
h1_north_west_variant                    "POSN27231W153071,ALPHA,025131,165" → 27.385, -153.11833
h1_third_field_not_three_digits_has_no_alt   "...,ALPHA,025131,16,BRAVO" and ",ALPHA,025131,1650,BRAVO" → fix, no alt_ft   (Review Focus 5)
label_80_slash_pos_and_alt               "3N01 POSRPT 0054/09 YBBN/YSSY VH-ZET\r\n/POS S27231E153071/ALT 255/MCH 743/FOB 133.7" → fix, alt_ft 25500
label_16_decimal_with_spaces             "194606,28825,2106,  91,S 27.384 E153.118" → -27.384, 153.118, no alt
label_3l_decimal_with_a_slash            "S 27.384/E153.118" → -27.384, 153.118
qq_run_together                          "YBBNYSSY 2003\r\n001FE09200331S2723.1E15307.1017214" → -27.385, 153.11833
a_fix_in_the_middle_and_the_first_of_two_wins
an_empty_text_yields_nothing
rejects: compact form without the POS token ("S27231E153071 ALPHA")   (Review Focus 1)
rejects: engine report "-24.0-49.7 369900.760", "FOB 133.7", "025131", "131550", "POSRPT 0054/09", "S27650E153071" (minutes 650), "LANDING123456E", a LAT header "LAT LON\r\n-27.3842 153.1175"
range: "POSS90001E153071" rejected, "POSS89599E179599" accepted   (Review Focus 4)
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --lib modes::derive`. Expected: compile error, module missing.

- [ ] **Step 3: Implement** `derive.rs`: a byte scanner per form (spec 4.3; degrees + minutes/60 or minutes×10/600 as the form has it; `(x * 1e5).round() / 1e5`), `extract` trying the forms in order, `apply` with the entry rules. No regex.

- [ ] **Step 4: Run** the extractor tests. Expected: PASS.

- [ ] **Step 5: Write the failing envelope tests** in `summary.rs`, with a row built by `row(|v| { v["derive"] = json!([...]); v["position"] = json!({...ft map...}); schema gains text, downlink, lat, lon, alt_ft })`:

```rust
#[test] fn a_downlink_text_with_a_fix_gets_lat_lon_and_position()   // position.lat -27.385, alt_m ≈ 16500 * 0.3048
#[test] fn a_text_without_a_fix_adds_no_keys()                      // no lat, lon, alt_ft, position keys
#[test] fn when_not_holding_skips_the_entry()                        // downlink false, and downlink absent (Review Focus 3)
#[test] fn a_structured_fix_is_never_overwritten_or_completed()     // lat -27.38 and lon 153.12 present → unchanged; lat present, lon null → still null, no position (Review Focus 2)
#[test] fn into_leaving_alt_out_never_writes_it()
#[test] fn summary_sees_the_derived_field()                         // template "{lat?:.3}" renders -27.385
```

- [ ] **Step 6: Run** them: compile error (`envelope` ignores `derive`). **Step 7: Implement**: in `envelope`, when `fields` is an object and `row.derive` is non-empty, call `derive::apply` before computing `summary`. **Step 8: Run** `cargo test -p zeta-core --lib modes`. Expected: PASS.

- [ ] **Step 9: Commit**: `Plan 34: the acars_position extractor (H1 POS, label 80 /POS, 16/3L decimal, QQ run together; downlinks only by the entry's when) and envelope applies a row's derive before its summary and position`.

---

### Task 3: `downlink` on the in-process ACARS event

**Files:**
- Modify: `crates/zeta-dsp/src/acars.rs` (`AcarsBlock`, `parse_block`)
- Test: `acars.rs` tests module (`expected()`, `downlink_text_splits_msg_no_and_flight`)

**Interfaces:**
- Produces: `AcarsBlock.downlink: bool`, true when `block_id` is `Some(c)` with `c.is_ascii_digit()`; serialised as `downlink` on every `acars_decode` event.

- [ ] **Step 1: Write the failing test** `downlink_follows_the_block_id`: a block with block id `'5'` → `downlink == true`; with `'A'` → false; with none → false. Extend `expected()` so `roundtrip_block` still compares whole blocks.
- [ ] **Step 2: Run** `cargo test -p zeta-dsp --lib acars::tests::downlink_follows_the_block_id`: compile error (no field).
- [ ] **Step 3: Implement** the field in `parse_block` (the digit test is already computed there for `msg_no`; reuse it).
- [ ] **Step 4: Run** `cargo test -p zeta-dsp --lib acars` and `cargo test -p zeta-dsp --test acars_stage`. Expected: PASS (the stage test compares fields by name; if it compares whole payloads, add `downlink`).
- [ ] **Step 5: Commit**: `Plan 34: an ACARS block says whether it is a downlink (a digit block id), so a row can gate on it`.

---

### Task 4: The rows, the fixture and the end-to-end check

**Files:**
- Modify: `modes/acars_decode.json`, `modes/vdl2_decode.json`, `modes/hfdl_decode.json`
- Create: `crates/zeta-ext/tests/data/dumpvdl2-h1pos.json`
- Modify: `crates/zeta-ext/tests/external.rs` (`mapped_row`, new tests), `crates/zeta/tests/modes_check.rs` if it pins the ACARS row's `expect`
- Test: `cargo test -p zeta-core --test modes`, `cargo test -p zeta-ext --test external`, `cargo test -p zeta --test modes_check`, `cargo test -p zeta --test cli`

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: the three rows as spec 4.4; the fixture line: a dumpvdl2-shaped aircraft frame (`/vdl2/avlc/src/type` `Aircraft`, `src/addr` `7C0000`, `acars.reg` `.VH-ZET`, `acars.label` `H1`, `acars.sublabel` `M1B`, `acars.msg_text` the H1 report of Task 2's first test, no `arinc622`), modelled on `dumpvdl2-adsc.json`.

- [ ] **Step 1: Edit the rows.**
  - `acars_decode.json`: schema `downlink` (`boolean`), `lat`, `lon` (`["number", "null"]`), `alt_ft` (`["integer", "null"]`); `position` `{"lat": "lat", "lon": "lon", "alt_m": "alt_ft", "alt_unit": "ft"}`; `derive` `[{"when": {"downlink": true}, "from": "text", "extract": "acars_position", "into": {"lat": "lat", "lon": "lon", "alt_ft": "alt_ft"}}]`; `subject.track_points` 100, `subject.track_min_m` 1000; `test.text` `"#M1BPOSS27231E153071,ALPHA,025131,165,BRAVO,025351,CHARL,M9,252035,168"`, `test.expect` adds `"lat": {"near": -27.385, "within": 0.001}, "lon": {"near": 153.1183, "within": 0.001}, "alt_ft": 16500`; `notes` (spec 4.4 text).
  - `vdl2_decode.json`: `derive` with `when: {"addr_type": "Aircraft"}`; `notes` gains the sentence.
  - `hfdl_decode.json`: field `src_type` from `/hfdl/lpdu/src/type` (check the value dumphfdl prints for an aircraft in its formatter source, `src/hfdl.c` or `src/lpdu.c` upstream, and record it in the row's `notes`), schema entry, `derive` with that `when` and `into` `{"lat": "lat", "lon": "lon"}`.
- [ ] **Step 2: Run** `cargo test -p zeta-core --test modes` and `cargo test -p zeta --test cli`. Expected: PASS (fix any test that pins the ACARS schema's property list or the row count of positioned rows).
- [ ] **Step 3: Write the failing adaptor tests** in `external.rs`: `mapped_row` gains the derive entry when given the vdl2 map; `a_text_position_report_gives_a_position` (the new fixture → `position.lat` ≈ -27.385), `a_ground_station_frame_is_not_parsed` (same line with `src/type` `Ground station` → no `position`, no `lat`), and the existing ADS-C fixture tests unchanged and passing.
- [ ] **Step 4: Run** `cargo test -p zeta-ext --test external`. Expected: the two new tests FAIL (fixture missing), then PASS once the fixture is written.
- [ ] **Step 5: Run the row's own check**: `cargo run -q -p zeta -- modes check --strict acars_decode`. Expected: `acars_decode: ok` with the fix in its event. Then `cargo test -p zeta --test modes_check`.
- [ ] **Step 6: Regenerate** AGENTS.md and SKILL.md (notes changed); both `--check`s PASS.
- [ ] **Step 7: Commit**: `Plan 34: the ACARS row carries lat/lon/alt_ft, a position map and the derive (its synthesised test is a position report at the demo station); VDL2 and HFDL derive beside their structured fixes; a dumpvdl2 H1 fixture`.

---

### Task 5: Docs, TODO and UAT

**Files:**
- Modify: `docs/guide.md` (the decoders paragraph that says ADS-B and VDL2 plot), `docs/agents/field-craft.md` (the ACARS and VDL2 bullets under "Decoding modes"), `TODO.md` (remove "Modes: positions in ACARS free text"; add a gist "Modes: further ACARS position layouts" naming the `RSI` table form), `UAT.md` (a "Plan 34" section with spec 5.5's checks)
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md` (field craft changed)

- [ ] **Step 1: Edit** the four documents: one sentence each in the guide and the two field-craft bullets (an event whose text carries a position report, labels H1, 80, 16, 3L, QQ, carries `lat`, `lon`, sometimes `alt_ft` and `position`; the aircraft's own report, minutes old; uplinks never parsed); the TODO swap; the UAT section.
- [ ] **Step 2: Run** `bash scripts/check-doc-links.sh`, the two `agents-md --check`s after regenerating, `bash scripts/public-check.sh`. Expected: all clean.
- [ ] **Step 3: Run the full gates** (Global Constraints). Expected: all PASS; note any `ui e2e skipped` line.
- [ ] **Step 4: Commit**: `Plan 34: docs: text positions in the guide and field craft; the TODO gist retired for the RSI form; UAT checks`.
