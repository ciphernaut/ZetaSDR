# Plan 32, the map layer: licensed sites as the likely origins of signals

Design spec, 2026-11-06. From the operator's brainstorm of the same day, kept apart from the
digital-mode sprints (`2026-11-06-decoder-wave2-design.md`, `2026-11-06-aircraft-links-design.md`,
`2026-11-06-span-decoding-and-sets-design.md`) at the operator's request; the wave 2 spec lists
"the map tab" as out of its scope and this document is that tab. Builds on the map pane
(`crates/zeta-ui/web/map.js`, plan 17 spec 7.2), the pane strip (`panes.js`, plan 19 spec 5.7),
the view policies (`view.js`, spec 4.3), subjects (`2026-11-02-subjects-design.md`), themes
(`2026-11-03-themes-design.md`), the knowledge providers (`crates/zeta-core/src/knowledge.rs`,
the `acma` provider in `crates/zeta-ext/src/acma.rs`) and the register scoring in
`crates/zeta-core/src/modes/score.rs`. A reviewer read it as the builder before hand-over; its
questions are folded in, settled in section 10 or passed to the operator in section 9.

The decisions in section 1.1 are the operator's. The shape in the rest of the document is the
author's, which the operator saw and did not object to; each call is listed in section 10 and is
open to correction.

## 1. Intent and why

A node with the register attached (`--knowledge-acma`) already knows, for any band, who is
licensed to transmit there, from which site, how far away and with which emission designator.
Today that knowledge reaches the operator as text: `knowledge_lookup` by hand, the watch's
`unknown` summary (`register says 16K0F1D ... Example Hill, 17 km`), one transcript line after
`classify` or What is this on a capture, and the nearest licensee's name in the Explore table. The
map, meanwhile, draws only what decoders say about themselves: sonde and aircraft fixes. The two
never meet, so "what is that carrier at 403.025 and where is it coming from" takes a lookup, a
read of a note line and a guess at the geography.

The map layer joins them. When a signal is measured (by identify, a watch, a capture, a classify,
or on demand from any running job or bits chain), the node asks the register what is licensed on
that band, scores each transmitter against the measured modulation, and publishes the answer as
one record. The page lights the sites on the map, brightest where the designator agrees with what
was heard, with a bearing from the station, and fades them as the signal goes quiet. A click on a
site offers the decoder that fits. The agent and MCP see the same record as an `origin` event and
an `origin_lookup` op, so "where is that coming from" is answerable in every transport.

Two layout changes come with it, both the operator's. The map stops being a toggle that halves
the waterfall and becomes a tab of the pane strip beside Subjects, Explore and Bits (the rule
from 2026-11-02: a new view earns a tab, it never takes a slice of the waterfall). And the pane
host, which today only sits under the waterfall, can be docked to its right, so the Subjects
table (or the map) stands beside the spectrum as a column instead of cutting its height.

### 1.1 The operator's decisions

1. A separate spec for the map layer, apart from the digital-mode sprints.
2. Likely origins light up on the map when a licensed frequency matches a signal's measured
   modulation type and an entry in the ACMA register (the `--knowledge-acma` provider).
3. The map becomes a whole new tab of the page's pane strip, beside Subjects, Explore and Bits,
   replacing today's map toggle over the waterfall.
4. The Subjects pane becomes relocatable to the right-hand side of the main view as well as
   underneath it.
5. The origin query may start as a query against a new signal: identify, watch and capture
   already consult the register. The operator was unsure whether existing jobs and bits chains
   have a register lookup. Finding (section 3): they do not.
6. Decoded id to map where practical: when a decoder or bits chain yields an identity (a
   callsign, an aircraft registration, an MMSI; a pager address is not one) and the register
   carries a field it can be matched against, the match confirms (brightens) or demotes a lit
   site. Where the register has no such field, the identity is shown on the map as its own
   subject only, as ADS-B and sondes are today, and does not touch the sites.

## 2. Goals, non-goals, anti-goals

### Goals

- One origin query on the node, "what is licensed on this band, and does the designator agree
  with layer 1", with one result record whoever asks: identify, the watch and the capture ops
  automatically (as they consult the register now), and on demand for any job, bits chain,
  selection or band through `origin_lookup`.
- The page lights each candidate site on the map, brightness by score, with a bearing line from
  the home; hover gives licensee, site, designator and score; a dim ring marks an assignment on
  the frequency whose designator disagrees with the measured modulation; lit sites fade as the
  signal goes quiet.
- A click on a lit site shows the assignment and offers the fitting decoder on the measured band,
  through the page's normal `applyOp` path with its conflict buttons. Nothing is stopped or
  replaced by the page or the agent on its own.
- The Map is a tab of the pane strip (`view set pane map`), the map toggle and `data-map` go.
- The pane host docks under the waterfall or to its right (`view set dock bottom|right`), a view
  policy like the others, with the theme's `layout.dock` as its default.
- The agent, MCP and the CLI see origins: an `origin` event on the job's stream (so the source's
  feed carries it), `origin_lookup`, and the last answer kept in `job_context.origins`.
- A decoded identity confirms a lit site when the register carries a field it can be matched
  against (section 4.6); otherwise it stays a subject.
- Honest degradation: no station (no bearings, sites still lit), no register (the tab shows
  subjects only), a remote source (no bearings, no reach cut).

### Non-goals

- Direction finding, triangulation or any position estimate from the signal itself. A lit site
  is the register's claim about who may transmit there, scored for plausibility; it is never a
  measurement of where the signal came from. The record says so (`agree`, `score`) and the field
  craft repeats "consistent with", never "is".
- A node-side store of subjects (TODO "Subjects: a running job keeps its tables on the node").
  `job_context.origins` is one small step in that direction and nothing more.
- Per-source positions for KiwiSDR and other remote sources. This spec defines what happens
  without one (section 4.8); the source record gaining `lat`/`lon` is a later plan.
- Fuzzy matching of decoded names against licensee names (an RDS `ps` of `4KQ` against a
  licensee's trading name). Only exact callsign matches are built (section 4.6).
- A register other than the ACMA one. The design is against `Allocation`, so another provider
  that fills the same fields gets the map for free, but none is written here.
- Resizable panes (a drag handle on the dock). The right dock's width is a theme token.
- Two panes on screen at once (the map on the right and Subjects underneath). The strip stays one
  pane at a time; see question 1 to the operator.
- Offline tiles, a second tile provider, or caching tiles on the node.

### Anti-goals

- Drawing a register row as a fact about the signal. A site is lit by a score the record carries;
  a confirmed one says what confirmed it. No "transmitter located" wording anywhere.
- The page computing scores or reading the register itself. The node is the one place that
  scores; the page draws what the record says, so the agent, MCP and the page never disagree.
- Starting, stopping or replacing jobs from a map click without the operator's click on the
  conflict button. The decoder offer is a `job_start`; a conflict gets the usual retry buttons.
- Real register extracts in the repository. Fixtures are synthetic rows around the demo station
  (section 6); tests never embed a real licensee, licence number or site.
- A tile fetch the operator did not cause: tiles load only while the map is on screen, and never
  in bulk.

## 3. Constraints and findings

- **Who consults the register today.** `knowledge_lookup` by hand; `classify` and
  `capture_classify` (`OpResult::Classification.allocations`, from `nearest_allocations`);
  `identify` once per run (`identify_loop`: `nearest_allocations`, then `allocation_term` in the
  scores, the best fit as `allocation` on each `candidate` event, the whole list on the
  `verdict`); the watch's `unknown` event (the identify verdict's `allocations`, worded by
  `register_says`); a `capture` job's record keeps `allocations` (`bursts.rs`, SigMF
  `zeta:allocations`). Decoder jobs, demodulators and bits chains never consult it. The page shows
  the nearest transmitter as a transcript line (`units.js registerText`, used by `menu.js` after
  classify and `explore.js` after capture_classify) and as the licensee column in Explore; it draws
  nothing on the map from an allocation.
- **The register's fields on the node side.** `acma.rs` reads `FREQUENCY`, `BANDWIDTH`, `EMISSION`,
  `DEVICE_TYPE`, `TRANSMITTER_POWER(_UNIT)`, `LICENCE_NO`, `SITE_NAME`, `LATITUDE`, `LONGITUDE` and
  `client.LICENCEE` into `Allocation` (`licensee`, `site`, `lat`, `lon`, `distance_km`,
  `bearing_deg`, `power_w`, `center_hz`, `bandwidth_hz`, `licence_no`, `role`, `emission`,
  `emission_decoded`, `as_of` always `null`). None of these is an identity a decoder yields. The
  register's `device_details` table has a `CALL_SIGN` column (to be confirmed against the register
  server; it is in the ACMA RRL extract the server loads) and `client` has `TRADING_NAME` (to be
  confirmed). Aircraft registrations, ICAO addresses and MMSIs are not register fields: aircraft
  and ship stations are class licences in Australia. Amateur stations have been under a class
  licence since 2024, so an APRS callsign is unlikely to match a register row (repeaters and
  beacons licensed as apparatus still carry callsigns; to be confirmed). So decision 6 reduces to
  one match today, decoded callsign against `CALL_SIGN`, and most identities stay subjects.
- **The provider's shape.** `AcmaKnowledge::lookup` asks `get_frequency_allocation` for the plan
  and one `execute_sql` for the assignments, cut to the band's reach around the station (a
  bounding box from `reach_for`), at most `ACMA_MAX_ROWS` (50) nearest first; without a station
  there is no box and rows sort by frequency offset. `ACMA_TIMEOUT` is 5 s for a whole lookup.
  `nearest_allocations` keeps `NEAREST_ASSIGNMENTS` (5) for identify and classify; the map wants
  more.
- **Scoring.** `score::allocation_match` is the fit test (a `tx` assignment whose width is within
  0.5-1.5 of the row's and whose designator letter is compatible with one of the row's labels,
  `DESIGNATOR_LABELS`); `ALLOCATION_SCORE` is 0.2. This spec grades the same tests rather than
  inventing new ones.
- **Allocation is strict.** `#[serde(deny_unknown_fields)]`, every field written and required in
  the schema (`require_all`); a new field is `#[serde(default)]` so stored capture records load.
- **The page's layout.** `#main` holds `#waterfall` (absolute, inset 0), `#map` (absolute, the
  right half while `html[data-map="on"]`) and `#pane` (absolute, bottom 40%, shown while a pane is
  selected; `html[data-strip]` lifts `#waterfall` and `#map` by 40%). In the narrow footprint a
  pane or the map takes the waterfall's place (`data-pane`). `#map-toggle` shows only narrow with
  `data-map` on. `panes.js` registers panes with `eligible`, `el`, `onShow`, `onHide`; one pane at a
  time; the selection is the `pane` policy; `subjects on|off` is an alias of it.
- **View policies.** `POLICIES` in `view.js` is the registry; `AGENT_VIEW_KEYS` (page) and
  `VIEW_KEYS` (`zeta-agent/src/page.rs`) list what the agent's `view_set` may set; both carry `map`
  today. Values live in `sessionStorage`.
- **Themes.** `theme.json`'s `layout` is `{rail, ribbon_tilt_deg}` with `deny_unknown_fields`;
  `footprints` is reserved and refused. The page reads `layout.rail` into `data-rail`. There is no
  mechanism yet for a theme to default a view policy; `plan auto` and `scanner auto` are the
  precedent for a policy whose default is resolved elsewhere.
- **The map.** `map.js` loads Leaflet lazily on first show, draws tracks per job and per
  `(job, key)` subject, a home marker from `homeOf` (the view's `home`, else the station read once
  at load), and centres once. `map.sync()` drops the tracks of jobs no longer running. Tiles are
  OSM's; the CSP's `img-src` names `https://tile.openstreetmap.org` and nothing else; the e2e
  aborts tile requests and the map must still draw.
- **Events.** A job's events go on its own stream and on the source's feed (plan 27); the page
  reads one feed per shown source plus the positioned jobs' streams on other sources
  (`MAX_POSITIONED`). The feed carries every event frame of every job on the source, so an
  `origin` event published on a job's stream reaches the page, `stream_tail`, `events.log` and
  `zeta events tail` without new plumbing. A hand lookup by band has no job and so no stream.
- **Sources.** `Capabilities` has `remote: bool` and no position. A KiwiSDR is remote; the station
  is where the node is.
- **The e2e.** `crates/zeta/tests/ui.rs` spawns `zeta serve` with file sources and a `--modes-dir`
  of fake rows (`fake_pos`, `fake_subjects`, run by `zeta-ext`'s `fake_lines`); no knowledge
  provider is attached. `crates/zeta-ext/tests/acma.rs` has an in-process axum stub of the register
  (SSE MCP answers for `get_frequency_allocation` and `execute_sql`). The suite's deadline is
  690 s and it runs about 474 s; new scenarios have a budget of about a minute together.
- **Repository rules.** Roles not host names; the demo station is Brisbane Airport (-27.3842,
  153.1175, 4 m); `AGENTS.md` and the skill are generated. The register is public data published
  by the ACMA; the page may show licensee names as the register publishes them.

## 4. The design

### 4.1 The origin query on the node

A new module `crates/zeta-core/src/node/origin.rs` with one entry point:

```rust
pub async fn origins(node: &Inner, q: &OriginQuery) -> OriginAnswer
```

`OriginQuery` is the signal as measured or declared: `source` (the band's source), `center_hz`,
`bandwidth_hz`, `modulation: Vec<String>` (layer-1 labels; empty when unknown),
`symbol_rate_baud: Option<Baud>`, `job: Option<JobId>`, and `at: Timestamp`.

It asks every knowledge provider for the band (region: the station's, else `DEFAULT_REGION`),
exactly as `knowledge_lookup` does and with the provider's own reach cut, keeps the `assignment`
rows, and scores each `tx` row (a receiver is never an origin; receivers and plan rows are counted
in the answer, not scored). Rows are deduplicated by `(licence_no, site, center_hz)` and capped at
`ORIGIN_SITES = 50`, best score first, then nearest. The cap is separate from
`NEAREST_ASSIGNMENTS` (5), which identify and classify keep for their candidate lists.

**The station used.** For a source whose `Capabilities.remote` is false, the station: the
provider's distances, bearings and reach cut apply. For a remote source (a KiwiSDR, another node),
the query is made with no station (`StationFn` answers `None` for that call), so no bounding box,
no distances, no bearings, no reach cut; the answer's `from` is `null` and says why
(`"remote source"`). The provider's interface gains nothing for this: the station function is
already a closure, and the origin module calls `allocations()` through a wrapper that withholds
the station. Section 4.8 says what the page does with it.

**The score.** Three graded terms, the same tests as `allocation_match`, summing to at most 1:

| Term | Weight | Rule |
|---|---|---|
| frequency | gate | the row's centre and the band overlap: `abs(row.center_hz - center_hz) <= row_width/2 + bandwidth_hz/2`, `row_width` the decoded designator's bandwidth, else the row's `bandwidth_hz`, else 0. A row that fails is not a candidate. |
| width | 0.4 | `r = row_width / bandwidth_hz`: 0.4 for 0.5 <= r <= 1.5; 0.2 for 0.25 <= r <= 4; 0 outside; 0.1 when the row has no width. |
| modulation | 0.4 | the designator's modulation letter compatible with one of `modulation` (`DESIGNATOR_LABELS`): 0.4, `agree: fits`; incompatible: 0, `agree: disagrees`; no designator or `modulation` empty: 0.2, `agree: unknown`. |
| reach | 0.2 | `distance_km` known and within the reach toward the site: 0.2; no distance (no station, remote source): 0.1; known and out of reach (only a static provider's rows, the register's are cut already): 0. |

An identity confirmation (4.6) sets `score` to 1 and `confirmed` to the identity; the terms stay
as computed so the reader sees both.

**The record.** One `Origin` per lit site:

```
Origin {
  allocation: Allocation,        // the row as knowledge_lookup gives it (with `callsign`, 4.6)
  score: f32,                    // 0..1
  agree: "fits" | "disagrees" | "unknown",
  terms: { width: f32, modulation: f32, reach: f32 },
  confirmed: Option<{ field: String, value: String }>,   // 4.6
}
OriginAnswer {
  origins: Vec<Origin>,
  band: { center_hz, bandwidth_hz },
  modulation: Vec<String>, symbol_rate_baud: Option<Baud>,
  from: Option<{ lat, lon, kind: "station" }>,           // null without a station or for a remote source
  from_note: Option<String>,                              // "no station set", "remote source"
  providers: Vec<String>,                                 // the providers asked ("acma", ...)
  receivers: u32, plans: u32,                             // rows seen and not scored
  reach: Option<Reach>,
  t_ns: Timestamp,
}
```

`Allocation` gains `callsign: Option<String>` (`#[serde(default)]`, in the schema like the others),
which the `acma` provider fills from `device_details.CALL_SIGN` (one more column in the fixed SQL;
to be confirmed against the register server, and `null` when the column is absent). Nothing else
about `Allocation` changes.

### 4.2 Who runs it, and what they publish

**Automatic, as now.** Each place that consults the register keeps its `allocations` as it is
(the agent prose and the watch's `register_says` are unchanged) and adds the origin answer:

- `identify`: after its one lookup, with the measured layer-1 label, it publishes one `origin`
  event (4.3) on its stream and puts the answer in its `verdict` as `origins` beside
  `allocations`. The watch's `unknown` event carries `origins` beside `allocations` (it already
  copies the verdict's). `candidate.allocation` stays the best fit by `allocation_match`.
- `classify` and `capture_classify`: `Classification` gains `origins` (the classifiers measured
  the label, so the node scores before answering). A page that runs What is this gets lit sites
  with no second op. The capture record keeps `allocations` only; `capture_classify` recomputes.
- The watch's `signal` events are not scored: a `signal` has no label until identify runs. The
  watch's `unknown` is where the register meets the measurement, as today.

**On demand.** A new op:

```
origin_lookup { job: JobId | null, source: SourceId | null, center_hz: Hz | null,
                bandwidth_hz: Hz | null, modulation: [label] | null }
```

Either `job` or the band (`source`, `center_hz`, `bandwidth_hz`) is given, never both.

- `job`: the band is the job's `tuning` (an attached job's is its input's); `modulation`, when
  `null`, is read from the job: a decoder row's `modulation` labels (a running decoder with
  CRC-clean frames is the strongest statement of its modulation there is), a demodulator's kind
  (`fm_demod`, `nbfm` → `fm`; `am_demod` → `am`; `ssb_demod` → `ssb`), a `bits` job's chain
  demodulator (`fsk_demod` → `fsk`, `psk_demod` → `psk`, OOK → `ook`), else empty (`unknown`).
  A `bits` job runs on source `node` from a capture: its band is the capture record's and its
  source for the station rule is the capture's source. A job with no band (`mix`, `spectrum`, a
  `scan`) is refused `invalid` naming the kind. The answer is returned, published as one `origin`
  event on the job's event stream, and kept as `job_context.origins` (the last answer only; the
  page seeds from it on load, and an agent reads it without a stream).
- the band: returned only; no job, no stream, no context. `modulation` given is used as is;
  `null` reads as unknown. The op does not measure: to score against a measurement, run
  `classify` on the selection, which returns `origins` itself.

The band form is `Op::read_only` (left out of `ops.log`, like `knowledge_lookup`); the job form
is not, since it writes the job's context and publishes an event, so it is logged and, from a
one-shot CLI while `zeta serve` holds the directory, goes through `--node` like any job op. The
agent needs no allowance for either: it starts, stops and replaces nothing. CLI:
`zeta origin lookup --job ID | --source S --center-hz N --bandwidth-hz N [--modulation fm,fsk]`.
MCP gets it from the schema as every op.

### 4.3 The `origin` event

Protocol `origin`, on the job's event stream (and so on its source's feed), one per answer:

```
{ "protocol": "origin", "job": "<id>", "band": {...}, "modulation": [...],
  "from": {...} | null, "from_note": ..., "origins": [...], "providers": [...],
  "receivers": n, "plans": n, "reach": {...} | null,
  "summary": "3 sites on 403.025 MHz: Example Water Authority, Example Hill 17 km fits 0.9; ...",
  "t_ns": ... }
```

The summary names at most three sites, best first, as `<licensee>, <site> <d> km <agree> <score>`
(`<d> km` dropped without a distance), or `no licensed transmitter on <MHz>` when `origins` is
empty, or `no register provider` when `providers` holds none that gives assignments. It is a
transcript line like the watch's (`<kind> <id tail>: <summary>`), so the operator sees it on the
page and in `events.log` whether or not the Map tab is open.

### 4.4 The Map tab

`#map` moves inside `#pane` beside `#subjects`, `#explore` and `#bits`. `map.js` registers the
`map` pane with the strip (`panes.register("map", "Map", {eligible, el, onShow, onHide})`):
always eligible (the tab is there whether or not anything is lit; question 6 to the operator),
Leaflet loaded on first `onShow` as today, `invalidateSize` on each show, nothing while hidden.
`#map-toggle`, `data-map`, `applyMapPane` and the narrow footprint's map rules in `style.css` go;
the narrow footprint shows the map as it shows any pane (in the waterfall's place while its tab
is selected).

The `map` view policy becomes an alias of `pane` as `subjects` is: `on` sets `pane map`, `off`
sets `pane none` when the map is the pane shown, `auto` (the new default) changes nothing. Stored
`map on` values keep working, the agent's `view_set map on` keeps working, and `view set pane map`
is the direct form. The guide's table and the field craft's `view set map on|off` sentence are
updated to say so.

What the map draws, beyond today's tracks, home and subjects:

- **Sites.** One `L.circleMarker` per lit site (class `map-site`, `data-agree` fits|disagrees|
  unknown, a `--site-brightness` custom property 0..1 the CSS maps to fill opacity), at
  `allocation.lat/lon`. A site that `disagrees` is a dashed ring with no fill (`map-site` with
  `data-agree="disagrees"`). Two assignments at one position (one site, several devices) are one
  marker whose popup lists them; the marker takes the best score.
- **Bearings.** One non-interactive `L.polyline` (class `map-bearing`, the same brightness) from
  the home (`homeOf`) to each lit site that `fits` or is `unknown`, when `from` is set and the
  site has a position. None for `disagrees`, none without a home, none for a remote source.
- **Hover.** A Leaflet tooltip: `<licensee>` / `<site>` / `<emission> (<words>)` /
  `<score> <agree>` / `<d> km, <bearing>°` (the last line only with a distance), plus
  `confirmed by <value>` when confirmed.
- **Click.** The popup: the tooltip's lines, `licence <no>`, `<power> W`, the measured band and
  labels the lookup used, and the actions of 4.5. Unlike a subject marker, a click selects nothing
  in the Subjects store.
- **Brightness and fade.** `brightness = score × freshness`, `freshness = max(0, 1 − age /
  origin_fade_s)`, `age` the seconds since the lit set's signal was last active. For a job-bound
  set, active means any event frame of the job on the feed (a decoder's frames, a demodulator's
  squelch opening as the page already tracks it for the dot) or a fresh `origin` event; for a
  one-shot set (a classify, a capture_classify, a band lookup) the lookup time, so it fades from
  the moment it lit. At zero the site goes. The clock is the page's, checked each second with the
  subjects' tick. `origin_fade_s` is a new view policy (a positive integer of seconds, default
  300, the subjects' drop time), in `AGENT_VIEW_KEYS`/`VIEW_KEYS`.
- **Keys.** Lit sets are keyed by job id (one set per job; a new `origin` event replaces the
  job's set) or by a one-shot key (`classify:<t_ns>`, `capture:<id>`, `band:<t_ns>`); a job's set
  goes with the job (`map.sync`, as tracks do). Sets are kept while the tab is hidden, so what lit
  during a classify is there when the tab opens, within the fade.
- **Seeding.** On the first `onShow` and after each jobs poll, the page reads `job_context.origins`
  of running jobs whose context names it (the same poll that seeds squelch gates), so a reload or
  a second page shows what the node last answered, aged from the record's `t_ns`.
- **Centre.** Unchanged: once, on the first fix or the home. A lit site never moves the view; a
  click on a popup action never does either.
- **Feed cost.** None new: `origin` events ride the source's feed the page already reads. A
  one-shot lookup is one op.

### 4.5 Actions on a lit site

The popup's buttons, each a `button[data-action]`, each through app.js's `applyOp` (conflicts get
the usual `stop <kind> and retry` buttons, never an automatic `replace`; a refusal is a transcript
line):

- **Decode with `<mode>`** for each mode row (at most three, best first) that scores against the
  assignment as `score_rows` would against a measured band: the row's labels compatible with the
  designator letter, its `band.bandwidth_hz` within 0.5-1.5 of the row width, the row `ready`.
  The op is `job_start {kind: <mode>, source: <the lookup's source>, center_hz, bandwidth_hz}` on
  the **measured** band the lit set came from, not the licensed centre (the no-grid rule: a
  demodulator is centred on the signal as heard). For a one-shot band lookup with no measurement,
  the licensed centre and the row's width are what there is, and the button's title says so.
- **Listen** when the designator's information symbol is `E` (telephony): `fm_demod` for `F`/`G`,
  `am_demod` for `A`, `ssb_demod` for `H`/`J`/`R`, on the measured band; the row plays as any audio
  job does.
- **Look it up** puts the register row as the agent would see it (`knowledge_lookup`'s JSON for
  that site) in the transcript as a collapsed `details`.

The register's `licence_no` is shown; no link out to the register's website is made (the page
loads nothing from outside its origin but tiles).

### 4.6 Decoded identities

A mode row may declare how its subject can be matched against the register:

```json
"subject": { "key": "src", ..., "identity": { "field": "src", "register": "callsign",
             "strip": "ssid" } }
```

`register` names an `Allocation` field; the only value this spec defines is `callsign`.
`strip: ssid` removes an AX.25 `-N` suffix before comparing; comparison is case-insensitive,
trimmed. Rows without `identity` keep today's behaviour exactly: their subjects go on the map as
subjects and never touch a site. Of the built-in rows, `aprs_decode` gets `identity` (`src`,
`callsign`, `strip: ssid`); `adsb_decode` (`icao`), `acars_decode` (`reg`), the sonde rows (`id`)
and `rds_decode` (`pi`) get none, since the register has no field for them (section 3). The AIS
row of wave 2 gets none either (MMSI). The wave 2 spec's rows are untouched by this one; the
field is optional and `null` by default, so it adds nothing to their load.

The match runs on the node, in the same place the node will one day keep subject tables: when a
job with an `identity` row and an `origins` context sees an event whose `field` value differs from
the last one matched, the node compares it with the `callsign` of each origin in its context. A
match sets that origin's `confirmed` and `score` to 1 and republishes the `origin` event with
`summary` `confirmed <value>: <licensee>, <site>`. A mismatch changes nothing: APRS, CB and
repeater channels carry many stations, so a decoded callsign that is not the licensee's proves
only that someone else also transmits there. (The operator asked for confirm or demote; the author
builds confirm only and asks in question 2 whether to demote for one-transmitter services such as
broadcast.) The page draws a confirmed site with a ring in the selection colour (`map-site` with
`data-confirmed`), and the tooltip's `confirmed by <value>` line.

### 4.7 The dock

A new view policy `dock`: `auto | bottom | right`, default `auto`. `auto` resolves to the theme's
`layout.dock` when its `theme.json` has one, else `bottom`. It positions the pane host `#pane`,
whichever pane is shown (Subjects as the operator asked, and equally Map, Explore and Bits: one
mechanism rather than a per-pane variant):

- `bottom`: as today, `#pane` the bottom 40% of `#main`, `#waterfall` lifted by 40% while a pane
  is selected.
- `right`: `#pane` a full-height column on the right of `#main`, `var(--pane-dock-w)` wide (the
  core default 40%; a theme token), `#waterfall` narrowed by the same while a pane is selected,
  its height untouched. The Subjects table in a tall column shows more rows and fewer columns at
  once (it scrolls sideways as today); the map in a tall column is a portrait map, which suits
  bearings from a home at the centre.

The page sets `<html data-dock="bottom|right">` from the resolved value (so `style.css` has one
selector per dock, as for `data-strip`) and re-resolves it on a theme change (`applyTheme`) and a
`dock` change. The narrow footprint ignores the dock: panes take the waterfall's place there, and
`data-dock` is still set so a theme may read it. `dock` joins `AGENT_VIEW_KEYS` and `VIEW_KEYS`
(presentation only). `theme.json` gains `layout.dock` as an optional field (`bottom` when absent),
refused with the field's name when it is neither word; the built-in themes do not set it; the
loader's `footprints` reservation stands, and when footprints arrive they override `layout.dock`
like the rest of `layout`. `docs/themes.md`'s table gets the row and `html[data-dock]` joins the
list of root attributes a theme may style against.

### 4.8 Degraded cases

- **No station.** `from` is `null`, `from_note` `"no station set"`. Sites are lit by frequency,
  width and designator; the reach term scores its unknown value; no bearings, no distances, the
  tooltip has no distance line. The register's query has no bounding box, so the rows are the 50
  nearest in frequency anywhere in the region: the map tab shows a note under the strip,
  `no station set: sites are not cut by reach (station_set)`, once per load. The map centres on
  the first lit site when there is no home and no fix.
- **No register provider.** Every answer has `origins: []` and a `providers` list without an
  assignment provider; the summary says `no register provider`. The tab shows subjects, tracks and
  the home as today; the Origins buttons (4.9) say the same in the transcript and light nothing.
  An attached but unreachable service is the same from the page's point of view (`providers`
  names `acma`, `origins` is empty, the node logs the failure as it does now) and the summary says
  `register gave no rows`; the distinction is in `providers`.
- **A remote source** (a KiwiSDR on HF, another node's source). The query is made without the
  station (4.1): `from` is `null`, `from_note` `"remote source"`, no bearings, no reach cut. HF
  rows are many (a frequency may be licensed to dozens of sites across the country), so the cap of
  50 by score then frequency offset matters here. The map draws the sites; nothing says where the
  receiver is. When a later plan gives sources a position, `from` becomes
  `{lat, lon, kind: "source"}` and bearings return, with no change to the record's shape.
- **A signal with no label.** `modulation` empty scores every designator `unknown` (0.2): sites
  light at middling brightness with `agree: unknown`, and the tooltip says `modulation not
  measured`. A classify on the band replaces the set with a scored one.
- **Tiles offline.** As today: Leaflet draws markers, lines and attribution over blank squares.

### 4.9 Where the page asks

- **The job row** (agent.js) gets an `Origins` button beside `graph` for any running job with a
  band: `origin_lookup {job}`; the answer's summary is the transcript line (the `origin` event
  provides it), the sites light, the Map tab is selected when the view's `pane` is `none`
  (a pane the operator chose stays; TODO "UI: more actions on an existing job" is the home of the
  shared entry table this button will join).
- **The Bits pane** gets an `Origins` button in its header, for the shown bits job.
- **The box menu**: What is this (`classify`) lights sites from its `origins` with no new entry;
  the menu gains `Origins` as a cheaper entry (`origin_lookup` by band, `modulation` from the
  waterfall's layer label when one is on the box, else null).
- **Explore**: What is this on a capture lights sites from `capture_classify`'s `origins`.
- **The agent**: `origin_lookup` as a tool, `origin` events through `stream_tail` of the feed, and
  `job_context.origins`. The field craft's register paragraph gains two sentences: ask
  `origin_lookup` for a running job before guessing who transmits; a confirmed origin is still a
  register match, say "the register's <licensee> at <site> is consistent with" and never "it is".

### 4.10 Privacy, tiles and the public repository

- The register is public data the ACMA publishes; licensee names, site names and licence numbers
  are shown as the register gives them, as `knowledge_lookup` and the watch's summary already do.
  Nothing is added that the register does not publish (no reverse geocoding, no linking out).
- Fixtures and tests use synthetic rows around the demo station (Brisbane Airport): invented
  licensees (`Example Water Authority`, `Example Broadcasting`), invented sites, synthetic
  callsigns that are not valid Australian assignments (`XX4EX`), positions a few kilometres from
  the station. The repository never embeds an extract of the register; `scripts/public-check.sh`
  stays clean.
- Tiles stay OpenStreetMap's, fetched by the browser only while the map is on screen, with the
  attribution control as today and the CSP unchanged; no prefetch, no bulk download, one map per
  page. OSM's tile usage policy asks for exactly this (a valid referer and user agent, which the
  browser supplies, attribution, no heavy use). The guide's sentence on tiles is unchanged.

## 5. Docs, TODO and UAT

- `AGENTS.md` and `skills/zetasdr/SKILL.md` regenerate from `agents_md.rs`: an `origin_lookup`
  note (the two forms, what `modulation` null reads, the record's fields and what `score` and
  `agree` mean, that a lit site is a hypothesis), the `origin` protocol in the events list, the
  `view_set` keys `dock` and `origin_fade_s`, and the `map` alias.
- `docs/agents/field-craft.md`: the register paragraph (section 4.9's sentences); the page
  paragraph (`view set pane map`, `map on|off` as the alias, the dock, lit sites and what they
  mean).
- `docs/guide.md`: the view-policy table (`map` as alias, `pane` gains `map`, `dock`,
  `origin_fade_s`), the map paragraph, `zeta origin lookup`.
- `docs/themes.md`: `layout.dock`, `html[data-dock]`, `--pane-dock-w`.
- `TODO.md`: fold "UI: the Subjects list must not take over the page" (done by the tab and the
  dock) and the map half of "UI: state-stream decoders beyond the first subjects"; add the
  leftovers: per-source positions for remote sources, two panes at once, resizable dock, fuzzy
  name matching, demotion rules.
- `UAT.md`: a section "The map layer" with the live checks of section 6.4.

## 6. Testing

### 6.1 Node (`crates/zeta-core`)

- `origin.rs` unit tests over synthetic `Allocation`s: the frequency gate; the width grades
  (0.4/0.2/0/0.1); the modulation term against each `DESIGNATOR_LABELS` letter, `fits`,
  `disagrees`, `unknown` (no designator, empty labels); the reach term with and without a
  distance; receivers and plan rows counted, not scored; dedup by `(licence_no, site,
  center_hz)`; the cap at 50 best first then nearest; a confirmation sets score 1 and keeps the
  terms; the summary's three forms.
- `ops`: `origin_lookup {job}` on a decoder job reads the row's labels, on `fm_demod` reads `fm`,
  on a `bits` job reads the chain's demodulator and the capture's band, on `mix` is refused
  `invalid`; the band form with and without `modulation`; a remote source's answer has `from`
  null and `from_note` `remote source`; with no provider `providers` is empty and `origins` empty;
  the answer is in `job_context.origins` and one `origin` frame is on the job's stream and the
  source's feed. `Allocation` with `callsign` round-trips and an older record without it loads.
- `identify` and `watch` (`--features test-fixtures`): a run publishes one `origin` event after
  its lookup and its verdict carries `origins`; the watch's `unknown` event carries `origins`
  beside `allocations`. Poll for the end state (the CI-only flake rule).
- Identity: a job with the APRS row and an `origins` context sees `src: "XX4EX-7"`; the origin
  with `callsign: "XX4EX"` is confirmed and republished; `src: "XX4ZZ"` changes nothing.
- `themes`: `layout.dock` parses `bottom` and `right`, is `bottom` when absent, and `"left"` is
  refused naming `layout.dock`.
- Schema: `schema` lists `origin_lookup` and the `origins` result; `agents-md --check` and
  `--skill --check` pass.

### 6.2 Provider (`crates/zeta-ext/tests/acma.rs`)

- The fixed SQL names `CALL_SIGN`; a stub answer with the column fills `callsign`, one without it
  leaves `null`; the stub's columns list is extended.

### 6.3 Page (`crates/zeta-ui/tests/e2e/ui.py`)

The harness (`crates/zeta/tests/ui.rs`) gains a register stub: the axum stub of
`zeta-ext/tests/acma.rs` moved to a shared test helper and run in-process on a loopback port,
passed as `--knowledge-acma http://127.0.0.1:<port>`, answering three synthetic rows for the tone
source's band (a `tx` `16K0F1D` 9 km north-west of the demo station with callsign `XX4EX`, a `tx`
`12K5A3E` 20 km east, an `rx` row) and nothing elsewhere; the node's station is the demo station
(`station_set` once at start). About a minute of new scenarios:

- `scenario_map_is_a_tab`: the strip shows `Map`; `#map-toggle` is absent; `view set pane map`
  shows `#map` inside `#pane`; `view set map on` selects the same pane and `view set map off`
  deselects it; `view show` lists `map auto`; the narrow footprint shows the map in the
  waterfall's place from its tab. The three existing map scenarios switch from `view set map on`
  to the tab and keep their assertions.
- `scenario_origins_light_sites`: `fake_pos` runs; the row's `Origins` button lights two
  `path.map-site` (one `data-agree="fits"` with a `map-bearing`, one `disagrees` with none, the
  `rx` row unlit), the tooltip has licensee, site, designator and score, the popup offers `Decode
  with fake_pos` and `job_start` is posted on the measured band (asserted from the ops stream, the
  job then stopped), the transcript has the `origin` summary; `view set origin_fade_s 2` and the
  sites go within a few seconds of the job's last event; stopping the job drops them at once.
- `scenario_no_station_lights_without_bearings`: with `station_clear`, the same lookup lights the
  sites with no `map-bearing`, no distance line, and the note under the strip.
- `scenario_dock_right`: `view set dock right` puts `#pane`'s box to the right of `#waterfall`'s
  at full height; `bottom` puts it back under; a test theme with `layout.dock: right` and `dock
  auto` resolves right, and the console theme bottom; the narrow footprint ignores it.
- `scenario_classify_lights_sites`: What is this on a box over the tone lights sites from the
  classification's `origins` with a one-shot key that fades from the lookup.
- The agent's `view_set` with `dock` and `origin_fade_s` applies (the existing agent-view
  scenario's key list).

### 6.4 Live (UAT, the field host)

1. With the register attached and a watch on 400-406 MHz, an `unknown` lights the sites the
   `register says` line names, bearings from the station, the fitting one brightest.
2. `Origins` on a running ACARS decoder lights the airport's SITA assignments as `fits`.
3. `Origins` on a CB transcriber at 477 MHz lights nothing or only `disagrees` rows (CB is a class
   licence): the summary says so.
4. `view set dock right` with the Subjects tab during an ADS-B run; the table reads in the column.
5. On a KiwiSDR source, a lookup on an HF broadcaster lights sites with no bearings.
6. An APRS run with a lit repeater site: a decoded `src` equal to its callsign confirms it (only if
   the register's `CALL_SIGN` is populated for it; otherwise note the finding).

## 7. Left to the plan

- The exact markup and CSS of the site markers, the tooltip, the popup's buttons, the note under
  the strip, and the dock's selectors; the names of the scoring constants.
- How the page keys and stores lit sets (a `Map` in `MapPane` beside `tracks`), and how `agent.js`
  hands the squelch activity to the fade clock.
- The shared register stub's module and how `ui.rs` runs it (an in-process axum task is the
  intent; a `--knowledge-allocations FILE` flag feeding `StaticKnowledge::from_files`'s allocations
  is the fallback if the stub proves awkward in the harness).
- The `identity` block's schema in `modes/row.rs`, and where the node runs the match (the launch's
  event tee, as the SondeHub sink hooks events, or a small `origin_confirm` stage).
- The exact `origin_lookup` CLI words and the `--modulation` list parsing.
- Whether the Explore table's licensee column reads from `origins` (best) or stays on
  `allocations` (nearest).

## 8. Open questions already answered (folded in)

- *Does the page score?* No: the node scores, the page draws (anti-goal). This is why `classify`
  and `capture_classify` return `origins` rather than the page reusing `allocations`.
- *Where does a job's modulation come from without a measurement?* Its row, its kind, its chain
  (4.2); the op never measures.
- *What is "quiet"?* The job's frames on the feed, or the lookup time for a one-shot (4.4).
- *Which band does the decoder start on?* The measured one (the no-grid rule), the licensed centre
  only for a band lookup with no measurement (4.5).
- *What about Kiwi sources?* No station for the query, no bearings, record shape ready for a
  source position (4.8).
- *Does the fit test differ from identify's?* Same tests, graded (4.1); identify's candidate list
  and `allocation_term` are untouched.

## 9. Questions for the operator

1. **The dock's scope.** The spec docks the whole pane host, so Subjects, Map, Explore and Bits
   all go right together and the strip stays one pane at a time. If what you want is the map on
   the right *while* the Subjects table sits underneath (two panes at once), that is a second pane
   host and a different strip; say so and it becomes this plan's shape instead.
2. **Demotion on an identity mismatch.** You asked for confirm or demote. The spec confirms only:
   on shared channels (APRS, CB, repeaters) a callsign that is not the licensee's proves nothing
   about the site. Should a mismatch demote a site when the service is one transmitter per
   frequency (broadcast), or never?
3. **Remote sources now or later.** For a KiwiSDR the spec lights sites with no bearings and no
   reach cut. Do you want a per-source position (`lat=,lon=` on `--source-kiwi`, or read from the
   Kiwi's own GPS) in this plan, so HF lookups are cut and drawn from the receiver?
4. **The Map tab always present, or earned?** The spec shows the tab always. The alternative is
   eligible only with a station, a register, or a positioned job (so a bare node has no tab).
5. **What a map click may start.** The spec offers `Decode with <mode>` and `Listen` through
   `job_start` only, with conflicts as buttons. Is a one-click decoder start from the map what you
   want, or should the click only show the assignment and leave starting to the box menu?
6. **The fade default.** 300 s, the subjects' drop time, from the job's last frame. Shorter (a
   sonde stops and its site should go), longer (a broadcaster is always there), or should a lit
   site simply stay while the job runs and fade only after it ends?

## 10. Calls made in this document (check these while reading)

- The dock is a property of the pane host, not of the Subjects pane alone (one mechanism; the
  operator's decision 4 is met for Subjects and the other panes come with it). The right dock's
  width is a theme token, 40% by default, not resizable in this plan.
- `dock auto` resolves to the theme's `layout.dock`: the first view policy with a theme default,
  on the `plan auto` precedent. The theme does not override an operator's explicit `bottom|right`.
- The `map` policy stays as an alias of `pane` (as `subjects` is) with a new default `auto`,
  rather than being removed: stored values, the agent's `view_set map`, the guide and the field
  craft keep working.
- The Map tab is always eligible (question 4).
- Scoring grades `allocation_match`'s own tests (width 0.4, modulation 0.4, reach 0.2) and adds
  nothing new; a confirmation is score 1. Receivers are never origins. The cap is 50, apart from
  identify's 5.
- A remote source is queried without the station, instead of with a wrong one.
- `origin_lookup` never measures; `classify` does, and so returns `origins`. A job's modulation
  is read from its row, kind or chain.
- The `origin` event goes on the job's stream and `job_context.origins` keeps the last answer;
  a band lookup has neither.
- Decoders offered from a site start on the measured band, not the licensed centre.
- Identity matching is exact callsign only, declared on the row (`subject.identity`), run on the
  node, confirm-only (question 2). Only `aprs_decode` gets the field now; `CALL_SIGN` is to be
  confirmed against the register server, and the field is `null` when absent.
- Lit sites fade on the job's activity with `origin_fade_s` (default 300) as a view policy, not a
  constant; one-shot sets fade from their lookup.
- The e2e's register is an in-process stub of the real MCP service with synthetic rows, so the
  provider's SQL and parsing run in the suite; the alternative static-file flag is the fallback.
- No real register extract in the repository; synthetic rows around the demo station only.
- Tiles, CSP and the Leaflet vendoring are unchanged.
