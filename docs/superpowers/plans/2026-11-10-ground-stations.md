# Plan 35: Ground stations as origins Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ground-station traffic on a data-link job lights the register's transmitter sites as `heard` (graded `from` for a station's own frame, `to` for an aircraft's frame addressed to it) and the node keeps a per-job tally of the stations, in `job_context.stations`.

**Architecture:** A row's `subject.station` (`when` pairs, `key`, `addressed`) says which events are a station's own and which name the station an event is sent to. The identity tee in `node/launch.rs` becomes a rules tee that also counts stations into a tally in the job's `JobEntry`; the tally is written to `job_context.stations` at a throttle (at once on a new station or a grade rise, else within 60 s by a per-job timer, and at the run's end by the supervisor) through `update_origins`, which now also sets `heard` on every kept origin and copies the tally into the answer and the `origin` event. The page paints a heard site solid at full freshness from the job's last station frame, and the Subjects table marks station rows.

**Tech Stack:** Rust (tokio, serde, schemars), the `modes/*.json` rows, vanilla JS page (`overlays.js`, `app.js`, `map.js`, `subjects.js`, `style.css`), the Playwright e2e suite.

**Spec:** `docs/superpowers/specs/2026-11-10-ground-stations-design.md` (all of it; section 7 lists the calls, section 3 the code facts the reviewer verified). Depends on plan 34 only for the ACARS row's `downlink` field (Task 5); every other task stands alone.

## Global Constraints

- `Origin.heard: Option<Heard>` and `OriginAnswer.stations: BTreeMap<String, StationSeen>` are `#[serde(default)]` with nullable/defaulted schemas, so a `job_context.origins` persisted before this plan still deserialises (`update_origins` reads a kept answer with `from_value(...).ok()`, so a failure would silently drop it).
- `heard` goes on every origin of an answer (all are transmitter rows); the score is never changed by this plan; nothing is ever `confirmed` by a station frame.
- Throttle: `STATION_REPUBLISH_MS: u64 = 60_000` in `origin.rs`. Writes: at once on a new key or a key's first `from`; else one timer per job for the window's remainder; at the run's end by the supervisor, never by the tee (the tee's `biased` select returns on cancellation first).
- The tally is seeded from the record's `context.stations` at launch (a new `JobEntry` is inserted on every launch), so relaunches and node restarts continue the counts.
- Pair matching: on the node, the event has the field and `serde_json::Value` equality; on the page, one shared helper with `String(have) === String(v)` used by `subjects.js` (`skipsMerge`) and `overlays.js`.
- Rows: `vdl2_decode` `station: {"when": {"addr_type": "Ground station"}, "key": "icao", "addressed": "dst"}`; `acars_decode` `{"when": {"downlink": false}, "key": null, "addressed": null}` (after plan 34); HFDL none (a TODO gist).
- Summary wording as spec 4.3; `ORIGIN_SUMMARY_SITES` keys and sites; the anonymous key reads `uplinks heard <n> frames`.
- Tests use the synthetic register of `crates/zeta-core/tests/origin_ops.rs` and `crates/zeta/tests/origin_identity.rs` (`Example` licensees at the demo station). No observed station address, site or callsign in tracked files; roles, never host names.
- AGENTS.md and SKILL.md regenerated after any schema or prose change; both `--check`s pass. Gates as CLAUDE.md, private `CARGO_TARGET_DIR`; one commit per task, `Plan 35: <what changed>`.

## Models per task

Opus for Tasks 2 and 3 (the tally's concurrency and the supervisor flush) and Task 4 (the page); Sonnet for Tasks 1, 5 and 6.

## Review Focus

Inputs the spec implies, each pinned by a test in the owning task.

1. A node restart with a job whose `context.stations` holds counts: the resumed run continues them rather than starting at zero. Task 3.
2. Two `graph put`s in quick succession (two overlapping runs of one id): the earlier run's flush must not overwrite a later run's newer tally. The flush writes only when its run is the entry's current run (`JobEntry.run`). Task 3.
3. An `origin_lookup` by band (the read-only form) never carries `heard` or `stations` (no job, no tally). Task 2.
4. A kept answer written before this plan (no `heard`, no `stations` keys) deserialises and gains them on the next write. Task 2.
5. The page with a lit set for a job whose row has `station` but which has not yet seen a station frame: brightness falls back to any event (no dead marker). Task 4.

---

### Task 1: `subject.station` in the row schema

**Files:**
- Modify: `crates/zeta-core/src/modes/row.rs` (`Subject` after `identity`; a `Station` struct beside `Identity`; `validate` beside the `merge_skip` check)
- Modify: `crates/zeta-core/src/modes/mod.rs` (re-export `Station`)
- Test: `row.rs` tests module (`edited(ADSB, ...)`, `parse`, `validate`)

**Interfaces:**
- Produces: `pub struct Station { pub when: BTreeMap<String, serde_json::Value>, pub key: Option<String>, pub addressed: Option<String> }` (`#[serde(deny_unknown_fields)]`, schemars `rename = "ModeStation", transform = require_all`, `key` and `addressed` `#[serde(default)]`); `Subject.station: Option<Station>` with `#[serde(default)]`; `pub fn Station::matches(&self, event: &serde_json::Value) -> bool` (every `when` pair present and equal).

- [ ] **Step 1: Write the failing tests**: `station_round_trips_and_defaults_null`; `station_names_schema_fields` (a `when` key, `key` and `addressed` not in the schema → `subject.station.when.nope`, `subject.station.key`, `subject.station.addressed`, each `not in event.schema.properties`); `an_empty_station_when_is_refused` (`subject.station.when: empty`); `station_matches_every_pair_present_and_equal` (missing field → false; `true` vs `"true"` → false).
- [ ] **Step 2: Run** `cargo test -p zeta-core --lib modes::row::tests::station`: compile error.
- [ ] **Step 3: Implement** the struct, the field, `matches`, the validation arms.
- [ ] **Step 4: Run** `cargo test -p zeta-core --lib modes::row`; regenerate AGENTS.md/SKILL.md; both `--check`s PASS.
- [ ] **Step 5: Commit**: `Plan 35: a row's subject names its fixed stations (station: when, key, addressed)`.

---

### Task 2: `heard`, `stations` and the station summary on the origin answer

**Files:**
- Modify: `crates/zeta-core/src/node/origin.rs` (`Origin`, `OriginAnswer`, new types; `summary`; `publish_origin`; `answer` for the fresh-lookup case)
- Test: `origin.rs` unit tests; `crates/zeta-core/tests/origin_ops.rs`

**Interfaces:**
- Produces: `pub struct StationSeen { pub from: u32, pub to: u32, pub first_ns: Timestamp, pub last_ns: Timestamp }`; `pub enum Grade { From, To }` (`snake_case`); `pub struct Heard { pub last_ns: Timestamp, pub grade: Grade }`; `Origin.heard: Option<Heard>`, `OriginAnswer.stations: BTreeMap<String, StationSeen>`; `pub fn heard_of(stations: &BTreeMap<String, StationSeen>) -> Option<Heard>` (newest `from` if any `from > 0`, else newest `to`, else None); `pub fn apply_stations(a: &mut OriginAnswer, stations: &BTreeMap<String, StationSeen>)` (copies the map, sets `heard` on every origin); `pub fn station_summary(a: &OriginAnswer) -> String` (spec 4.3 wording; the tail is today's `summary` for the sites or the empty-origins phrasing).

- [ ] **Step 1: Write the failing unit tests**: `heard_prefers_from_over_a_newer_to`; `apply_stations_marks_every_origin`; `station_summary_one_heard_two_addressed_and_uplinks` (asserting the exact strings for: `{"1A2B3C": from 3}` → `1 ground station on 136.975 MHz: 1A2B3C heard 3 frames; Example ...`; two `to` keys in descending total; `"*"` → `uplinks heard 2 frames`); `station_summary_with_no_origins_keeps_todays_tail` (the three phrasings); `an_answer_persisted_without_heard_and_stations_deserialises` (a JSON literal of a pre-plan answer → `heard` None, `stations` empty) (Review Focus 4).
- [ ] **Step 2: Run** `cargo test -p zeta-core --lib node::origin::tests`: compile errors.
- [ ] **Step 3: Implement** the types and functions; add `"stations": a.stations` to `publish_origin`'s payload; in the fresh lookup path (`answer` by job), after the answer is built, `apply_stations` from the job's tally (Task 3's accessor; until then an empty map).
- [ ] **Step 4: Write the failing integration test** in `origin_ops.rs`: `the_band_form_carries_no_heard_or_stations` (Review Focus 3) and extend `a_decoder_job_reads_its_rows_labels_and_publishes_once` to assert the `origin` frame has `stations: {}` and each origin `heard: null`.
- [ ] **Step 5: Run** `cargo test -p zeta-core --lib node::origin` and `cargo test -p zeta-core --test origin_ops`. Expected: PASS. Regenerate AGENTS.md/SKILL.md (schema changed); `--check`s PASS.
- [ ] **Step 6: Commit**: `Plan 35: an origin answer carries heard (from|to) on every site and the stations tally; the station summary`.

---

### Task 3: The tally: the rules tee, the throttle timer, the supervisor flush, the launch seed

**Files:**
- Modify: `crates/zeta-core/src/node/mod.rs` (`JobEntry` gains the tally)
- Modify: `crates/zeta-core/src/node/launch.rs` (`tee_identity` → `tee_rules` and its call site; `record_running` seeds the tally; the supervisor flushes before `bus.unregister`)
- Modify: `crates/zeta-core/src/node/origin.rs` (`Inner::note_station`, `Inner::write_stations`, `Inner::stations_of`)
- Test: `crates/zeta/tests/origin_identity.rs` (new tests beside `a_decoded_callsign_confirms_a_lit_site_and_republishes`, with a station row modelled on `identity_row()`), `origin.rs` unit tests for the throttle decision

**Interfaces:**
- Consumes: `Station::matches` (Task 1); `apply_stations`, `station_summary`, `STATION_REPUBLISH_MS` (Task 2).
- Produces: in `JobEntry`: `stations: StationTally` where `pub struct StationTally { pub seen: BTreeMap<String, StationSeen>, pub dirty: bool, pub written_ns: Option<Timestamp>, pub timer_armed: bool }`; `Inner::note_station(&self, id: &JobId, run: u64, key: &str, grade: Grade, t_ns: Timestamp) -> WriteNow` with `pub enum WriteNow { Now, Arm(Duration), Later }` (the throttle decision, pure, tested as `fn decide(tally: &StationTally, key_is_new: bool, grade_rose: bool, now: Timestamp) -> WriteNow`); `Inner::write_stations(&self, id: &JobId, run: u64, source: &SourceId) -> bool` (no-op unless dirty and `run` is the entry's current run; `update_origins` with `apply_stations` + `station_summary` when an answer is kept, else writes `context.stations` and persists); `Inner::stations_of(&self, id: &JobId) -> BTreeMap<String, StationSeen>` for Task 2's fresh-lookup path.

- [ ] **Step 1: Write the failing throttle unit tests** in `origin.rs`: `a_new_key_writes_now`, `a_grade_rise_writes_now`, `a_repeat_inside_the_window_arms_the_remainder` (asserting the `Duration`), `a_repeat_with_a_timer_armed_is_later`, `a_repeat_past_the_window_writes_now`.
- [ ] **Step 2: Run**: compile error. **Step 3: Implement** `decide` and the types.
- [ ] **Step 4: Write the failing integration tests** in `origin_identity.rs` (a fake-lines station row: `station: {"when": {"kind": "gs"}, "key": "gs", "addressed": "to_gs"}`, lines: an aircraft line with `to_gs: "GS1"`, a station line `kind: "gs", gs: "GS1"`, an aircraft line with `to_gs: "GS2"`):
  - `station_frames_fill_the_tally_and_republish_with_heard`: after `look_up`, `job_context.stations` has `GS1 {from 1, to 1}` and `GS2 {from 0, to 1}`; the last `origin` frame in `events.log` has `stations` and every origin `heard.grade == "from"`; its summary starts `2 ground stations on`.
  - `the_tally_is_written_within_the_window_not_per_frame`: with the lines file repeated every 200 ms for 3 s, the `origin` frames logged number at most 3 (the first key, the second key, the grade rise), none from repeats.
  - `a_restart_continues_the_counts` (Review Focus 1): stop the node, start it on the same state dir, wait for the resumed job's next write: `from`/`to` are larger than before, not reset.
  - `an_older_run_never_overwrites_a_newer_tally` (Review Focus 2): a unit test on `write_stations` with a mismatched `run` returning false and changing nothing.
- [ ] **Step 5: Run** `cargo test -p zeta --test origin_identity`: FAIL (no tally).
- [ ] **Step 6: Implement**: `tee_rules` (identity branch unchanged; station branch: `matches` → `from` under `key` (or `"*"`), else `addressed` non-blank → `to`; then `note_station`; on `Now` call `write_stations`; on `Arm(d)` spawn one `tokio::spawn` with a `Weak<Inner>` and the run's token that sleeps `d` then calls `write_stations`; set up the tee when the row has `identity` or `station`); `record_running` seeds `stations.seen` from `record.context["stations"]`; the supervisor calls `write_stations` after `finish` and before `bus.unregister`.
- [ ] **Step 7: Run** `cargo test -p zeta --test origin_identity`, `cargo test -p zeta-core --test origin_ops`, `cargo test -p zeta-core --features test-fixtures --test watch` (the watch's trial jobs launch and end often: no panic, no leak). Expected: PASS.
- [ ] **Step 8: Commit**: `Plan 35: the station tally in the job entry, counted by the rules tee, written at once on a new station or grade rise, else within the window by a timer, and flushed by the supervisor; seeded at launch`.

---

### Task 4: The page

**Files:**
- Modify: `crates/zeta-ui/web/subjects.js` (export `matchesPairs(state, pairs)`; `skipsMerge` uses it; `station` class on rows), `crates/zeta-ui/web/overlays.js` (`lastStationEvent`), `crates/zeta-ui/web/app.js` (the `lastEvent` option at both `mountMap` sites), `crates/zeta-ui/web/map.js` (`data-heard`, brightness factor, bearing line, hover and popup lines), `crates/zeta-ui/web/style.css` (`[data-heard]` rules; a station row mark in the console theme if the theme files carry it)
- Modify: `crates/zeta/tests/ui.rs` (`fake_vdl2`: `dst` in schema and lines, the `station` block), `crates/zeta-ui/tests/e2e/ui.py` (`scenario_origins_light_sites` or a new `scenario_heard_sites`)

**Interfaces:**
- Consumes: `origin` payload `stations` and `origins[].heard` (Task 2); the row's `subject.station` (Task 1).
- Produces: `export function matchesPairs(obj, pairs) -> bool` in `subjects.js`; `ov.lastStationEvent(jobId) -> number | null`; `tr.subject.station`; `path.map-site[data-heard="from"|"to"]`.

- [ ] **Step 1: Edit `ui.rs`**: `fake_vdl2` lines gain `"dst": "1A2B3C"` on the aircraft lines and `"dst": "7C0000"` on the station line; schema gains `dst`; the subject block gains `"station": {"when": {"addr_type": "Ground station"}, "key": "icao", "addressed": "dst"}`.
- [ ] **Step 2: Write the failing e2e assertions** (in `ui.py`, the origins scenario on `fake_vdl2` with the synthetic register, or a new scenario): after the Origins button, `path.map-site[data-heard="from"]` exists; its hover contains `ground station heard`; `tr.subject.station[data-key="1A2B3C"]` exists and no `tr.subject.station[data-key="7C0000"]`; with `view set origin_fade_s 2`, the site's `--site-brightness` falls below 0.5 within 3 s with no new station frame (Review Focus 5 is the fallback: before any station frame a row-with-station job's lit set still brightens on an aircraft event; assert the brightness is 1 right after the first aircraft frame on a fresh lines file without the station line, if the scenario can stage it; else a unit assertion in a small JS test is not available, so note it in UAT).
- [ ] **Step 3: Run** `ZETA_UI_ONLY=<scenario> cargo test -p zeta --test ui -- --nocapture`: FAIL.
- [ ] **Step 4: Implement** the five JS/CSS changes (spec 4.5): `matchesPairs`; `lastStationEvent` recorded in `ov.render`'s event path for a watched job whose row has `station`; `app.js` option `(id) => state.ov?.lastStationEvent(id) ?? state.ov?.lastEvent(id) ?? null`; `map.js` `drawSet`: `el.dataset.heard = grade` when `site.best.heard`, bearing line also when heard, `part.score = 1` when heard; hover and popup lines; CSS.
- [ ] **Step 5: Run** the scenario, then the whole suite once (`cargo test -p zeta --test ui -- --nocapture`, about 5 min; check for `ui e2e skipped`). Expected: PASS.
- [ ] **Step 6: Commit**: `Plan 35: the page paints a heard site solid at freshness from the job's last station frame, names the grade in the hover, marks station rows in the Subjects table`.

---

### Task 5: The rows

**Files:**
- Modify: `modes/vdl2_decode.json` (`subject.station`, `notes`), `modes/acars_decode.json` (`subject.station` with `downlink`; **only once plan 34's Task 3 and 4 have landed**, else skip the ACARS edit and say so in the commit)
- Test: `cargo test -p zeta-core --test modes`, `cargo test -p zeta --test cli`

- [ ] **Step 1: Edit** the rows per Global Constraints; VDL2 `notes` gain "a ground station's frames, and the aircraft frames addressed to it, light the register's transmitter sites as `heard`; `job_context.stations` counts them".
- [ ] **Step 2: Run** the two test targets and `cargo run -q -p zeta -- modes check --strict vdl2_decode` where dumpvdl2 is installed (else it prints `skipped`). Regenerate AGENTS.md/SKILL.md. Expected: PASS.
- [ ] **Step 3: Commit**: `Plan 35: the VDL2 row names its ground stations (icao, addressed by dst); the ACARS row its uplinks`.

---

### Task 6: Agent prose, docs, TODO and UAT

**Files:**
- Modify: `crates/zeta/src/agents_md.rs` (the `origin_lookup` note: `heard` `{last_ns, grade}` and `stations`, how to read them, still not `confirmed`), `docs/guide.md` (map paragraph: one sentence), `docs/agents/field-craft.md` (register bullet: one sentence), `TODO.md` (gist "Modes: HFDL ground stations as origins", spec non-goals text), `UAT.md` (a "Plan 35" section from spec 5.5)
- Regenerate: `AGENTS.md`, `skills/zetasdr/SKILL.md`

- [ ] **Step 1: Edit** the five documents and regenerate.
- [ ] **Step 2: Run** `bash scripts/check-doc-links.sh`, both `agents-md --check`s, `bash scripts/public-check.sh`, then the full gates (Global Constraints). Expected: all clean and PASS.
- [ ] **Step 3: Commit**: `Plan 35: docs: heard sites and the station tally in the agent guide, the guide and the field craft; HFDL stations gist; UAT checks`.
