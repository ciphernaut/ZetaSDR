# Plan 30: Aircraft data links (VDL Mode 2 and HFDL) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two program rows, `vdl2_decode` (dumpvdl2) and `hfdl_decode` (dumphfdl), decode the other aircraft data links beside ACARS and ADS-B, with aircraft positions where the link carries one, one aircraft table on the page across the links, and a per-source position so a KiwiSDR's catches are placed where the Kiwi is.

**Architecture:** Two generic additions to the mode-row schema (`decoder.output.fields`, a JSON-pointer map from a program's nested JSON line to the row's flat event fields, applied in the adaptor's `Publisher::publish` before `envelope`; the `$center_khz` placeholder) and one to `subject` (`merge`, the fields that identify one subject across rows). The rows are data in `modes/`, checked by `zeta modes check` against off-air fixtures under `modes/fixtures/` (resolved through `ZETA_FIXTURES_DIR` or `modes/` under the current directory). A `Port::position` accessor and `SourceInfo.position` give any source a position (the Kiwi reads its own from `/status`; `--source-position` configures one). The page merges per-job subjects into one table as a view in `subjects.js`, never in the store. No change to the node's job, lease or launch machinery.

**Tech Stack:** Rust (serde, schemars, `serde_json::Value::pointer`, tokio), the `modes/*.json` rows, vanilla JS page (`crates/zeta-ui/web/subjects.js`), the Playwright e2e suite (`crates/zeta-ui/tests/e2e/ui.py`), a uv-inline-metadata Python script for the fixture cut.

**Spec:** `docs/superpowers/specs/2026-11-06-aircraft-links-design.md` (all of it; section 7 holds the operator's decisions of 2026-11-06, section 8 the calls).

## Global Constraints

- A row is `deny_unknown_fields`, every field required unless `#[serde(default)]`; a schema for a defaulted field is nullable (`require_all` keeps it required and `null` when absent); errors are `RowError { field, error }` naming `<field>: <error>`, and a bad row is `broken`, never a dead node (`crates/zeta-core/src/modes/row.rs`).
- Without `decoder.output.fields` a program row behaves exactly as today (the whole parsed object is the event). The envelope's four fields (`protocol`, `summary`, `t_ns`, `position`) are added after the map and are exempt from the "every schema property is mapped" rule.
- Placeholders are the fixed list `PROGRAM_PLACEHOLDERS` in `row.rs`, substituted by `substitute` in `crates/zeta-ext/src/external.rs`; an unknown one is `broken` at `decoder.args[i]`.
- The two rows: `vdl2_decode` as spec 4.3 (`bandwidth_hz` 25000, `centers_hz` `[136975000, 136725000, 136775000, 136825000, 136875000]`, `--oversample 1`, `S16_LE`, `input.rate_hz` 105000, key `icao`, `position` with `alt_unit: ft`, `identify.dwell_ms` 15000, `merge ["reg", "icao"]`, `track_points` 100, `track_min_m` 1000); `hfdl_decode` as spec 4.4 (`bandwidth_hz` 6000, `centers_hz` `[]`, `range_hz` `[2800000, 22100000]`, `CS16`, `--sample-rate $rate_hz`, `input.rate_hz` 12000, `$center_khz`, key `icao`, `identify.dwell_ms` 40000, `within_ms` 10000, `merge ["reg", "icao"]`). `install.debian` is `null` with the OpenWebRX+ repository named in `notes`.
- Fixtures: `modes/fixtures/<mode>-<dial kHz>k-<n>.sigmf-{meta,data}`, at most 300 KB each, `ci8` allowed, reviewed line by line before commit (link-layer frames, position reports and operational messages kept; free text cut), the meta holding only SigMF core fields, `zeta:expected` and a `zeta:source` role. `.gitignore` gains `!/modes/fixtures/*.sigmf-data`.
- The operator's decisions (spec 7) bind every task: the B210 is **not** re-pinned by any task (the VDL2 fixture is UAT); the operator's own KiwiSDR is **never** added to the deployed node, a unit file, a deploy helper or persisted state by any task; test use from a developer shell with a throwaway `zeta` process is allowed; its host name is private (`"the local KiwiSDR"`, `kiwi-host`, and the untracked environment variable `ZETA_KIWI_HOST` are the only ways to refer to it; it never appears in a tracked file, a commit message or the ledger).
- The node runs, and every test passes, with no fixtures recorded, no Kiwi reachable, neither program installed and no network.
- Numbers from the spec are named constants, never literals in logic: `TAIL_MAX_MS = 10_000` (exists), `KIWI_STATUS_PATH = "/status"`, `FIXTURES_DIR_ENV = "ZETA_FIXTURES_DIR"`, `DEFAULT_FIXTURES_DIR = "modes"`, `MERGE_SECTION_CLASS = "subject-merged"`.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are generated: after any op, flag, row or field-craft change run `cargo run -q -p zeta -- agents-md --check`, regenerate (`cargo run -q -p zeta -- agents-md > AGENTS.md`, `cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md`) and commit the result in the same commit. Never edit them by hand. Prose goes in `crates/zeta/src/agents_md.rs` and `docs/agents/field-craft.md`.
- Gates (CLAUDE.md): `cargo fmt -p <crate> -- --check` per crate (the nested worktree breaks `cargo fmt --all`), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped` line), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Use a private `CARGO_TARGET_DIR`.
- The repository is public: roles, never private host names or LAN addresses, in code, tests, fixtures, docs and commit messages ("the field host", "the local KiwiSDR"). The demo station is Brisbane Airport (-27.3842, 153.1175, 4 m).
- Commits go on this branch; one commit per task, message in the repo's style (`Area: what changed`), no trailer. Each task's implementer appends one line to the ledger `.superpowers/sdd/plan30/progress.md` (task, commit, verdict, what was skipped and why); the reviewer appends theirs.

## Models per task

Opus where the task defines an interface later tasks consume, confirms behaviour against a live or upstream source, or changes the page; Sonnet where the brief is complete and the tests pin the behaviour.

| Task | Model | Why |
|---|---|---|
| 1 row schema: `fields`, `$center_khz`, `merge` | Opus | the types every later task names |
| 2 the adaptor applies the map | Sonnet | one function and a placeholder, tests pin it |
| 3 `modes check` fixture lookup | Sonnet | path resolution and a message |
| 4 the two rows, paths confirmed upstream, fake-program checks | Opus | the pointers must match the decoders' real JSON |
| 5 a source's position: type, accessor, `SourceInfo`, `--source-position` | Sonnet | plumbing with a fixed shape |
| 6 the Kiwi reads `/status` | Opus | protocol work on a socket, confirmed against the local KiwiSDR when reachable |
| 7 the page's merged aircraft table | Opus | a new view in `subjects.js`, a new e2e scenario |
| 8 the HFDL fixture (conditional) | Opus | live radio work, content review, a script |
| 9 field craft and the guide | Sonnet | |
| 10 TODO, UAT, the plan series, the gate | Sonnet | |

Ledger: `.superpowers/sdd/plan30/progress.md` in the worktree, one line per task with the implementer's and reviewer's verdicts; the rulings made on the operator's behalf go into the final report.

## Review Focus

Inputs the spec implies but no obvious test exercises; each gets a test in the task that owns the code:

1. A dumpvdl2 line whose ADS-C `tags` has no basic report, or one in a later slot than the map lists, must give an event with no `position` and every other field intact, never a parse failure or a position of nulls. (Task 2: `a_mapped_line_without_the_listed_tag_has_no_position`.)
2. A pointer that resolves to a string where `position` wants a number (`"lat": "n/a"`) gives no `position`; the event still publishes. (Task 2: `a_non_numeric_fix_is_no_position`.)
3. A Kiwi whose `/status` hangs or refuses must not delay or fail the tune: the source streams, `position()` is `None`, no fault. (Task 6: `a_hanging_status_page_neither_blocks_nor_faults`.)
4. The merged table when one of the two jobs stops, or a member subject is dropped by `subject_drop_s`: the survivor returns to its own section, a selection on a dropped member clears, no stale merged row. (Task 7: the scenario's last steps.)
5. `--source-position` naming a source no flag defines, or a latitude out of range, is refused at startup naming the flag, before the node opens. (Task 5: `source_position_refuses_unknown_ids_and_bad_values`.)

---

### Task 1: The row schema: `decoder.output.fields`, `$center_khz`, `subject.merge`

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`ProgramOutput`, `Subject`, `PROGRAM_PLACEHOLDERS`, `validate`, `validate_program`, tests module)
- Test: `crates/zeta-core/src/modes/row.rs` tests module

**Interfaces:**
- Consumes: `ProgramOutput { via, port, parser }`, `Subject { key, name, waterfall, columns, heading, label, track_points, track_min_m }`, `validate(row, stages)`, `validate_program(p)`, `RowError::new`, `PROGRAM_PLACEHOLDERS: [&str; 4]`.
- Produces:
  - `#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)] #[serde(untagged)] pub enum FieldPath { One(String), First(Vec<String>) }` with `pub fn pointers(&self) -> &[String]` (one or the list) and `pub fn check(&self, field: &str) -> Result<(), RowError>` (`decoder.output.fields.<field>: a JSON pointer starts with "/"` for any pointer not starting `/`; `decoder.output.fields.<field>: an empty list` for `First(vec![])`).
  - `ProgramOutput` gains `#[serde(default)] pub fields: Option<BTreeMap<String, FieldPath>>` (schema nullable via `require_all`; doc: "event field ← JSON pointer(s) into the parsed line, the first that resolves to a non-null value winning; `null` takes the whole object").
  - `Subject` gains `#[serde(default)] pub merge: Vec<String>` (doc: "the event fields whose equal values mean the same subject across rows sharing `name`; empty for none").
  - `PROGRAM_PLACEHOLDERS: [&str; 5] = ["$port", "$rate_hz", "$center_hz", "$center_khz", "$fifo"]`; `pub fn center_khz_text(center_hz: u64) -> String` (`8942`, `8942.5`, `136975`: an integer when `center_hz % 1000 == 0`, else up to three decimals with trailing zeros trimmed).
  - `pub const ENVELOPE_FIELDS: [&str; 4] = ["protocol", "summary", "t_ns", "position"]`.
  - In `validate`: when `fields` is `Some`, every key must be a schema property (`decoder.output.fields.<key>: not in event.schema.properties`), every schema property not in `ENVELOPE_FIELDS` must be a key (`event.schema.properties.<name>: not mapped by decoder.output.fields`), each value passes `FieldPath::check`; `subject.merge[i]` must name a schema property (or the subject's `key`), else `subject.merge[<i>]: not in event.schema.properties`.

- [ ] **Step 1: Write the failing tests** (in the existing tests module; `edited(ADSB, |v| ...)` builds a program row with a `fields` map over ADSB's schema)

```rust
#[test] fn fields_map_round_trips_and_defaults_to_null() { /* a row without `fields` parses with None; one with {"icao": "/a/b", "lat": ["/x", "/y"]} parses to One and First */ }
#[test] fn fields_keys_must_be_schema_properties() { /* key "nope" -> field "decoder.output.fields.nope" */ }
#[test] fn every_schema_property_is_mapped_except_the_envelope() { /* ADSB schema with a `fields` map missing "altitude_ft" -> "event.schema.properties.altitude_ft"; a schema listing "protocol" with no map entry for it is Ok */ }
#[test] fn a_pointer_must_start_with_a_slash_and_a_list_may_not_be_empty() { /* "icao" -> "decoder.output.fields.icao" error "a JSON pointer starts with \"/\""; [] -> "an empty list" */ }
#[test] fn center_khz_is_an_integer_when_it_divides() { assert_eq!(center_khz_text(8_942_000), "8942"); assert_eq!(center_khz_text(8_942_500), "8942.5"); assert_eq!(center_khz_text(136_975_000), "136975"); assert_eq!(center_khz_text(8_942_125), "8942.125"); }
#[test] fn center_khz_is_a_known_placeholder_and_others_still_break() { /* args ["$center_khz"] validate; ["$centre_khz"] -> "decoder.args[0]" */ }
#[test] fn merge_round_trips_defaults_empty_and_names_schema_fields() { /* ADSB subject + "merge": ["icao"] -> vec!["icao"]; absent -> empty; ["nope"] -> "subject.merge[0]" */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-core modes::row` — Expected: FAIL to compile (`FieldPath`, `center_khz_text`, `merge`).
- [ ] **Step 3: Implement** the types, the constant, `center_khz_text`, the three validation rules in `validate` (beside the existing position and subject schema checks) and `FieldPath::check` from `validate_program`'s caller.
- [ ] **Step 4: Run** — `cargo test -p zeta-core modes::` and `cargo test -p zeta-core --test modes` (the embedded rows still parse, `modes_show` reports `fields: null` and `merge: []`) — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core
git commit -m "Modes: decoder.output.fields (event field <- JSON pointers into a program's line), the \$center_khz placeholder and subject.merge on the row schema"
```

Acceptance: `cargo test -p zeta-core` green; a row with a `fields` map and one with `merge` are reported by `modes_show` as written, and the seven error cases name their field.

---

### Task 2: The adaptor applies the map and fills `$center_khz`

**Files:**
- Modify: `crates/zeta-ext/src/external.rs` (`substitute`, `Publisher::publish`, a new `pub(crate) fn apply_fields`)
- Create: `crates/zeta-ext/tests/data/dumpvdl2-adsc.json` (one dumpvdl2 line with an ACARS block carrying an ADS-C basic report), `crates/zeta-ext/tests/data/dumpvdl2-xid.json` (an XID frame, no ACARS), `crates/zeta-ext/tests/data/dumphfdl-downlink.json`, `crates/zeta-ext/tests/data/dumphfdl-uplink.json`, `crates/zeta-ext/tests/data/dumphfdl-spdu.json`
- Test: `crates/zeta-ext/tests/external.rs`

**Interfaces:**
- Consumes: Task 1's `FieldPath::pointers`, `ProgramOutput.fields`, `center_khz_text`; `zeta_core::modes::summary::envelope(row, start_ns, fields)`; the `fake_lines` harness (`json_row`, `start`, `send`, `next_event`).
- Produces: `pub(crate) fn apply_fields(map: &BTreeMap<String, FieldPath>, line: &Value) -> Value` (an object of the mapped fields: for each key the first pointer resolving to a non-null `Value`, cloned; keys with none are absent); `substitute` replaces `$center_khz` with `center_khz_text(center_hz)`.
- The five test vectors are built to the documented shape (libacars's `acars`/`arinc622`/`adsc` keys, dumpvdl2's `vdl2.avlc`, dumphfdl's `hfdl.lpdu`/`spdu`) with invented, non-identifying values (registration `VH-ZET`, ICAO `7C0000`, flight `ZET1`, a position near the demo station), each file's first line a `//`-free JSON object and a sibling `README` line in the test saying they are synthetic until the fixtures replace them (Task 8 / UAT).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn apply_fields_takes_the_first_present_pointer_and_copies_values_as_they_are() { /* over dumpvdl2-adsc.json with the vdl2 row's map (spec 4.3, pasted): icao, reg, text are strings, lat/lon numbers, alt_ft an integer, no "vdl2" key; a pointer to an object copies it */ }
#[test] fn a_mapped_line_without_the_listed_tag_has_no_position() { /* dumpvdl2-xid.json: icao present, reg/lat/lon absent; envelope adds no "position" */ }
#[test] fn a_non_numeric_fix_is_no_position() { /* the adsc line with lat set to "n/a": the event publishes, no "position" */ }
#[test] fn the_hfdl_list_form_keys_downlinks_and_uplinks_on_the_aircraft() { /* downlink: icao from src.ac_info; uplink: from dst.ac_info; spdu: no icao, gs_id present */ }
#[test] fn substitute_fills_center_khz() { /* args ["$center_khz", "$center_hz"] at 8_942_500 -> ["8942.5", "8942500"] */ }
#[tokio::test] async fn a_row_with_fields_publishes_the_mapped_event_and_without_them_the_whole_object() { /* fake_lines printing dumpvdl2-adsc.json: with the map, v["icao"] and v["position"]["lat"] set and v.get("vdl2") is None; the same row without "fields": v["vdl2"]["avlc"]["src"]["addr"] set */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-ext --test external fields substitute` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** `apply_fields`, call it in `publish` between `parsers::parse` and `envelope` when `block.output.fields` is `Some`, and extend `substitute`.
- [ ] **Step 4: Run** — `cargo test -p zeta-ext` — Expected: PASS (the existing `json_lines_rows_publish_enveloped_events` unchanged).
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ext
git commit -m "External: a program row's output.fields map is applied to each parsed line before the envelope; \$center_khz substituted"
```

Acceptance: the six tests pass; a row without `fields` publishes byte-for-byte what it did before.

---

### Task 3: `modes check` resolves relative fixture paths and names a missing one

**Files:**
- Modify: `crates/zeta/src/modes_check.rs` (`capture`, a new `fn fixture_path`), `.gitignore` (`!/modes/fixtures/*.sigmf-data`)
- Create: `modes/fixtures/.gitkeep`
- Test: `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Consumes: `TestSpec::Fixture { path }`, `Outcome::{Skipped, Fail}`, `check_row`'s `skip` closure.
- Produces: `pub const FIXTURES_DIR_ENV: &str = "ZETA_FIXTURES_DIR"`, `pub const DEFAULT_FIXTURES_DIR: &str = "modes"`; `fn fixture_path(path: &str) -> Result<PathBuf, String>`: an absolute path as given; a relative one under `$ZETA_FIXTURES_DIR` when set, else under `./modes`; `Err` when the meta is not a file: `fixture <path> not found (ZETA_FIXTURES_DIR unset; looked in <dir>)` or `(ZETA_FIXTURES_DIR=<value>; looked in <dir>)`. A missing fixture is a skip (`--strict`: a fail), decided in `check_row` before a node opens, like `<program> missing`.

- [ ] **Step 1: Write the failing tests** (the harness `check(dir, args)` and `write_row`)

```rust
#[test] fn a_relative_fixture_is_found_under_zeta_fixtures_dir() { /* write_fixture into <dir>/fx as "aprs"; row path "aprs.sigmf-meta"; check with .env("ZETA_FIXTURES_DIR", <dir>/fx) -> "aprs_fixture: ok" */ }
#[test] fn a_relative_fixture_defaults_to_modes_under_the_current_directory() { /* .current_dir(<dir>) with <dir>/modes/aprs.sigmf-meta -> ok */ }
#[test] fn a_missing_fixture_is_skipped_naming_the_lookup_and_strict_fails_it() { /* path "fixtures/none.sigmf-meta", ZETA_FIXTURES_DIR unset, cwd <dir>: "aprs_fixture: skipped: fixture fixtures/none.sigmf-meta not found (ZETA_FIXTURES_DIR unset; looked in <dir>/modes)"; --strict -> "fail: ..." and exit 1 */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta --test modes_check fixture` — Expected: FAIL (`ok` where a skip is expected, or an fs error).
- [ ] **Step 3: Implement** `fixture_path`, call it in `check_row` (skip on `Err`) and `capture` (the resolved path), add the un-ignore rule and the `.gitkeep`.
- [ ] **Step 4: Run** — `cargo test -p zeta --test modes_check` — Expected: PASS (`a_fixture_test_row_is_checked` with its absolute path unchanged).
- [ ] **Step 5: Commit**

```bash
git add crates/zeta/src/modes_check.rs crates/zeta/tests/modes_check.rs .gitignore modes/fixtures/.gitkeep
git commit -m "Modes check: relative fixture paths resolve under ZETA_FIXTURES_DIR or modes/, a missing fixture is a named skip"
```

Acceptance: the three tests pass; `zeta modes check` output for the existing rows is unchanged.

---

### Task 4: The `vdl2_decode` and `hfdl_decode` rows, their pointers confirmed, checked through the fake program

**Files:**
- Create: `modes/vdl2_decode.json`, `modes/hfdl_decode.json`
- Modify: `crates/zeta-core/src/modes/mod.rs` (`EMBEDDED` 8 → 10), `crates/zeta-core/tests/modes.rs` (`EMBEDDED`, `the_rows_name_the_planned_subjects`, `the_rows_thin_their_tracks_as_planned`), `crates/zeta/tests/modes_check.rs` (`modes_check_passes_the_four_in_crate_rows`, `an_unknown_id_lists_the_known_ones`, the `NO_SONDE`-style pins `ZETA_HANDLER_DUMPVDL2`, `ZETA_HANDLER_DUMPHFDL`), `crates/zeta-ext/tests/data/*.json` (Task 2's vectors, corrected to the confirmed paths)
- Test: `crates/zeta-core/tests/modes.rs`, `crates/zeta/tests/modes_check.rs`

**Interfaces:**
- Consumes: Tasks 1-3; `fake_lines --lines-file PATH` (`crates/zeta-ext/tests/fake_lines`, prints its lines after `FAKE_AFTER_BYTES` of input, default 1024); the `write_row` helper (`modes_check.rs`) extended to take a whole row.
- Produces: the two rows exactly as spec 4.3 and 4.4, `test.path` `fixtures/vdl2-136975k-1.sigmf-meta` and `fixtures/hfdl-<freq>k-1.sigmf-meta` (the HFDL frequency chosen in Task 8; until then `fixtures/hfdl-pending.sigmf-meta`), `expect` `{"icao": "<hex>"}` / `{"gs_id": <n>}` as placeholders the fixture tasks fill; `notes` with the install sentence and the "pin first, beside ACARS" order.
- Confirming the paths (the first step, before the rows): read dumpvdl2's and dumphfdl's upstream JSON formatters (`github.com/szpajder/dumpvdl2`: `src/avlc.c`, `src/vdl2.c`, `src/output-common.c`, the `json_key` of each `la_type_descriptor`; `github.com/szpajder/dumphfdl`: `src/lpdu.c`, `src/spdu.c`, `src/hfnpdu.c`, `src/hfdl.c`; libacars's `acars.c`, `arinc.c`, `adsc.c`, `libacars.c` for `la_proto_tree_format_json`'s nesting) with WebFetch, and fix every pointer in the rows and in Task 2's vectors to the names found. When a developer shell has `ZETA_KIWI_HOST` set and `dumphfdl` on `PATH`, also capture one real `spdu` line from the local KiwiSDR with a throwaway `zeta` (never the deployed node; the Kiwi's host name stays out of every file) and compare. Record in the ledger which paths were confirmed from source and which await the VDL2 fixture (the ADS-C tag wrapper).

- [ ] **Step 1: Confirm the paths upstream** as above; note the result in the ledger.
- [ ] **Step 2: Write the failing tests**

```rust
// crates/zeta-core/tests/modes.rs
#[test] fn embedded_rows_parse_and_validate() { /* EMBEDDED is the ten ids in order, all Ready */ }
#[test] fn the_aircraft_link_rows_are_pinned() { /* vdl2_decode: label "VDL2", protocol "vdl2", band.bandwidth_hz 25000, subject.key "icao", merge ["reg","icao"], position alt_unit ft, decoder.input.rate_hz 105000; hfdl_decode: "HFDL"/"hfdl", 6000, range_hz [2800000, 22100000], rate 12000, args contain "$center_khz", dwell_ms 40000 */ }
// crates/zeta/tests/modes_check.rs
#[test] fn modes_check_passes_the_four_in_crate_rows() { /* plus "vdl2_decode: skipped: dumpvdl2 missing", "hfdl_decode: skipped: dumphfdl missing" */ }
#[test] fn the_vdl2_row_decodes_through_the_fake_program() { /* an operator-dir copy of vdl2_decode: id "vdl2_fake", programs [<fake_lines path>], args ["--lines-file", <dumpvdl2-adsc.json>], test {kind: synth, family: psk, ... (the psk family's minimal params as a_row_test_may_name_the_fsk_family uses for fsk), expect {"icao": "7C0000", "reg": "VH-ZET"}, within_ms 10000}: "vdl2_fake: ok" */ }
#[test] fn the_hfdl_row_decodes_an_uplink_through_the_fake_program() { /* the same with dumphfdl-uplink.json, expect {"icao": "7C0000"} */ }
```

- [ ] **Step 3: Run to verify they fail** — `cargo test -p zeta-core --test modes` and `cargo test -p zeta --test modes_check` — Expected: FAIL (8 embedded; unknown mode).
- [ ] **Step 4: Write the rows**, update `EMBEDDED` in both places, pin the two programs missing in the `check` harness, extend `write_row`.
- [ ] **Step 5: Run** — `cargo test -p zeta-core --test modes`, `cargo test -p zeta --test modes_check`, `cargo run -q -p zeta -- modes show vdl2_decode` — Expected: PASS; the row prints with `fields` and `merge`.
- [ ] **Step 6: Commit**

```bash
git add modes crates/zeta-core crates/zeta crates/zeta-ext/tests/data
git commit -m "Modes: vdl2_decode (dumpvdl2, ADS-C positions) and hfdl_decode (dumphfdl on a Kiwi) rows, pointers confirmed against upstream, checked through fake_lines"
```

Acceptance: ten embedded rows, both `ready` on a host with the programs and `unavailable` naming the url without; the two fake-program checks print `ok`; the ledger says which paths were confirmed.

---

### Task 5: A source's position: `SourcePosition`, `Port::position`, `SourceInfo.position`, `--source-position`

**Files:**
- Modify: `crates/zeta-core/src/types.rs` (`SourcePosition`, `PositionFrom`), `crates/zeta-core/src/port.rs` (`Port::position`), `crates/zeta-core/src/ops.rs` (`SourceInfo.position`, filled in `Op::SourceList`), `crates/zeta-core/src/node/mod.rs` (`NodeConfig.source_positions: BTreeMap<SourceId, SourcePosition>`, `Node::source_position(&SourceId) -> Option<SourcePosition>`: configured else the port's), `crates/zeta/src/cli.rs` and `crates/zeta/src/sources.rs` (`--source-position ID=LAT,LON[,ALT_M]`, repeatable; `parse_source_position`), `crates/zeta/src/node_setup.rs` (`open_with` takes the map; an id no source flag defines is refused `--source-position: unknown source <id>`), `crates/zeta/src/agents_md.rs` (the flag line beside `--source-kiwi`; `source_list` text: "`position` `{lat, lon, alt_m, from}` where the source knows where it is, `null` otherwise")
- Test: `crates/zeta/src/sources.rs` tests, `crates/zeta-core/tests/ops_sources.rs` (or the existing source-list test file), `crates/zeta/tests/cli.rs`

**Interfaces:**
- Produces: `#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)] pub struct SourcePosition { pub lat: f64, pub lon: f64, #[schemars(required, schema_with = "nullable::<f64>")] pub alt_m: Option<f64>, pub from: PositionFrom }`; `#[serde(rename_all = "snake_case")] pub enum PositionFrom { Status, Configured }`; `Port::position(&self) -> Option<SourcePosition> { None }`; `SourceInfo { ..., #[serde(default)] #[schemars(required, schema_with = "crate::types::nullable::<SourcePosition>")] pub position: Option<SourcePosition> }`; `pub fn parse_source_position(s: &str) -> Result<(String, SourcePosition), String>` (lat in -90..=90, lon in -180..=180, errors `expected ID=LAT,LON[,ALT_M], got <s>` / `lat out of range` / `lon out of range`).

- [ ] **Step 1: Write the failing tests**

```rust
// sources.rs
#[test] fn source_position_parses_with_and_without_altitude() { /* "k=-27.3842,153.1175,4" -> ("k", {lat, lon, alt_m: Some(4.0), from: Configured}); "k=-27.3842,153.1175" -> alt_m None */ }
#[test] fn source_position_refuses_unknown_ids_and_bad_values() { /* "k=-91,0" -> "lat out of range"; "k=0" -> "expected ID=LAT,LON[,ALT_M]"; and, in cli.rs, `zeta --source-file f=... --source-position nope=0,0 source list` exits 2 with "--source-position: unknown source nope" */ }
// zeta-core
#[tokio::test] async fn source_list_reports_a_configured_position_and_null_without() { /* a file source: position null; with NodeConfig.source_positions set: {lat, lon, alt_m, from: "configured"} */ }
#[test] fn the_schema_lists_position_as_nullable() { /* node schema: SourceInfo.position type includes "null" and is required */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta sources::` , `cargo test -p zeta-core --test ops_sources` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the type, the accessor, the field, the config map, the flag and its refusal, the guide lines.
- [ ] **Step 4: Run** — `cargo test -p zeta-core`, `cargo test -p zeta --test cli --test agents_md`, `cargo run -q -p zeta -- agents-md --check` (regenerate `AGENTS.md` and `SKILL.md`) — Expected: PASS, generated files updated.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-core crates/zeta AGENTS.md skills
git commit -m "Sources: a per-source position (Port::position, SourceInfo.position, --source-position ID=LAT,LON[,ALT_M]); the station stays the node's"
```

Acceptance: `source_list` shows `position` for a configured source and `null` for every other; a bad flag is refused naming it; the schema and the guide carry the field.

---

### Task 6: The Kiwi reads its position from `/status`

**Files:**
- Modify: `crates/zeta-source-kiwi/src/source.rs` (`KiwiSource::position`, `Status.position`), `crates/zeta-source-kiwi/src/session.rs` (a `status_page(host, port, timeout) -> Result<String, String>` HTTP/1.0 GET over a `TcpStream`, called once per connection before the websocket upgrade, bounded by `handshake_timeout_ms`; a new `pub(crate) fn parse_status_position(text: &str) -> Option<SourcePosition>` reading `gps=(lat, lon)` and `asl=<m>`), `crates/zeta-source-kiwi/src/testing.rs` (`FakeScript.status: Option<String>`; `FakeKiwi` answers `GET /status` with it, or hangs when `status_hang: true`), `docs/agents/field-craft.md` is Task 9's
- Test: `crates/zeta-source-kiwi/src/session.rs` tests, `crates/zeta-source-kiwi/tests/kiwi.rs`

**Interfaces:**
- Consumes: Task 5's `SourcePosition`, `PositionFrom::Status`, `Port::position`.
- Produces: `KiwiSource::position()` returns the last status read (`from: Status`), `None` before the first connection or when the page had no `gps=` line; `pub const KIWI_STATUS_PATH: &str = "/status"`; the status read's failure is counted in `Status.status_errors` and never faults the source.
- Confirmed live: when a developer shell has `ZETA_KIWI_HOST` set, run a throwaway `zeta --state-dir ./tmp/kiwi-state --source-kiwi hf=$ZETA_KIWI_HOST:8073 source list` after a short job (or `curl http://$ZETA_KIWI_HOST:8073/status`) and check the `gps=` and `asl=` lines are as assumed; adjust the parser; write the shape found (keys only, no values) in the ledger. The local KiwiSDR is never added to the deployed node.

- [ ] **Step 1: Write the failing tests**

```rust
// session.rs
#[test] fn status_position_reads_gps_and_asl() { /* "status=active\ngps=(-27.3842, 153.1175)\nasl=4\n" -> Some({-27.3842, 153.1175, Some(4.0), Status}); "gps=(0.000000, 0.000000)" with "gps_good=0" is still a position (the configured one); no gps line -> None; "gps=(x, y)" -> None */ }
// tests/kiwi.rs
#[tokio::test] async fn a_kiwi_with_a_status_page_reports_its_position() { /* FakeScript with status Some(...): after the first tune, source.position() == Some(...), source_list-style read through Port */ }
#[tokio::test] async fn a_kiwi_without_gps_in_its_status_reports_none() {}
#[tokio::test] async fn a_hanging_status_page_neither_blocks_nor_faults() { /* status_hang: true, handshake_timeout_ms 500: tune returns Ok within 2 s, position None, fault None, frames arrive */ }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p zeta-source-kiwi` — Expected: FAIL to compile.
- [ ] **Step 3: Implement** the status GET, the parser, the `FakeKiwi` extension and the accessor.
- [ ] **Step 4: Run** — `cargo test -p zeta-source-kiwi` — Expected: PASS. Then the live confirmation above when possible; ledger line either way.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-source-kiwi
git commit -m "Kiwi: the source reads its position from the receiver's /status page once per connection (gps=, asl=), never blocking or faulting on it"
```

Acceptance: the four tests pass; with a Kiwi that has `gps=` in its status, `zeta source list` shows `position.from: "status"`.

---

### Task 7: One aircraft table across the links (the page)

**Files:**
- Modify: `crates/zeta-ui/web/subjects.js` (`SubjectsPane`: `mergeGroups(jobs)`, `mergedColumns(rows)`, `drawMergedSection`, `render`), `crates/zeta-ui/web/style.css` (`section.subject-merged` shares `section.subject-job`'s rules), `crates/zeta/tests/ui.rs` (`fake_acars` and `fake_vdl2` rows in `fake_modes`, their lines files), `crates/zeta-ui/tests/e2e/ui.py` (the scenario, registered in the suite's list), `modes/acars_decode.json` (`subject.merge: ["reg"]`)
- Test: `crates/zeta-ui/tests/e2e/ui.py::scenario_subjects_merge_across_rows`

**Interfaces:**
- Consumes: `SubjectStore.list(jobId)`, `rowOf(kind)`, `select(jobId, key)`, `selected()`, `subjectValues`, `columns(row)`; Task 1's `subject.merge` as `modes_show` reports it.
- Produces, in the pane (spec 4.8): running jobs whose rows have a non-empty `subject.merge` are grouped by `row.subject.name`; a group of two or more jobs draws one `section.subject-merged[data-name]` (class `MERGE_SECTION_CLASS`) in place of their per-job sections. Rows of that section are groups of subjects joined by union-find: two subjects join when some field in either row's `merge` (the key counted as `state[row.subject.key]`) has equal non-blank values in both. Group state merged newest-last-seen wins; `position` the newest member's; `count` the sum; `dim` when all dim; the key column is the first `merge` field present (`reg`, else `icao`); columns the union of the rows' `columns` by field, then `Via` (the member jobs' row `protocol`s, sorted, comma-separated), then `Position`, `Distance`, `Last seen`, `Count`; the heading `aircraft, N (<kind> <source>, <kind> <source>)`; a press selects the newest member's `{job, key}`; `tr.subject[data-job][data-key]` carries that member's. A job alone in its name, or with an empty `merge`, keeps its per-job section.
- The fake rows: `fake_acars` (key `reg`, `merge ["reg"]`, columns `flight`, `label`, `text`) prints `VH-ZET` twice and `VH-ABC` once; `fake_vdl2` (key `icao`, `merge ["reg", "icao"]`, columns `reg`, `flight`, `text`) prints `7C0000` with `reg VH-ZET` twice and `7C0001` with no `reg` once; both `name: "aircraft"`, both on the `tone` source at distinct widths (`start_subject_job`).

- [ ] **Step 1: Write the failing scenario**

```python
def scenario_subjects_merge_across_rows(page, url, secret):
    a = start_subject_job(page, url, secret, "fake_acars", bandwidth_hz=41_000)
    v = start_subject_job(page, url, secret, "fake_vdl2", bandwidth_hz=42_000)
    try:
        open_subjects(page)
        merged = page.locator("#subjects section.subject-merged")
        expect(merged).to_have_count(1)
        expect(page.locator("#subjects section.subject-job")).to_have_count(0)
        expect(merged.locator("h3")).to_contain_text("aircraft, 3")
        zet = merged.locator('tr.subject:has(td[data-col="reg"]:text-is("VH-ZET"))')
        expect(zet).to_have_count(1)
        expect(zet.locator('td[data-col="via"]')).to_have_text("fake_acars, fake_vdl2")
        expect(zet.locator('td[data-col="count"]')).to_have_text("4")
        expect(merged.locator("tr.subject")).to_have_count(3)  # VH-ZET, VH-ABC, 7C0001
        zet.dispatch_event("pointerdown", {"button": 0})
        expect(zet).to_have_class(re.compile(r"\bselected\b"))
    finally:
        http_json(url, secret, "/op", {"op": "job_stop", "job": v})
    expect(page.locator("#subjects section.subject-merged")).to_have_count(0)
    expect(page.locator('#subjects section.subject-job[data-job="%s"]' % a)).to_have_count(1)
    http_json(url, secret, "/op", {"op": "job_stop", "job": a})
```

- [ ] **Step 2: Run to verify it fails** — `ZETA_UI_ONLY=scenario_subjects_merge_across_rows cargo test -p zeta --test ui -- --nocapture` — Expected: FAIL (no `section.subject-merged`).
- [ ] **Step 3: Implement** the grouping and the merged section in `subjects.js`, the fake rows and lines in `ui.rs`, `merge` on `acars_decode`.
- [ ] **Step 4: Run** — the scenario alone, then `cargo test -p zeta --test ui -- --nocapture` (the existing subjects scenarios unchanged, no `ui e2e skipped` line), `cargo test -p zeta-core --test modes` (acars's `merge`) — Expected: PASS.
- [ ] **Step 5: Commit**

```bash
git add crates/zeta-ui crates/zeta/tests/ui.rs modes/acars_decode.json
git commit -m "UI: one subject table across rows that declare subject.merge (aircraft across acars_decode and vdl2_decode), a view over the per-job subjects"
```

Acceptance: the scenario passes; with only one merging job running the pane looks exactly as before.

---

### Task 8 (conditional): The HFDL fixture from the local KiwiSDR

> Runs only when the developer shell has `ZETA_KIWI_HOST` set, the Kiwi answers, and `dumphfdl` is on `PATH`. Otherwise the task records `skipped: no Kiwi reachable from this shell` in the ledger, leaves `hfdl_decode`'s check skipped, and Task 10 writes the recording as a UAT task. It never touches the deployed node, a unit file or a deploy helper, and the Kiwi's host name never leaves the shell.

**Files:**
- Create: `scripts/sigmf-cut.py` (uv inline metadata, numpy only: `--in <cf32 sigmf-meta> --from-s A --to-s B --format ci8 --out <path stem> --expected '<json>' --source kiwi`: cuts the span, scales to `ci8`, writes the pair with the core fields, `zeta:expected` and `zeta:source`), `modes/fixtures/hfdl-<dial kHz>k-1.sigmf-meta`, `modes/fixtures/hfdl-<dial kHz>k-1.sigmf-data`
- Modify: `modes/hfdl_decode.json` (`test.path`, `expect` `{"gs_id": <n>}`), `crates/zeta/tests/modes_check.rs` (nothing: the embedded check still pins dumphfdl missing)

**Interfaces:**
- Consumes: Task 4's row; `zeta serve --state-dir ./tmp/kiwi-state --listen 127.0.0.1:<port> --source-kiwi hf=$ZETA_KIWI_HOST:8073` as a throwaway node from a developer shell; `job_start --kind capture` with a whole window, or `iq read` of the ring, on the dial frequency with 12 000 Hz; `dumphfdl --iq-file <cf32> --sample-format CF32 --sample-rate 12000 --centerfreq <khz> <khz>` offline.
- Produces: a fixture of at most 300 KB opening on a squitter within its first 3 s (`within_ms` 10 000 holds), `zeta:expected {"gs_id": <n>}`, reviewed line by line (no free text; squitters, logons and position reports only); the row's `test` pointing at it; `zeta modes check --strict hfdl_decode` printing `ok` from the checkout with dumphfdl installed.

- [ ] **Step 1: Pick the frequency** from dumphfdl's `etc/systable.conf` for the two stations nearest the demo station at the current hour; write it (the frequency, not the host) in the ledger.
- [ ] **Step 2: Record** 60 s through the throwaway node; stop it; delete `./tmp/kiwi-state`.
- [ ] **Step 3: Decode offline**, choose the cut, run `scripts/sigmf-cut.py`, decode the cut again at 12 kHz (and at 24 kHz if it fails: then `input.rate_hz` becomes 24 000 in the row and the ledger says so), read every line.
- [ ] **Step 4: Run** — `cargo run -q -p zeta -- modes check --strict hfdl_decode` from the checkout — Expected: `hfdl_decode: ok`. `bash scripts/public-check.sh` — Expected: clean (the meta has no host names).
- [ ] **Step 5: Commit**

```bash
git add scripts/sigmf-cut.py modes/fixtures modes/hfdl_decode.json
git commit -m "Modes: the hfdl_decode fixture (10 s of one HFDL channel off a KiwiSDR, ci8) and the sigmf-cut script"
```

Acceptance: `modes check --strict hfdl_decode` is `ok` where dumphfdl is installed and `skipped: dumphfdl missing` where not; the pair is under 300 KB and carries nothing private.

---

### Task 9: Field craft and the guide

**Files:**
- Modify: `docs/agents/field-craft.md` ("Decoding pagers, aircraft and FM data": VDL2 and HFDL bullets, the recipe's VDL2 line, a sentence on `subject.merge`; "Remote HF receivers (KiwiSDR)": the source's `position`, `--source-position`, HFDL on a Kiwi), `crates/zeta/src/agents_md.rs` (`args` doc text: `$center_khz`; the `modes_list`/`modes_show` notes: `fields`, `merge`; the kinds paragraph: `vdl2_decode` and `hfdl_decode` in one sentence each beside `acars_decode`), `AGENTS.md`, `skills/zetasdr/SKILL.md` (regenerated)
- Test: `crates/zeta/tests/agents_md.rs`, `bash scripts/check-doc-links.sh`

**Interfaces:**
- Consumes: spec 4.9's text: VDL2 at 136 975 000 Hz worldwide (136 725 000, 136 775 000, 136 825 000, 136 875 000 in Europe), 25 000 Hz, dumpvdl2; beside ACARS pin the radio at 133 500 000 Hz / 8 000 000 Hz first, then `plan_start acars_vhf`, then the VDL2 job; a ground-station row's ACARS columns name the aircraft of each uplink; an ADS-C report gives the frame a `position`. HFDL on a Kiwi: `center_hz` is the listed (dial) frequency, 6 000 Hz, the signal 1.44 kHz above it, a squitter every 32 s proves the channel, frequencies from a list not a raster, dumphfdl.

- [ ] **Step 1: Write the text** in both places.
- [ ] **Step 2: Regenerate and check** — `cargo run -q -p zeta -- agents-md > AGENTS.md && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md && cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check`, `cargo test -p zeta --test agents_md`, `bash scripts/check-doc-links.sh` — Expected: clean.
- [ ] **Step 3: Commit**

```bash
git add docs/agents AGENTS.md skills crates/zeta/src/agents_md.rs
git commit -m "Guide: VDL Mode 2 beside ACARS, HFDL on a KiwiSDR, the merged aircraft table and a source's position in the field craft"
```

Acceptance: `agents-md --check` and `--skill --check` pass; the field craft names both rows with their bands and the pin-first order.

---

### Task 10: TODO, UAT, the plan series, the gate

**Files:**
- Modify: `TODO.md`, `UAT.md` (a "Plan 30" section), `docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md` (a row 30 in the sprint table and a "Plan 30 status" section before "Plan 26 status")

- [ ] **Step 1: TODO.md** — Replace "Modes: VDL Mode 2 beside ACARS, and an airband capture plan" and "Feature: VDL Mode 2 and HFDL rows (the other aircraft data links)" with one entry "Aircraft data links: done in plan 30 (`docs/superpowers/specs/2026-11-06-aircraft-links-design.md`, `docs/superpowers/plans/2026-11-06-aircraft-links.md`); the airband named set is plan 31's". Add "Aircraft links: deferred minors from plan 30": `adsb_decode` has no `subject.merge` yet (one line adds `["icao"]`); ADS-C over the X.25/ATN half is not mapped; the VDL2 fixture is unrecorded until UAT (and the HFDL one, when Task 8 was skipped); the ADS-C tag wrapper is confirmed only on the fixture; `--source-position` is per node start, not an op; the merged table's `Via` lists protocols, not job ids.
- [ ] **Step 2: Plan series** — row 30 (built on this branch, commit range, "see Plan 30 status"); the status section: what shipped (the schema additions, the two rows, the fixture lookup, the source position and the Kiwi's status read, the merged table, the fixture or its absence), what the gate proved (the row and adaptor tests, the fake-program checks, the e2e scenario, the Kiwi tests), the calls made (the "Calls made in this plan" list below, in one paragraph), what waits for the operator (UAT below).
- [ ] **Step 3: UAT.md** — "Plan 30: aircraft data links" with the operator's tasks: (1) record the VDL2 fixture together: pin the B210 at 133 500 000 Hz / 8 000 000 Hz, `capture` on 136 975 000 Hz / 25 000 Hz, review, commit, `zeta modes check --strict vdl2_decode` is `ok`, the ADS-C pointers confirmed on a frame with a basic report; (2) `plan_start acars_vhf` and `vdl2_decode` on 136.975 MHz inside the pin: one aircraft table with `Reg`/`Flight`/`Text` and `Via`, the ground station's row, a `position` on an ADS-C frame, `knowledge_lookup` naming `vdl2_decode`, no `external.unparsed` growth; (3) add your Kiwi to your instance as `--source-kiwi` (your call, your configuration; nothing in the repository names it): `source_list` shows its `position` (`from: status`, or set `--source-position`), `hfdl_decode` on one of the nearest station's listed frequencies gives a squitter within 40 s and an aircraft row with a position when one logs on; (4) when Task 8 was skipped: record the HFDL fixture as spec 4.6 says and `zeta modes check --strict hfdl_decode` is `ok`.
- [ ] **Step 4: Gate** — `cargo fmt -p <each crate> -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium`, `cargo test -p zeta --test ui -- --nocapture` (no `ui e2e skipped`), `cargo run -q -p zeta -- agents-md --check`, `cargo run -q -p zeta -- agents-md --skill --check`, `bash scripts/public-check.sh`, `bash scripts/check-doc-links.sh`. Expected: all green.
- [ ] **Step 5: Commit**

```bash
git add TODO.md UAT.md docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md
git commit -m "Docs: plan 30 (aircraft data links) in TODO, UAT and the plan series status"
```

Acceptance: the gate is green; TODO carries no stale VDL2/HFDL note; UAT lists the four operator tasks.

---

## Calls made in this plan (the builder's, on the spec's silence)

1. `FieldPath` is an untagged `String | Vec<String>`; the map is `Option<BTreeMap<String, FieldPath>>` on `ProgramOutput` (spec 6 left the shape open).
2. The "every schema property is mapped" rule exempts `ENVELOPE_FIELDS` (`protocol`, `summary`, `t_ns`, `position`), since existing rows list `protocol` in their schema and `envelope` adds all four after the map.
3. `apply_fields` runs for every parser, not only `json_lines`; a flat object under `/name` pointers is unchanged, so no existing row moves.
4. The fixture lookup is decided in `check_row` before a node opens, like `<program> missing`, so a missing fixture costs no node.
5. The two rows ship with placeholder `test.path`s and `expect`s until their fixtures exist; the fake-program checks (`fake_lines --lines-file`) prove the maps in CI, and the test vectors are synthetic until a fixture replaces them.
6. The source position is a `Port` accessor plus a generic, repeatable `--source-position ID=LAT,LON[,ALT_M]` kept in `NodeConfig.source_positions`, refused at startup for an id no source flag defines; it is not an op (a gist in Task 10).
7. The Kiwi's status read is one HTTP/1.0 GET on a fresh `TcpStream` per connection, bounded by `handshake_timeout_ms`, counted on failure and never a fault.
8. The merged table is a view in `SubjectsPane` over per-job subjects, grouped by `subject.name` and joined by union-find on `merge` fields; the store, the map and selection keep per-job identity; `adsb_decode` is not given `merge`.
9. Task 8 is conditional on a developer shell reaching the local KiwiSDR through `ZETA_KIWI_HOST`; the plan never names the host and never configures the deployed node; a skipped Task 8 becomes a UAT task.
10. The VDL2 fixture is not recorded by this plan (the operator's decision 6); the ADS-C pointer paths are confirmed from upstream source now and on the fixture later.

## Answered by the operator (2026-11-06)

Spec section 7: keying stands and the row gains `position` (decision 4); no B210 re-pin now (6); the operator adds their own Kiwi, the plan only makes a Kiwi work and may use the local one for tests from a developer shell (7); per-source position now (10); one aircraft table across ACARS and VDL2 in scope (20); catalogue left as upstream (21).

## Self-review notes

- Spec coverage: 4.1 → Tasks 1, 2; 4.2 → Tasks 1, 2; 4.3, 4.4 → Task 4 (fixtures: Task 8 and UAT); 4.5 → Tasks 5, 6; 4.6 → Tasks 3, 8; 4.7 → Task 4 (`install`, `notes`); 4.8 → Task 7; 4.9 → Tasks 9, 10; 5.1 → Tasks 1, 4; 5.2 → Tasks 2, 4; 5.3 → Task 3; 5.4 → Tasks 5, 6, 7; 5.5 → Task 10's UAT; 6 → Tasks 4 (paths), 8 (12 kHz), 6 (status lines), 1 (`FieldPath`), 7 (DOM).
- Names used across tasks: `FieldPath`/`pointers`/`ProgramOutput.fields`/`Subject.merge`/`center_khz_text`/`ENVELOPE_FIELDS` (1 → 2, 4, 7), `apply_fields` (2), `fixture_path`/`FIXTURES_DIR_ENV` (3 → 4, 8), `SourcePosition`/`PositionFrom`/`Port::position`/`SourceInfo.position`/`NodeConfig.source_positions` (5 → 6), `MERGE_SECTION_CLASS`/`section.subject-merged` (7).
- Where the spec no longer matches the code (reported to the caller too): `Capabilities` lives in `crates/zeta-core/src/types.rs`, not `port.rs`; `*.sigmf-data` is ignored repository-wide (the existing fixtures were force-added), so Task 3 adds the un-ignore rule; the fake program the spec's 5.2 proposed (`fake-dumpvdl2.sh`) already exists as `crates/zeta-ext/tests/fake_lines` with `--lines-file`, which the e2e's fake rows use, so the plan uses it; `modes check` takes one id (`id: Option<String>`), so UAT runs it per row; the Kiwi crate has no HTTP client (websocket only), hence the raw GET; `SourceInfo` is built in `ops.rs` under `Op::SourceList`, not in `node/`; the plan series table ends at plan 26 (no rows 27-29), so Task 10 adds row 30 only.
