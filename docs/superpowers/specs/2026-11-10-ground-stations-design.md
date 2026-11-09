# Plan 35, ground stations: the transmitters a data link's uplinks prove, and a tally of them

Design spec, 2026-11-10. From the operator's request of the same day, after the plan 34 spec
(`docs/superpowers/specs/2026-11-10-acars-text-positions-design.md`): "we should track origins
of ground station traffic too", settled in conversation as two things: light the register
site when a ground station transmits on the channel, and keep a tally of which ground stations
are heard per channel. A reviewer read it as the builder before hand-over; section 7 records
the calls made here. Builds on the origin query (plan 32,
`docs/superpowers/specs/2026-11-06-map-layer-design.md` 4.1-4.6, `crates/zeta-core/src/node/origin.rs`,
the identity tee in `node/launch.rs`), subjects (`subject.merge_skip`, `subjects.js`) and the
map's lit sets (`map.js` `origin`). Depends on plan 34 for the `downlink` field of the
in-process ACARS event and the `src_type` field of the HFDL row.

## 1. Intent and why

On the field host, `origin_lookup` on the VDL2 job finds the register's two licensed data-link
transmitters on the channel at the airport, both the same airline-telecoms licensee, a few
hundred metres apart. Both score about 0.5 and draw as dashed rings: the licence's emission
designator says AM data and the measured modulation (PSK) disagrees, so the modulation term
is zero. Their brightness follows the job's last event, whichever end of the link sent it. A
site is only ever `confirmed` when a decoded callsign equals the register's, which a VDL2
ground station's ICAO address never will.

What the job hears depends on where it listens. The field host is about a hundred kilometres
from the airport: in two days its VDL2 job decoded 278 frames, every one an aircraft's
(`addr_type: Aircraft`; airborne aircraft are line of sight, a 20 W ground station on the
terminal is not), and its seven ACARS jobs heard 105 downlinks and one uplink. Yet every one of
those aircraft frames names the ground station it is talking to (`dst`): two station
addresses account for all 278, and the aircraft are talking to them on this channel, within
VHF range of the receiver. A receiver nearer the airport hears the station's own frames as
well: CRC-clean, on the licensed channel, the strongest evidence the node will ever have that
the register's transmitter is on air, stronger than an emission designator typed into a
licence years ago. The same holds for an ACARS uplink (a block whose `block_id` is a letter,
plan 34's `downlink: false`) and for an HFDL squitter, though HF reach makes the register's
row a weaker guess there.

Two things follow:

1. Ground-station traffic should light the register's transmitter sites on the band as
   `heard`: drawn solid, their brightness following the last such frame, the designator
   mismatch overruled by the decode, and the origin summary saying so for the agent. Two
   grades, told apart everywhere: a station's own frame (`from`), and an aircraft's frame
   addressed to it (`to`), which proves a station of that address serves this channel in the
   receiver's region, not that its transmitter was heard. The node cannot tell two co-sited
   transmitters apart, so the score is not touched and no site is singled out: "a ground
   station is on this channel; the register's two transmitter sites are consistent".
2. A tally of the ground stations per job: each station's key, how many frames it sent and
   how many were addressed to it, first and last seen, kept in `job_context` beside
   `origins`, so it survives a page refresh and the CLI and the agent can read it. The
   Subjects table keeps a per-session row per ground station heard directly; the tally is the
   node's record and the only place the addressed stations appear.

Both are generic. A row says which of its events are a fixed station's, with field = value
pairs as `merge_skip` does today for the same VDL2 ground station, which field names the
station, and which field of any other event names the station it is sent to. Nothing in the
node knows VDL2.

## 2. Goals, non-goals, anti-goals

### Goals

- A row's `subject` can declare `station`: which events are a fixed station's, the field that
  names the station (`null` when the protocol gives none: an ACARS uplink), and the field of
  any other event that names the station it is addressed to (`null` when none).
- The node keeps `job_context.stations` per job: key → `{from, to, first_ns, last_ns}` (frames
  the station sent, frames addressed to it), updated on every matching frame, persisted and
  published at a throttle, never per frame.
- A job's kept origin answer marks every transmitter row (`role: tx`) `heard` with the time
  and grade (`from` or `to`) of the newest station frame, the origin event is republished with
  a summary that says so, and a fresh `origin_lookup` on a job with stations in its tally
  answers `heard` at once.
- The page draws a `heard` site solid whatever its `agree`, lights it from the last station
  frame, and says in the hover whether the station was heard or addressed, how many frames and
  how long ago.
- `vdl2_decode` and `acars_decode` declare their stations (HFDL: non-goals).
- The agent guide tells how to read a `heard` site: the register's transmitter is on air, the
  site is consistent, it is still not an identification of which co-sited transmitter.

### Non-goals

- Placing a ground station on the map by its own identity (a ground-station database keyed by
  ICAO address, dumpvdl2's `--gs-file`). The register's sites stand for it; a station the
  register has nothing for lights nothing.
- Changing a `heard` site's score, or confirming it. `confirmed` stays the identity match of
  plan 32; `heard` is a third, independent mark.
- A per-station event or feed. The tally is context; the frames are already events.
- HFDL's ground stations, this plan. dumphfdl splits a station's frames across two PDU types
  (a squitter is an SPDU with `spdu.src`, an uplink an LPDU with `lpdu.src`), its station ids
  are integers, and the row's `gs_id` today names the addressed station on a downlink and
  nothing on an uplink. The row needs its own pointer work (a `src_type` and `gs_id` that
  cover both PDU types, a key rule that takes a number) before `station` can hold; a TODO gist
  records it. The `station` mechanism itself is protocol-neutral and ready for it.
- Time-bounding `heard`. It is a fact with a time: the page's brightness fades by freshness
  and the hover says how long ago; a fresh `origin_lookup` on a long-running job reports a
  station heard hours ago as heard then.
- Uplink content. What the ground station said stays in the frame's fields and in the Subjects
  table as today.

### Anti-goals

- Flooding. A VDL2 ground station sends several frames a second (squitters, acknowledgements).
  Persisting the job record, republishing the origin event and appending to `events.log` on
  each would be hundreds of writes a minute. The tally is updated in memory per frame and
  written out at the throttle of section 4.3.
- Singling out a site the evidence does not pick: never `confirmed` by a ground-station frame,
  never a score bump, never a marker for the station itself.
- Protocol knowledge in the node: no `addr_type` or `Ground station` string outside the rows.
- Real register rows in tracked files: tests use the synthetic register of `origin_ops.rs`
  (`Example` licensees at the demo station).

## 3. Constraints

- A job's origin answer is kept at `job_context.origins` and updated through
  `Inner::update_origins` (`origin.rs`), which persists the record and republishes the
  `origin` event on the job's event stream (so to the source feed and `events.log`); the
  identity tee (`launch.rs` `tee_identity`, one task per event output, set up at launch for a
  row with `subject.identity`) calls it on every event whose identity field is set. The same
  tee pattern serves stations, and the two rules can share one tee. Every `Origin` in an
  answer is a transmitter row already: `answer` and `score_row` drop the register's receiver
  and plan rows into the `receivers` and `plans` counts, so `heard` goes on every origin.
  `publish_origin` builds the event payload field by field, so a new answer field reaches the
  event only when added there.
- A launch (start, resume, a `graph put` restart, an elastic relaunch) inserts a fresh
  `JobEntry` in `Inner::jobs` (`launch.rs` `record_running`), so runtime state in the entry
  dies with the run; the record's `context` is what survives, and the supervisor persists the
  record (`finish`) before it unregisters the job's streams and before `released`. The tee
  tasks end on the cancellation token first (`biased` select), so a tee never sees the run's
  last frames: an end-of-run flush belongs to the supervisor.
- The page's lit set for a job key takes `activity()` from the `lastEvent` option `app.js`
  hands `mountMap` (today `state.ov.lastEvent(id)`); `app.js` calls `map.origin` for an
  `origin` event and the Origins button, and `map.seed` lights each running job's
  `context.origins` on every jobs poll with the same default activity. The change point is
  that one option. `overlays.js` sees every event of a watched job and knows the job's row.
- `subjects.js` evaluates `merge_skip` against a subject's accumulated `state` with
  `String(have) === String(v)` (`skipsMerge`); an event-level test in `overlays.js` must use
  the same comparison, from one shared helper, or the two disagree on numbers and booleans.
- The UI e2e suite replays no IQ fixture and has no dumpvdl2: VDL2 on the page is the
  `fake_vdl2` fake-lines row in `crates/zeta/tests/ui.rs` (one `Ground station` line, no
  `dst` yet); fake lines are written whole per repeat, so ordered sequences are unit-test
  material in `origin.rs`, not e2e.
- `OriginAnswer` and `Origin` are `deny_unknown_fields` and persisted inside the job record;
  a new field needs `#[serde(default)]` so a context written before this plan still reads, and
  the schema (`require_all` where the type uses it) lists it nullable.
- `style.css` draws `path.map-site[data-agree="disagrees"]` with no fill and
  `[data-confirmed]` with a wide selected-colour stroke; a `heard` mark needs its own
  attribute and rule, and must not be mistaken for `confirmed`.
- Plan 34 adds `downlink` to the in-process ACARS event; this plan's ACARS row edit assumes
  it. Built first, the ACARS row waits; VDL2's `addr_type` and `dst` exist today.
- Repository rules as in plan 34: roles, no observed site names, callsigns or positions in
  tracked files; `scripts/public-check.sh` clean before `main`.

## 4. The design

### 4.1 `subject.station`

```json
"subject": {
  "...": "...",
  "station": { "when": { "addr_type": "Ground station" }, "key": "icao", "addressed": "dst" }
}
```

- `when`: field = value pairs, every one of which must hold (JSON equality) for an event to
  be a station's own frame; the shape and semantics of `merge_skip`. Required, non-empty.
- `key`: the event field whose string value names the station on such a frame; `null` when
  the protocol has none, in which case the station is the anonymous `"*"` (one per job).
- `addressed`: on an event that does not match `when`, the field whose string value names the
  station it is sent to; `null` when the protocol has none. Read only on non-station events,
  since on a station's own frame the same field names the aircraft.
- `null` (the default) for a row with no fixed stations.

Validation: every `when` key, a non-null `key` and a non-null `addressed` must be
`event.schema.properties` keys (`subject.station.when.<k>` / `subject.station.key` /
`subject.station.addressed: not in event.schema.properties`); `when` must not be empty
(`subject.station.when: empty`).

The rows:

- `vdl2_decode`: `{"when": {"addr_type": "Ground station"}, "key": "icao", "addressed":
  "dst"}`. Its `merge_skip` stays as it is (a different question: table merging).
- `acars_decode`: `{"when": {"downlink": false}, "key": null, "addressed": null}` (plan 34's
  field).
- `hfdl_decode`: none this plan (non-goals; a TODO gist).

Matching, on the node and on the page, is one rule: a pair holds when the event has the field
and the two values are equal as JSON on the node, and `String(have) === String(v)` on the page
(the comparison `subjects.js` already uses for `merge_skip`, factored into one helper both
`subjects.js` and `overlays.js` call).

### 4.2 The tally: `job_context.stations`

```json
"stations": {
  "10916A": { "from": 0, "to": 412, "first_ns": 1794106239601503721, "last_ns": 1794279100000000000 }
}
```

Kept in memory in the job's `JobEntry` (`Inner::jobs`), under the jobs lock, as
`stations: Mutex<StationTally>` or a field the tee updates through `Inner` (the plan's
choice): a `BTreeMap<String, StationSeen>` plus `dirty: bool` and `written_ns`. An event
matching the row's `station.when` counts one `from` under the `key` field's value as a string
(a string trimmed, a number as printed; missing or blank: `"*"`); any other event with a
non-blank `addressed` field counts one `to` under that value. `first_ns` and `last_ns` span
both. Written into `job_context.stations` and persisted at the throttle of 4.3, and once at
the run's end. A new `JobEntry` is inserted on every launch, so at launch the tally is seeded
from the record's `context.stations` (empty when none), and the end-of-run flush is the
supervisor's, done where it persists the record before unregistering the streams: a relaunch
(a recovered source, a `graph put`) and a node restart keep the tally, since it is the job's,
not the run's.

### 4.3 `heard` on the kept origins, and the throttle

A new field on `Origin`: `heard: Option<Heard>`, `{last_ns, grade}` with `grade` `from` (a
station's own frame) or `to` (an aircraft's frame addressed to one): the newest `from` frame
in the tally when one exists, else the newest `to` frame (the stronger evidence stays
visible); `null` with an empty tally. Set on every origin of the answer (all are transmitter
rows). A new field on `OriginAnswer`: `stations: BTreeMap<String, StationSeen>` (default
empty), a copy of the tally at the time of writing, added to `publish_origin`'s payload as
`stations`, so the `origin` event and `job_context.origins` both carry the counts the page
shows.

The tally is written (persisted, and the origin event republished when an answer is kept):

- at once, when a station key is seen for the first time in this job, or a known key sends its
  first own frame (its grade rises to `from`);
- otherwise at most once per `STATION_REPUBLISH_MS` (60 000): a frame that leaves the tally
  dirty with no write due yet arms one `tokio::time::sleep` for the remainder of the window
  (one timer per job at most; a Weak `Inner`, ended by the run's token), which writes when it
  fires if the tally is still dirty. So a rare-frame channel is written within a minute of its
  last change, not on the next frame;
- at the run's end by the supervisor, if dirty.

The write with a kept answer sets `heard` and `stations`, persists and republishes with
summary `<k> ground station(s) on <MHz> MHz: <key> heard <n> frames, <key> addressed by <n>
frames; <licensee>, <site> <d> km <agree> <score>; ...`: the keys in descending total, the
first `ORIGIN_SUMMARY_SITES` of them, the anonymous key reading `uplinks heard <n> frames`,
then the sites as today's summary lists them (`ORIGIN_SUMMARY_SITES`); with no origins the
tail is today's phrasing for the case (`no licensed transmitter on <MHz> MHz`, `register gave
no rows`, `no register provider`). With no kept answer (no lookup yet), only
`job_context.stations` is written; nothing is published, since there is no answer to
republish.

A fresh `origin_lookup --job` on a job with a non-empty tally answers with `heard` and
`stations` already set and the summary above, so the agent sees it on its first call.

### 4.4 The tee

`tee_identity` becomes `tee_rules`: one task per event output for a row with `subject.identity`
or `subject.station` (or both), evaluating each event against whichever rules the row has.
Identity behaviour is unchanged. For a station match it updates the tally in the job's entry
and decides, by 4.3, whether to write now or arm the timer. The end-of-run flush is the
supervisor's (section 4.2), never the tee's.

### 4.5 The page

- `overlays.js` records, per watched job whose row has `subject.station`, the
  `performance.now()` of its last station frame (`ov.lastStationEvent(jobId)`, null when none):
  an event matching `station.when` (the shared pairs helper of 4.1) or one with a non-blank
  `addressed` field.
- `app.js` hands `mountMap` `lastEvent: (id) => ov.lastStationEvent(id) ?? ov.lastEvent(id)`:
  every lit job set (an `origin` event, the Origins button, `map.seed` on a jobs poll) then
  follows the ground station on a row with a `station`, and any event on every other row, as
  today. No `map.origin` call changes.
- `map.js`: a site whose best origin has `heard` gets `data-heard="from"|"to"`, paints at
  freshness alone (brightness factor 1 instead of its score: the decode outranks the terms)
  and gets its bearing line whatever its `agree`; `style.css` gives `path.map-site[data-heard]`
  a fill even when `data-agree="disagrees"` and a thin solid ring in the user colour (distinct
  from `[data-confirmed]`'s wide selected-colour stroke); the `to` grade a shade fainter, the
  theme's choice. The hover gains `ground station heard, <n> frames, last <m> min ago` or
  `ground station addressed by <n> frames, ...` from the answer's `stations` (the sums over
  keys, the newest `last_ns`), the popup one line per station key with both counts.
- `subjects.js`: on a row whose `station.key` is non-null, a subject whose state matches
  `station.when` (the shared helper) gets class `station` on its `tr.subject`, so the theme can
  mark it (a small glyph or muted key; the console theme's choice, left to the plan). A row
  with `key: null` (ACARS, where the uplink's `reg` is the aircraft's) marks nothing. No other
  table change.

### 4.6 CLI, agent and docs

- `zeta job context --job <id>` shows `stations` as part of the context; nothing new to add.
- `agents_md.rs`, `origin_lookup` prose: `heard` and `stations`, how to read them ("the
  register's transmitter is on air on this channel; the site is consistent; which co-sited
  transmitter is not known"), and that a `heard` site is still not `confirmed`.
- The VDL2 and ACARS row `notes` say the ground station's frames light the register's sites.
- `docs/guide.md`, the map paragraph: one sentence on `heard` sites.
- `docs/agents/field-craft.md`, the register bullet: one sentence.
- `UAT.md`: a plan 35 section (5.5).
- `TODO.md`: a gist "Modes: HFDL ground stations as origins" with the pointer work of the
  non-goal.

## 5. Testing

### 5.1 Row schema (`row.rs`)

`station` round-trips, defaults null, a bad `when` key or `key` names its field, an empty
`when` is refused (`subject.station.when: empty`), `key: null` is allowed.

### 5.2 Origin (`origin.rs` unit tests, `crates/zeta/tests/origin_identity.rs` style)

- The throttle: a first key writes at once; a second frame of the same key within the window
  does not but arms the timer; a new key writes at once; a known key's first own frame (grade
  `to` → `from`) does; the timer's firing writes when dirty and not otherwise; the run's end
  flushes (through the supervisor, with the tee already cancelled).
- The tally is seeded from `context.stations` at launch, so a relaunch continues the counts.
- `heard` is set on every origin, with the grade (`from` over a newer `to`), `stations`
  copied with both counts into the answer and the published event's payload; the summaries of
  4.3 for one key heard, two keys addressed, the anonymous key, and an empty `origins` with
  each of today's three tails.
- A station's own frame counts `from` under `key`; another event counts `to` under
  `addressed`; the `addressed` field of a station's own frame is ignored; a numeric key is
  counted as its printed form.
- A context persisted without `heard`/`stations` (a pre-plan record) still deserialises.
- A fresh `origin_lookup` on a job with a tally answers `heard` and `stations`.
- End to end with a fake decoder row (the test registries already build rows): ground-station
  events on a job with the synthetic register → `job_context.stations`, one republished
  `origin` event with the summary, no further event within the window.

### 5.3 Page

The `fake_vdl2` row in `crates/zeta/tests/ui.rs` gains `dst` in its schema and lines (the
aircraft lines addressed to the ground station `1A2B3C`) and the `station` block of 4.1; the
e2e suite's origin scenario (or a short new one on that row with the synthetic register)
asserts `data-heard` on the lit site (`from`, since the fake lines include the station's own
frame), the hover line, and `subjects.js`'s `station` class on the ground-station row and on
no aircraft row. The `to`-only grade and ordered sequences are unit tests in `origin.rs`.

### 5.4 Agents and docs

`cargo run -q -p zeta -- agents-md --check` and `--skill --check` after regeneration;
`check-doc-links.sh`.

### 5.5 Live (UAT, the field host)

With the VDL2 job running and a lookup done: within a minute, `job_context.stations` lists the
channel's two ground stations with growing `to` counts (the field host hears the aircraft, not
the stations: `from` stays 0 there); the map shows the airport's two sites solid, graded
`to`, bright after each aircraft frame; the origin event in `events.log` is republished about
once a minute, not per frame; the hover says `addressed by`. From a vantage within reach of
the airport (a portable receiver, or a Kiwi near it), the same job shows `from` counts, the
hover `heard`, and the Subjects table marks the ground-station row. With the seven ACARS jobs
(after plan 34): `stations["*"]` counts the rare uplinks per channel, and the register's sites
on those channels light when one arrives.

## 6. Left to the plan

- The exact shape of the tally in `JobEntry` and how the supervisor's flush is sequenced with
  the record's final persist (one persist, not two, where it can).
- The `StationSeen` and `Heard` types' home (`origin.rs`).
- The theme's mark for a station row and the exact hover wording.
- Whether `origin_lookup` by band (the read-only form) ever reports `heard` (no: it has no
  job and no tally).

## 7. Calls made in this document

- Evidence as a third mark (`heard`) beside `agree` and `confirmed`, not a score change: the
  decode proves a transmitter on the channel, not which site.
- Two grades of `heard`, `from` and `to`, after the field host's log showed that a receiver a
  hundred kilometres from the airport hears the aircraft and never the station: without the
  addressed grade neither the lit site nor the tally would ever show anything there.
- The tally in `job_context.stations`, independent of whether a lookup has run, with a copy in
  the origin answer for the page; never a per-frame event.
- A 60 s throttle with an immediate write on a new station, a per-job timer so a rare channel
  is written within the window, and a flush by the supervisor at the run's end (the reviewer
  showed the tee is cancelled before the last frames and the job entry is replaced on each
  launch, so the tally is seeded from the record at launch).
- Brightness from the page's own clock on the last station frame, not from node republishes;
  a heard site paints at freshness alone, since the score's terms are weaker evidence than the
  decode, and the score itself is untouched.
- One `tee_rules` for identity and station rather than two tees per output.
- An anonymous `"*"` station for protocols whose uplinks carry no station identity (ACARS),
  and no Subjects mark on such a row, since its uplink's `reg` is the aircraft's.
- HFDL deferred to a TODO gist: its station frames need pointer work this plan should not
  carry.
- `heard` not time-bounded; the page fades it and dates it.
