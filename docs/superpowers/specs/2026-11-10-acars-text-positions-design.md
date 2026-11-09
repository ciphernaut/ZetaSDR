# Plan 34, positions from ACARS text: the H1, 80, 16 and QQ position reports

Design spec, 2026-11-10. From the operator's question of the same day: why the aircraft heard
on the ACARS and VDL Mode 2 jobs never reach the map; and from the `TODO.md` gist "Modes:
positions in ACARS free text" (2026-11-08, a label `3L` text with a slash-separated fix), which
this plan retires. A reviewer read it as the builder before hand-over; its two questions
(uplinks, the `3L` form) are settled in sections 4.1 and 4.3, and section 7 records the calls
made here. Builds on the mode registry
(`docs/superpowers/specs/2026-10-27-mode-registry-design.md`, `crates/zeta-core/src/modes/`),
the event envelope (`crates/zeta-core/src/modes/summary.rs`), the aircraft data links
(`docs/superpowers/specs/2026-11-06-aircraft-links-design.md`: `decoder.output.fields`, the
`vdl2_decode` and `hfdl_decode` rows) and subjects
(`docs/superpowers/specs/2026-11-02-subjects-design.md`). The map (plan 32) is not changed: it
already draws every event that carries a `position`.

## 1. Intent and why

The page plots an event when the row's `position` map finds numbers in the event's `lat` and
`lon` fields. Today three aviation rows can supply them:

- `adsb_decode`, from dump1090's decoded position: thousands of fixes a day on the field host.
- `vdl2_decode`, only from an ADS-C basic report that libacars decodes inside dumpvdl2
  (`/vdl2/avlc/acars/arinc622/adsc/tags/<i>/basic_report`): one fix in two days on the field
  host.
- `acars_decode` has no `position` map at all, and the in-process stage emits only the block's
  fields (`reg`, `flight`, `label`, `text`, ...), so nothing an ACARS job produces can be
  plotted. It never could; the operator's memory of ACARS on the map was ADS-B.

Yet the aircraft do say where they are. On the field host's event log of the last two days,
about 113 of the ACARS and VDL2 events carried an ACARS payload, and 17 of them a fix in the
message text, against the single ADS-C report:

| Form | Label | Seen | Example (synthetic, at the demo station) |
|---|---|---|---|
| FMS position report, degrees and tenths of minutes | H1 (`#M1B` sublabel) | 13 | `POSS27231E153071,ALPHA,025131,165,BRAVO,025351,CHARL,M9,252035,168` |
| Position report with `/POS` and `/ALT` | 80 | 1 | `3N01 POSRPT 0054/09 YBBN/YSSY VH-ZET\r\n/POS S27231E153071/ALT 255/MCH 743/FOB 133.7` |
| Decimal degrees with hemisphere letters | 16, 3L | 1 (+ the TODO's `3L`) | `194606,28825,2106,  91,S 27.384 E153.118`; `S 27.384/E153.118` |
| Degrees and minutes to one decimal, run together | QQ | 1 | `YBBNYSSY 2003\r\n001FE09200331S2723.1E15307.1017214` |
| Signed decimals under a `LAT LON` header | H1 (`RSI`) | 1 | `  LAT      LON      GALT\r\n-27.3842  153.1175  31480` |

libacars 2.2.1 decodes ADS-C, CPDLC, MIAM and the media advisory, not these free-text reports,
so dumpvdl2 and dumphfdl leave them in `msg_text`, and the in-process ACARS stage has no parser
for them. The fix is a text extractor the rows can name: parse the position report out of the
text into `lat`, `lon` and `alt_ft`, let the row's `position` map do the rest, and the aircraft
appear where the ADS-C ones do, in the aircraft table and on the map.

What the operator wants: the aircraft heard on the seven ACARS jobs and the VDL2 job on the map
with a track, as ADS-B's are, without a page change and without a per-protocol special case.
Two facts shape the design:

1. The in-process stage (`zeta-dsp`'s `util::event`) and the program adaptor (`zeta-ext`'s
   `Publisher::publish`) both finish an event through the one `summary::envelope`. A step
   there serves ACARS, VDL2 and HFDL alike: the same report is parsed whichever link carried it.
2. The formats need arithmetic (degrees and minutes to decimal degrees, a flight level to feet)
   and strict acceptance, so the extractor is code in core, named from the row as data, the way
   a row names a parser. The hook is generic; the first extractor is the aviation one.

## 2. Goals, non-goals, anti-goals

### Goals

- A row can declare `derive`: fields filled from another field's text by a named extractor,
  only where the decoder left them empty and only on events that pass the entry's `when`
  condition (here: downlinks, the aircraft's own words). The first extractor,
  `acars_position`, parses the four report forms of the table above (H1 FMS, label 80 `/POS`,
  labels 16 and 3L decimal, QQ run together) into `lat`, `lon` (decimal degrees, south and
  west negative) and, where the form states it unambiguously, `alt_ft`.
- `acars_decode` gains the three fields, a `position` map and the derive, so an ACARS job's
  event with a position report carries `position`, and its aircraft get a marker and a track
  on the map and a position in the Subjects table, like VDL2's today.
- `vdl2_decode` and `hfdl_decode` gain the derive: an ADS-C or HFDL structured fix still wins;
  a frame without one but with a text report gets its fix from the text.
- `zeta modes check --strict acars_decode` proves it end to end: the row's synthesised test
  message is a position report at the demo station and `expect` names the fix.
- The agent guide says that an aircraft's position may come from the text, which labels carry
  one and that the fix is the aircraft's own report, not a measurement.

### Non-goals

- The `RSI` table form (signed decimals under a `LAT LON` header) and any other airline-specific
  layout. One in eighteen; a later extractor or a widening of this one, once more of it is seen.
- Positions from uplinks. A ground station's H1 uplink carries route and flight-plan
  waypoints in the very shapes forms 2 and 3 read (`N4530.0W07345.0`), so an uplink is never
  parsed: the `when` gate of section 4.1 keeps the extractor to downlinks on every row. Nor
  CPDLC, nor the `#M1A`/`#M2` sublabels other than the position report.
- Speed, heading, destination or ETA from the report. The H1 form carries waypoints and times;
  only the fix and the flight level are taken. `subject.heading` stays null on the ACARS row.
- A page change. The map and the Subjects table already handle a `position` from any row; the
  cap on positioned jobs on other sources (`MAX_POSITIONED`, `TODO.md` "Page: positioned jobs
  on other sources onto their feeds") is unchanged, see section 3.
- A regex engine in `zeta-core`. The forms are regular enough for a hand-written scanner.

### Anti-goals

- A false fix. An engine report, a fuel figure, a time or a frequency in the text must never
  become a position: every accepted form needs both hemisphere letters, degrees and minutes in
  range, and the two axes adjacent. A text the extractor is not sure of yields nothing.
- A wrong altitude. Where a form's altitude field is airline-specific (label 16, QQ), `alt_ft`
  stays null; the subject keeps the last altitude it had, as the store already does for a fix
  without one.
- Protocol-specific code outside the extractor. The hook, the validation and the envelope step
  know nothing of ACARS.
- Real traffic in the repository. Fixtures and tests use synthetic texts placed at the demo
  station (Brisbane Airport, -27.3842, 153.1175), the registration `VH-ZET`, invented waypoints
  and flight numbers. No observed registration, waypoint or fix from the field host's log is
  committed.

## 3. Constraints

- A row is `deny_unknown_fields`; a new optional field is `#[serde(default)]`; a bad row is
  `broken` with `<field>: <reason>` (`RowError`), never a dead node (`row.rs`). The JSON schema
  of a row is generated from the structs (`schemars`), so `modes_show --schema` and AGENTS.md
  follow the code. AGENTS.md and `skills/zetasdr/SKILL.md` are regenerated, never edited.
- `summary::envelope(row, start_ns, fields)` is the one place every decoder's event passes:
  `util::event` in `zeta-dsp` (graph rows, when the stage context has the row) and
  `Publisher::publish` in `zeta-ext` (program rows, after `decoder.output.fields` flattened the
  line). It sets `protocol`, `summary`, `t_ns`, `position` and `access`. The derive step runs
  first inside it, so `summary` can show a derived field and `position` can read one.
- `validate` requires, for a program row with `decoder.output.fields`, that every
  `event.schema.properties` entry outside `ENVELOPE_FIELDS` is mapped by `fields`. A derived
  field is mapped by `derive`, and the rule must admit it (section 4.2). The in-process ACARS
  row is a graph row, where the rule does not apply.
- A row's `test.expect` accepts `{near, within}` for numbers (plan 29), and the ACARS test is
  `kind: synth` with a free `text`: the synthesiser carries any text, so the row's own check can
  exercise the extractor.
- dumpvdl2 and dumphfdl strip the H1 sublabel into their own fields (`sublabel`, `mfi`), so a
  VDL2 or HFDL text begins `POS...` where a plain ACARS text begins `#M1BPOS...`. The
  extractor accepts both.
- The aircraft table merges an aircraft's rows across the links and takes the newest member's
  position (`subjects.js`, plan 30 spec 4.8). A text fix on ACARS and an ADS-C fix on VDL2 for
  one aircraft already fold into one marker.
- A page showing another source follows the newest `MAX_POSITIONED` (4) jobs whose row has a
  `position` or a `subject` (`overlays.js` `syncPositioned`). `acars_decode` has a `subject`,
  so its seven jobs on the field host already fall under that cap today; the position map
  changes nothing there. A page on the Kiwi plots only the newest four ACARS jobs: the
  existing cap and the existing TODO ("Page: positioned jobs on other sources onto their
  feeds"); the plan notes it in the UAT, nothing more.
- Direction. Plain ACARS has no direction field: a downlink block's `block_id` is a digit, an
  uplink's a letter (`crates/zeta-dsp/src/acars.rs`, where the framer already uses the digit
  to split `msg_no` and `flight` off the text). dumpvdl2 names the frame's source type
  (`/vdl2/avlc/src/type`, the row's `addr_type`, `Aircraft` or `Ground station`); dumphfdl
  names its PDU source type (`/hfdl/lpdu/src/type`, `Ground station` in the uplink fixture;
  the aircraft value is confirmed by the plan against dumphfdl's formatter).
- Repository rules: no real site names, observed frequencies or positions in tracked files;
  `scripts/public-check.sh` clean before `main`.

## 4. The design

### 4.1 `derive`: fields from a field's text, by a named extractor

A new optional row field:

```json
"derive": [
  { "when": { "downlink": true },
    "from": "text", "extract": "acars_position",
    "into": { "lat": "lat", "lon": "lon", "alt_ft": "alt_ft" } }
]
```

- `when`: event field → value, every pair of which must hold (JSON equality) for the entry to
  run; the shape of `subject.merge_skip`, which skips a VDL2 ground station the same way. An
  empty object runs on every event. Here it keeps the extractor to downlinks (section 4.4
  names each row's pair).
- `from`: the event field whose string value the extractor reads. Not a string, or absent:
  the entry yields nothing.
- `extract`: the extractor's name. The set is fixed in core (`Extractor` enum, serde
  `snake_case`); the first and only member is `acars_position`. An unknown name must be
  reported at `derive[i].extract`: `parse` names an unknown word only through
  `bad_word_field` (`row.rs`), which knows four fields today, so it learns to walk
  `derive[i].extract`.
- `into`: extractor output → event field. Each extractor documents its outputs
  (`acars_position`: `lat`, `lon`, `alt_ft`). An output left out of `into` is dropped. When
  any `into` target already holds a non-null value the whole entry is skipped (the decoder's
  structured fix wins over the text, and a fix is never assembled half from each: an ADS-C
  latitude is never paired with a text longitude). `into` is required, so a row says what it
  takes.
- Several entries run in order; a later one sees the earlier one's fields.

Applied by `envelope` before anything else it does. `fields` must be an object (it always is
from both callers); the step is a no-op for a row without `derive`.

The extractor trait is one function, `fn extract(&self, text: &str) -> BTreeMap<&'static str,
Value>`, returning only the outputs it found. It lives in `crates/zeta-core/src/modes/derive.rs`
with its tests.

### 4.2 Validation

In `validate`, for each `derive[i]`:

- every `when` key must be an `event.schema.properties` key, else `derive[i].when.<key>: not
  in event.schema.properties`;
- `derive[i].from` must be an `event.schema.properties` key, else `derive[i].from: not in
  event.schema.properties`.
- every `into` key must be an output of the extractor, else `derive[i].into.<key>: not an
  output of acars_position (lat, lon, alt_ft)`;
- every `into` value must be an `event.schema.properties` key, else `derive[i].into.<key>: not
  in event.schema.properties`.

The program-row rule "every schema property is mapped by `decoder.output.fields`" admits a
property that any `derive[i].into` writes. The ACARS row, a graph row, lists `lat`, `lon` and
`alt_ft` in its schema as nullable numbers and integers, as VDL2's does.

### 4.3 The `acars_position` extractor

Reads one text and returns the first fix it accepts, scanning left to right. Four forms, in
the order they are tried; the first that matches ends the scan.

1. **Compact degrees and minutes** (H1 FMS report, label 80 `/POS`): a hemisphere letter `N`
   or `S`, two degree digits, three minute digits (minutes times ten), then at most one space,
   then `E` or `W`, three degree digits, three minute digits. `S27231E153071` is 27° 23.1' S,
   153° 07.1' E: lat -27.385, lon 153.11833. Accepted only right after a `POS` token (the form
   appears as `POSS27231E153071` and as `/POS S27231E153071`): the scanner finds `POS` and reads
   the fix immediately after it, or after one space. Minutes under 600, lat degrees under 90,
   lon degrees under 180, both axes present.
   - Altitude: in the H1 FMS form the fix is followed by `,<waypoint>,<hhmmss>,<FL>,...`; the
     third comma-separated field after the fix is taken as a flight level when it is exactly
     three digits (`165` → 16 500 ft). In the label 80 form, `/ALT <n>` with `n` of two or
     three digits is a flight level. Otherwise `alt_ft` is absent.
2. **Decimal degrees with letters** (labels 16 and 3L): `N` or `S`, optional space, degrees
   with a decimal point (one or two integer digits, one to four decimals), then one or more
   spaces or one `/`, then `E` or `W`, optional space, degrees with a decimal point (one to
   three integer digits). `S 27.384 E153.118` and `S 27.384/E153.118`. In range as above. No
   altitude.
3. **Degrees and minutes to one decimal, run together** (QQ): `N`/`S`, two degree digits, two
   minute digits, a point, one decimal, then `E`/`W`, three degree digits, two minute digits, a
   point, one decimal: `S2723.1E15307.1`. Minutes under 60. No altitude.
4. Nothing else. In particular a run of digits without both hemisphere letters, a `LAT` header,
   a signed decimal or a fix whose axes are separated by anything but the allowed spaces or
   the one slash is not a fix.

Output `lat` and `lon` are rounded to five decimals (about a metre; the report's own precision
is 0.1 arc minute, about 185 m); `alt_ft` is an integer.

Rejections the tests pin: the engine report `-24.0-49.7 369900.760` (two signed numbers), a
fuel-on-board `FOB 133.7`, a time `025131`, a frequency `131550`, text with `POS` but no
letters after it (`POSRPT 0054/09`), a fix with minutes `S27650E153071` (650 ≥ 600), an `N`
followed by six digits inside a word (`LANDING123456E`).

### 4.4 The rows

`modes/acars_decode.json`:

- `event.schema.properties` gains `downlink` (`boolean`, section 4.5), `lat` and `lon`
  (`["number", "null"]`) and `alt_ft` (`["integer", "null"]`).
- `position`: `{"lat": "lat", "lon": "lon", "alt_m": "alt_ft", "alt_unit": "ft"}`.
- `derive`: the entry of section 4.1, `when: {"downlink": true}`.
- `notes` (the row's own field, rendered by `agents_md.rs` under `## Mode rows`; the row has
  none today): an event whose text is a position report (labels H1, 80, 16, 3L, QQ) carries
  `lat`, `lon`, sometimes `alt_ft`, and `position`; it is the aircraft's own report, typically
  a few minutes old by the time it is relayed; uplinks are never parsed.
- `subject` gains `"track_points": 100, "track_min_m": 1000`, as VDL2's: a position report
  every few minutes at 400 kt is 50 km apart, so the thinning never bites, but the two aviation
  rows should read the same.
- `test.text` becomes an H1 FMS report at the demo station,
  `#M1BPOSS27231E153071,ALPHA,025131,165,BRAVO,025351,CHARL,M9,252035,168`, and `expect` adds
  `"lat": {"near": -27.385, "within": 0.001}, "lon": {"near": 153.1183, "within": 0.001},
  "alt_ft": 16500`.
`modes/vdl2_decode.json`: the same `derive` entry with `when: {"addr_type": "Aircraft"}` (the
field the row already maps from `/vdl2/avlc/src/type`). The ADS-C fields keep precedence by
the fill-only-empty rule. Its `notes` gain the sentence above.

`modes/hfdl_decode.json`: a new mapped field `src_type` from `/hfdl/lpdu/src/type` (the
uplink fixture shows `Ground station` there; the plan confirms the aircraft value in
dumphfdl's formatter and the pointer on an aircraft frame), listed in the schema, and the
`derive` entry with `when: {"src_type": "<that value>"}`. Its `into` names `lat` and `lon`
only, the schema having no altitude field; or it gains `alt_ft` and an `alt_m` on its position
map, the builder's choice. HFDL's structured `pos` keeps precedence.

### 4.5 In-process stage

`AcarsBlock` gains `downlink: bool`, true when `block_id` is an ASCII digit (the rule the
framer already applies to split `msg_no` and `flight` off a downlink's text), so every event of
`acars_decode` carries it. Nothing else in `AcarsDecodeStage` changes: it serialises the block
and calls `util::event`, which calls `envelope` with the row. Without a row in the stage
context (a bare graph without a mode), the event has no derive and no position, as today.

### 4.6 Docs

- `docs/guide.md`, the decoders paragraph: one sentence, after the ADS-B and VDL2 map text,
  that ACARS, VDL2 and HFDL events whose text carries a position report also plot.
- `docs/agents/field-craft.md`: the ACARS and VDL2 bullets gain the sentence of section 4.4.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` regenerated.
- `UAT.md`: a plan 34 section (section 5.5).
- `TODO.md`: the gist "Modes: positions in ACARS free text" is removed (it also quotes an
  observed fix, which the repository rules keep out of tracked files), and a new one notes the
  `RSI` form and any further airline layouts, once the plan lands.

## 5. Testing

### 5.1 Extractor (`crates/zeta-core/src/modes/derive.rs` unit tests)

Each form at the demo station, north and west variants (`N27231W153071` → 27.385, -153.11833),
the slash and the space separators of form 2, the H1 flight level present and absent (a
two-digit or four-digit third field → no `alt_ft`), label 80 with `/ALT 255` → 25 500, every
rejection of section 4.3, a fix at the end of the text and one in the middle, text with two
fixes (the first wins), an empty text.

### 5.2 Row schema and envelope (`row.rs`, `summary.rs`)

- A row with `derive` parses; an unknown extractor is `derive[0].extract`; a bad `when` key,
  `from`, `into` key or `into` value names its field as in section 4.2.
- `envelope` skips an entry whose `when` does not hold: an uplink text with a waypoint in form
  3 yields no `lat`/`lon`; the same text on a downlink does.
- `AcarsBlock`'s `downlink` is true for a digit `block_id`, false for a letter and for none
  (`zeta-dsp` `acars.rs` tests).
- A program row whose schema lists `lat` mapped only by `derive` validates; one that lists a
  field mapped by neither is still `event.schema.properties.<name>: not mapped by
  decoder.output.fields`.
- `envelope` with a derive and a position map: a text with a fix yields `position`; a text
  without one yields no `position` and no `lat`/`lon` keys; a field already holding a number
  (the ADS-C case) is not overwritten; an `into` that leaves `alt_ft` out never writes it;
  `summary` sees the derived field.

### 5.3 Adaptor (`crates/zeta-ext/tests/external.rs`)

A dumpvdl2-shaped line (new fixture `dumpvdl2-h1pos.json`, synthetic: an aircraft frame whose
`acars.msg_text` is the H1 report at the demo station and no `arinc622`) through the VDL2 map
and derive (`mapped_row` in `external.rs` gains the entry) gives an event with `position` at
the demo station; the same line with the source type `Ground station` gives none; the existing
ADS-C fixture still gives its ADS-C position unchanged when the derive is added to the row.

### 5.4 `modes check` (`crates/zeta/tests/modes_check.rs`)

`zeta modes check --strict acars_decode` passes with the new text and `expect`: the
synthesiser, the MSK demodulator, the framer, the envelope and the extractor in one run.

### 5.5 Live (UAT, the field host)

With the seven ACARS jobs and the VDL2 job running on the B210 and the page on that radio:
within a day, `events.log` holds `acars` events with `position`; the Subjects table shows
those aircraft with a position; the map shows a marker per aircraft and a track for one heard
twice; an aircraft heard with a text fix on ACARS and an ADS-C fix on VDL2 is one marker. On a
page showing another radio, only the newest four ACARS jobs are followed (the existing cap):
note, do not fix here.

## 6. Left to the plan

- The exact `AcarsBlock` serialisation: whether the stage's event already has `lat`/`lon`
  keys as null (it does not today; the derive adds them only when found, and the schema's
  nullable types cover both).
- Whether `alt_ft` on the HFDL row is worth adding now (section 4.4).
- The exact wording and place of the per-row AGENTS.md note.
- Whether the H1 flight-level rule (third field, exactly three digits) holds on the field
  host's traffic: the plan's live task reads a day of `acars` events and compares `alt_ft`
  with the aircraft's ADS-B altitude where both are heard. If it is wrong more than rarely, the
  H1 form drops its altitude and keeps the fix.
- Rounding and the `near`/`within` tolerances in `expect`.

## 7. Calls made in this document

- A row-level `derive` with named extractors, rather than parsing in the ACARS stage (would not
  serve VDL2 and HFDL) or a regex-in-JSON mechanism (cannot do the degree arithmetic, and
  would add a regex engine to core).
- Downlinks only, by a `when` gate on the entry, with a `downlink` field added to the
  in-process ACARS event and the source type mapped on the program rows. The reviewer raised
  the uplink waypoint case; the alternative, accepting the risk and reading a day of events
  for uplink fixes, was rejected because a false fix is the first anti-goal.
- Fill only empty fields: the decoder's structured fix always wins over the text.
- Four forms now, with the slash separator of the TODO's `3L` case folded into the decimal
  form; the `RSI` table later; altitude only where the form states it.
- `into` is required and explicit, so a row never receives an output it did not ask for.
- The ACARS row's own synthesised test carries the proof, so `modes check --strict` covers the
  feature without a new fixture capture.
- Synthetic fixtures at the demo station; nothing from the log is committed.
