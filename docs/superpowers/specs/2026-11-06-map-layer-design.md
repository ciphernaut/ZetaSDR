# Plan 32, the map layer: licensed sites as the likely origins of signals

Design spec, 2026-11-06, revised the same day with the operator's answers (section 9). From the
operator's brainstorm of the same day, kept apart from the digital-mode sprints
(`2026-11-06-decoder-wave2-design.md`, `2026-11-06-aircraft-links-design.md`,
`2026-11-06-span-decoding-and-sets-design.md`) at the operator's request; the wave 2 spec lists
"the map tab" as out of its scope and this document is that tab. Builds on the map pane
(`crates/zeta-ui/web/map.js`, plan 17 spec 7.2), the source tabs and the side column
(`app.js`, `index.html`), the pane strip (`panes.js`, plan 19 spec 5.7), the view policies
(`view.js`, spec 4.3), subjects (`2026-11-02-subjects-design.md`), themes
(`2026-11-03-themes-design.md`), the knowledge providers (`crates/zeta-core/src/knowledge.rs`,
the `acma` provider in `crates/zeta-ext/src/acma.rs`), the register scoring in
`crates/zeta-core/src/modes/score.rs` and the KiwiSDR source (`crates/zeta-source-kiwi`). A
reviewer read it as the builder before hand-over; its questions are folded in, settled in
section 10 or decided by the operator in section 9.

The decisions in sections 1.1 and 9 are the operator's. The shape in the rest of the document
is the author's, which the operator saw and did not object to; each call is listed in section
10 and is open to correction.

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
was heard, with a bearing from the receiver that heard it, and fades them as the signal goes
quiet. A click on a site shows the assignment. The agent and MCP see the same record as an
`origin` event and an `origin_lookup` op, so "where is that coming from" is answerable in every
transport.

Three layout changes come with it, all the operator's. The map stops being a toggle that halves
the waterfall and becomes a tab of the **sources strip**, beside the radios: selecting it
replaces the RF view (the waterfall) in the main area, and a source tab brings the RF view back
exactly as it was (the rule from 2026-11-02: a new view earns a tab beside the radios, it never
takes a slice of the waterfall). The pane host, which today only sits under the waterfall, can
be docked into the right-hand column as another collapsible section stacked with the Transcript
and Jobs panes, so the Subjects table (or Explore, or Bits) stands beside the main view instead
of cutting its height. And a KiwiSDR source carries its own position, so what is heard through
it is cut and drawn from the Kiwi, not from the station.

### 1.1 The operator's decisions

From the brainstorm (2026-11-06, morning):

1. A separate spec for the map layer, apart from the digital-mode sprints.
2. Likely origins light up on the map when a licensed frequency matches a signal's measured
   modulation type and an entry in the ACMA register (the `--knowledge-acma` provider).
3. The map is a whole new tab of the page, replacing today's map toggle over the waterfall.
4. The Subjects pane becomes relocatable to the right-hand side of the main view as well as
   underneath it.
5. The origin query may start as a query against a new signal: identify, watch and capture
   already consult the register. The operator was unsure whether existing jobs and bits chains
   have a register lookup. Finding (section 3): they do not.
6. Decoded id to map where practical: when a decoder or bits chain yields an identity (a
   callsign, an aircraft registration, an MMSI; a pager address is not one) and the register
   carries a field it can be matched against, the match confirms a lit site. Where the register
   has no such field, the identity is shown on the map as its own subject only, as ADS-B and
   sondes are today, and does not touch the sites.

From the answers to the reviewer's questions (2026-11-06, afternoon; section 9 has the detail):

7. The Map is a tab **in the sources strip** (`#tabs`, where the radios are listed), and when
   selected it **replaces the RF view** in the main area. The RF view may shrink to a small live
   window somewhere (a thumbnail: a droppable stretch item, the builder's call).
8. The panes (Subjects, Explore, Bits) dock either underneath the main view, as today, or in the
   **right-hand column**, stacked with the Transcript and Jobs panes as another collapsible
   section.
9. Direction, not scope: a ComfyUI-style layout with operator flexibility (section 2, non-goals).
10. Each KiwiSDR source carries its **own position**, read from the Kiwi's status where it
    reports one, else configured on the source, and signals received through it are cut and
    drawn from the Kiwi's position. Now, in this plan; plan 30 (aircraft links) only consumes it.
11. Identity matching is **confirm-only**.
12. A click on a lit site **shows the assignment only**; nothing is started from the map.
13. The fade default of **300 s** stands, tweakable through the view policy.
14. The Map tab is **always present**.

## 2. Goals, non-goals, anti-goals

### Goals

- One origin query on the node, "what is licensed on this band, and does the designator agree
  with layer 1", with one result record whoever asks: identify, the watch and the capture ops
  automatically (as they consult the register now), and on demand for any job, bits chain,
  selection or band through `origin_lookup`.
- The query is made from the receiver that heard the signal: the station for a local radio, the
  source's own position for a KiwiSDR that has one (section 4.11), and with no vantage otherwise.
- The page lights each candidate site on the map, brightness by score, with a bearing line from
  the vantage; hover gives licensee, site, designator and score; a dim ring marks an assignment on
  the frequency whose designator disagrees with the measured modulation; lit sites fade as the
  signal goes quiet. A click on a lit site shows the assignment and nothing else.
- The Map is a tab of the sources strip (`view set main map`, or its tab); selecting it replaces
  the RF view in `#main`, and the shown source's RF view (its zoom, pins, outlines and page job)
  is exactly as it was when a source tab brings it back. The map toggle and `data-map` go.
- The pane host docks under the main view or into the right column (`view set dock
  bottom|right|auto`), a view policy like the others, with the theme's `layout.dock` as the
  `auto` default; on the right it is one more collapsible `section.pane` in `#side`.
- The agent, MCP and the CLI see origins: an `origin` event on the job's stream (so the source's
  feed carries it), `origin_lookup`, and the last answer kept in `job_context.origins`.
- A decoded identity confirms a lit site when the register carries a field it can be matched
  against (section 4.6); otherwise it stays a subject.
- A KiwiSDR source's position is a field of its `source_list` record (`position`), read from the
  Kiwi's own status where it has one, else from `--source-position`; a source with
  neither has `null` and is treated as a remote source without a vantage.
- Honest degradation: no station (no bearings, sites still lit), no register (the tab shows
  subjects only), a remote source without a position (no bearings, no reach cut).

### Non-goals

- Direction finding, triangulation or any position estimate from the signal itself. A lit site
  is the register's claim about who may transmit there, scored for plausibility; it is never a
  measurement of where the signal came from. The record says so (`agree`, `score`) and the field
  craft repeats "consistent with", never "is".
- A node-side store of subjects (TODO "Subjects: a running job keeps its tables on the node").
  `job_context.origins` is one small step in that direction and nothing more.
- Reading a position from the other remote source kinds. `Port::position` is defined for every
  port and only the KiwiSDR fills it from its receiver here (any kind can be given one with
  `--source-position`); another node's source (`zeta-source-node`) could forward its
  node's station and a VRT stream could carry a context packet's position, but neither is built
  (TODO, section 5).
- Starting a decoder from the map. The operator decided (section 9, item 12) that a click on a
  lit site shows the assignment only; the box menu and the job rows are where jobs start.
- Fuzzy matching of decoded names against licensee names (an RDS `ps` of `4KQ` against a
  licensee's trading name). Only exact callsign matches are built (section 4.6). Demotion on a
  mismatch is not built (item 11).
- A register other than the ACMA one. The design is against `Allocation`, so another provider
  that fills the same fields gets the map for free, but none is written here.
- Resizable panes (a drag handle on the dock). The docked section's share of the column is a
  theme token.
- Two panes of the strip on screen at once (Subjects under the main view *and* Explore in the
  column). The strip stays one pane at a time, docked where `dock` says; the map in the main
  area with Subjects docked right is the two-at-once the operator asked for.
- Offline tiles, a second tile provider, or caching tiles on the node.
- **Direction, not this plan's scope: a ComfyUI-style layout.** The operator's longer aim is a
  page the operator arranges: views, panes and graph editing as movable, resizable nodes in the
  style of a ComfyUI canvas, with the engine facts that make it possible (fan-out on the bus,
  re-expansion of operator graphs). `TODO.md`'s gist "Architecture: fan-out, and the transcriber
  as part of its demodulator's graph (towards a ComfyUI-style editor)" is where that direction
  is kept, and this spec adds to it the layout half: the `main` and `dock` policies are the
  first two "where does this view go" switches, and a canvas layout would subsume them. Nothing
  in this plan precludes it; nothing in this plan builds it.

### Anti-goals

- Drawing a register row as a fact about the signal. A site is lit by a score the record carries;
  a confirmed one says what confirmed it. No "transmitter located" wording anywhere.
- The page computing scores or reading the register itself. The node is the one place that
  scores; the page draws what the record says, so the agent, MCP and the page never disagree.
- Any job started, stopped or replaced from the map. The popup has no action buttons (item 12).
- Real register extracts in the repository. Fixtures are synthetic rows around the demo station
  (section 6); tests never embed a real licensee, licence number or site.
- A tile fetch the operator did not cause: tiles load only while the map is on screen, and never
  in bulk.
- The operator's KiwiSDR host name, or any LAN address, in a tracked file. Tracked text says
  "the local KiwiSDR" or `kiwi-host`; the real name lives in the node's untracked unit or
  environment. No plan task, deploy helper or persisted record adds the operator's Kiwi to any
  node: the operator adds their own sources themselves.

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
  `client.LICENCEE` (`parse_rows`, the fixed SQL in `assignments_sql`) into `Allocation`
  (`licensee`, `site`, `lat`, `lon`, `distance_km`, `bearing_deg`, `power_w`, `center_hz`,
  `bandwidth_hz`, `licence_no`, `role`, `emission`, `emission_decoded`, `as_of` always `null`).
  None of these is an identity a decoder yields. The register's `device_details` table has a
  `CALL_SIGN` column (to be confirmed against the register server; it is in the ACMA RRL extract
  the server loads) and `client` has `TRADING_NAME` (to be confirmed). Aircraft registrations,
  ICAO addresses and MMSIs are not register fields: aircraft and ship stations are class licences
  in Australia. Amateur stations have been under a class licence since 2024, so an APRS callsign
  is unlikely to match a register row (repeaters and beacons licensed as apparatus still carry
  callsigns; to be confirmed). So decision 6 reduces to one match today, decoded callsign against
  `CALL_SIGN`, and most identities stay subjects.
- **The provider's shape, and where its station comes from.** `AcmaKnowledge::lookup` asks
  `get_frequency_allocation` for the plan and one `execute_sql` for the assignments, cut to the
  band's reach around the station (a bounding box from `reach_for`), at most `ACMA_MAX_ROWS` (50)
  nearest first; without a station there is no box and rows sort by frequency offset.
  `ACMA_TIMEOUT` is 5 s for a whole lookup. `nearest_allocations` keeps `NEAREST_ASSIGNMENTS` (5)
  for identify and classify; the map wants more. **The station is not a parameter of the
  provider trait**: `KnowledgeProvider::allocations(region, center_hz, bandwidth_hz)` has none,
  and the `acma` provider reads its own `StationFn` closure (`node_setup.rs attach_acma`:
  `station::load(&dir)` from the state directory) inside `lookup`. So "ask from the Kiwi's
  position" or "ask with no station" needs a trait method that takes the vantage per call
  (section 4.1); the earlier draft's "wrapper that withholds the station" could not have worked.
- **Scoring.** `score::allocation_match` is the fit test (a `tx` assignment whose width is within
  `ALLOCATION_WIDTH` (0.5-1.5) of the row's and whose designator letter is compatible with one
  of the row's labels, `DESIGNATOR_LABELS`); `ALLOCATION_SCORE` is 0.2. This spec grades the same
  tests rather than inventing new ones.
- **Allocation is strict.** `#[serde(deny_unknown_fields)]`, every field written and required in
  the schema (`require_all`); a new field is `#[serde(default)]` so stored capture records load.
- **Sources.** `Capabilities` (`types.rs`) has `remote: bool` and no position; the KiwiSDR source
  sets `remote: true`. `Port` (`port.rs`) has `capabilities`, `current`, `fault`, `gain`,
  `stats`, `worker` and no position. `SourceInfo` (`ops.rs`) is strict; its defaulted fields
  (`worker`, `ring`) are listed in `require_worker` so the schema still requires them. The
  KiwiSDR source (`zeta-source-kiwi`) keeps a `Status` (rates, RSSI, overflows, gaps, fault) fed
  by the websocket session's `MSG` lines; it never reads the Kiwi's HTTP side. Every KiwiSDR
  serves `GET /status` without a password: a plain-text `key=value` document that carries the
  receiver's position as `gps=(<lat>, <lon>)` (the owner's configured position, or the GPS fix
  when the Kiwi has one), `gps_good=<fixes>` and `asl=<m>`, among other lines (the exact lines
  to be confirmed against the local KiwiSDR from a developer shell; the parser is tested on a
  fixture text). `--source-kiwi ID=HOST:PORT[/PASSWORD]` is parsed by `sources.rs parse_kiwi_with`
  into `SourceSpec::Kiwi {id, host, port, password}`, with the password refused when it holds a
  space, `=` or a control character; `node_setup.rs` builds `KiwiConfig::new(host, port,
  password)` and `KiwiSource::new(id, cfg)`. The crate's fake Kiwi (`testing.rs`, feature
  `test-fake`) is a websocket server only; it cannot answer a plain `GET /status`.
- **The page's layout.** `#top` holds `#tabs` (the source tabs, `button.src-tab[data-id]`, rebuilt
  by `app.js renderTabs(sources)` on each `source_list` and re-decorated by `syncTabs`; a click
  runs `select(id)`, which stores `zeta.source`, sets `aria-selected` and notifies the
  `onSourceChange` listeners), `#panes` (the pane strip), `#listen-all`, `#map-toggle` and the
  drawer toggle. `#main` holds `#waterfall` (absolute, inset 0), `#map` (absolute, the right half
  while `html[data-map="on"]`) and `#pane` (absolute, bottom 40%, shown while a pane is selected;
  `html[data-strip]` lifts `#waterfall` and `#map` by 40%). `#side` is the grid's right column:
  two `section.pane`s (`.transcript`, `flex: 1`; `.jobs`, `max-height: 40%`), each with an `h2 >
  button.pane-head[aria-expanded]` that folds it (`section.pane[data-collapsed]`; `app.js
  applySide`, `mountSideHeads`, `SIDE_PANES`, the `side` policy `both|transcript|jobs|none`). In
  the narrow footprint `#side` is a drawer, a pane or the map takes the waterfall's place
  (`data-pane`), and `#map-toggle` shows only with `data-map` on. `panes.js` registers panes with
  `eligible`, `el`, `onShow`, `onHide`; one pane at a time; the selection is the `pane` policy;
  `subjects on|off` is an alias of it.
- **View policies.** `POLICIES` in `view.js` is the registry (`values` or `parse`, `default_`,
  `describe`, an optional `then` for aliases); `AGENT_VIEW_KEYS` (page) and `VIEW_KEYS`
  (`zeta-agent/src/page.rs`) list what the agent's `view_set` may set; both carry `map` today.
  Values live in `sessionStorage`.
- **Themes.** `theme.json`'s `layout` is `{rail, ribbon_tilt_deg}` (`themes/theme_json.rs
  Layout`, `deny_unknown_fields`); `footprints` is reserved and refused. The page reads
  `layout.rail` into `data-rail`. There is no mechanism yet for a theme to default a view
  policy; `plan auto` and `scanner auto` are the precedent for a policy whose default is resolved
  elsewhere. `docs/themes.md` lists the root attributes a theme may style against
  (`html[data-theme]`, `html[data-rail]`, `html[data-footprint]`) and the e2e `THEME_HOOKS` list
  checks each named hook exists on the live page and in the docs.
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
  `job_context_merge(id, patch)` (`node/mod.rs`) is how the node writes into a job's context;
  `tee_transcripts` (`launch.rs`) is the shape of a per-job event watcher that is not a stage.
- **The e2e.** `crates/zeta/tests/ui.rs` spawns `zeta serve` (`ServeProc::start_full`) with file
  sources and a `--modes-dir` of fake rows (`fake_pos`, `fake_subjects`, run by `zeta-ext`'s
  `fake_lines`); no knowledge provider is attached and no station is set.
  `crates/zeta-ext/tests/acma.rs` has an in-process axum stub of the register (SSE MCP answers
  for `get_frequency_allocation` and `execute_sql`). The suite's deadline is 690 s and it runs
  about 474 s; new scenarios have a budget of about a minute together. The theme-shots scenario
  and `THEME_HOOKS` use `view set map on` and `#map`.
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
keeps the `assignment` rows, and scores each `tx` row (a receiver is never an origin; receivers
and plan rows are counted in the answer, not scored). Rows are deduplicated by `(licence_no,
site, center_hz)` and capped at `ORIGIN_SITES = 50`, best score first, then nearest. The cap is
separate from `NEAREST_ASSIGNMENTS` (5), which identify and classify keep for their candidate
lists.

**The vantage.** The query is made from where the signal was received, resolved per source by
`vantage_for(source) -> Option<Vantage>`:

| Source | Vantage | `from` in the answer |
|---|---|---|
| `Capabilities.remote` false (a local radio, a recording) | the station | `{lat, lon, kind: "station"}`, or `null` with `from_note: "no station set"` |
| remote with a `position` (a KiwiSDR, section 4.11) | the source's position | `{lat, lon, kind: "source", source: <id>}` |
| remote without one (another node's source, a Kiwi with no position) | none | `null`, `from_note: "remote source without a position"` |

`Vantage {lat, lon, alt_m, antennas}`: for the station its own antennas; for a source position
`Station::default_antennas` (the 0-30 MHz dipole and the discone), since a Kiwi's antenna is
unknown to the node. The provider trait gains the vantage as a per-call parameter:

```rust
async fn allocations_from(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz,
                          from: Option<&Station>) -> Vec<Allocation>;   // default: self.allocations(..)
async fn reach_from(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz,
                    from: Option<&Station>) -> Option<Reach>;           // default: self.reach(..)
```

`AcmaKnowledge` implements both by passing `from` to its existing `lookup(station: Option<Station>,
..)` instead of its `StationFn`; `allocations` and `reach` keep calling the closure, so
`knowledge_lookup`, identify and classify are unchanged. The static providers ignore `from`. The
origin module builds the `Station` value for a source vantage from the position, the default
antennas and the station's region (else `DEFAULT_REGION`). So a Kiwi's lookup is cut to the reach
around the Kiwi, its rows carry `distance_km` and `bearing_deg` from the Kiwi, and nothing about
the register side knows which kind of vantage it was given.

**The score.** Three graded terms, the same tests as `allocation_match`, summing to at most 1:

| Term | Weight | Rule |
|---|---|---|
| frequency | gate | the row's centre and the band overlap: `abs(row.center_hz - center_hz) <= row_width/2 + bandwidth_hz/2`, `row_width` the decoded designator's bandwidth, else the row's `bandwidth_hz`, else 0. A row that fails is not a candidate. |
| width | 0.4 | `r = row_width / bandwidth_hz`: 0.4 for 0.5 <= r <= 1.5; 0.2 for 0.25 <= r <= 4; 0 outside; 0.1 when the row has no width. Amended after the live deploy (2026-11-08): the grade is multiplied by `1 - abs(row.center_hz - center_hz) / (row_width/2 + bandwidth_hz/2)` and rounded to 2 places, so a row that only overlaps the band's edge no longer ties the on-channel one. |
| modulation | 0.4 | the designator's modulation letter compatible with one of `modulation` (`DESIGNATOR_LABELS`): 0.4, `agree: fits`; incompatible: 0, `agree: disagrees`; no designator or `modulation` empty: 0.2, `agree: unknown`. |
| reach | 0.2 | `distance_km` known and within the reach toward the site: 0.2; no distance (no vantage): 0.1; known and out of reach (only a static provider's rows, the register's are cut already): 0. Amended after the live deploy (2026-11-08): within the reach the term is graded by distance, `0.2 * (1 - 0.5 * distance_km / reach_km)` rounded to 2 places (0.2 at the station, 0.1 at the limit; a reach with no radius keeps 0.2), so a station 5 km away no longer ties one 266 km away inside a 300 km reach. |

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
  source: SourceId,
  modulation: Vec<String>, symbol_rate_baud: Option<Baud>,
  from: Option<{ lat, lon, kind: "station" | "source", source: Option<SourceId> }>,
  from_note: Option<String>,                              // "no station set", "remote source without a position"
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
  with no second op. The capture record keeps `allocations` only; `capture_classify` recomputes,
  with the capture's source as the vantage rule's source.
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
  source for the vantage rule is the capture's source. A job with no band (`mix`, `spectrum`, a
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
{ "protocol": "origin", "job": "<id>", "source": "<id>", "band": {...}, "modulation": [...],
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

**Where it is.** The Map is a tab of the sources strip: `renderTabs` appends one fixed
`button.src-tab.map-tab[role=tab][data-view="map"]` labelled `Map` after the source tabs, on every
render (it is not a source, so `source_list` never lists it and `syncTabs` never touches it). It is
always present (item 14), whether or not anything is lit, positioned or set.

**What it does.** A new view policy `main: rf | map` (default `rf`) says what `#main` shows.
The Map tab sets `main map`; a source tab sets `main rf` (and selects that source, as today); a
second click on the selected Map tab sets `main rf`. The page sets `<html data-main="rf|map">`
and `style.css` has one selector per value:

- `rf`: as today, `#waterfall` fills `#main` (less the bottom dock), `#map` is `display: none`.
- `map`: `#map` fills `#main` (less the bottom dock), `#waterfall` is `visibility: hidden` (it
  keeps its box, so its canvases keep their size and the page's spectrum job keeps streaming;
  `waterfall.js` skips painting while hidden, so a hidden waterfall costs no draw).

`#map` moves out of its right-half position into `#main` as a full-size sibling of `#waterfall`;
`#map-toggle`, `data-map`, `applyMapPane`, the narrow footprint's `data-pane="map"` rules and the
`#map` half-width rules in `style.css` go. `map.js` is told to show on `main` (`view.onChange`,
`invalidateSize` on each show, Leaflet loaded on the first), nothing while hidden. In the narrow
footprint the Map tab works exactly the same (the map takes the waterfall's place, as it did
through `#map-toggle`); the pane strip's narrow behaviour (a pane in the main area's place while
its tab is selected) is unchanged and sits over the map as it sits over the waterfall.

**What persists.** Selecting the Map tab changes nothing about the shown source: `state.selected`
and `zeta.source` keep the source, its tab keeps the shown colour (class `kept`, so the operator
sees which source the page returns to; `aria-selected` moves to the Map tab, one selected tab per
tablist), the waterfall keeps its zoom, its pins, its outlines, its ribbons and its elastic
spectrum job (the overlays keep following the feed, so the subjects, the squelch dots and the lit
sites stay current), the box menu's selection is dropped as a source switch drops it, and the
command line keeps operating on the shown source (`shown()` is unchanged). Returning (`main rf`)
shows the waterfall exactly as it was, with no retune, no relaunch and no re-centre.

**The `map` policy.** `map on|off` (`AGENT_VIEW_KEYS`, `VIEW_KEYS`, the guide and the field
craft) becomes an alias of `main`, as `subjects` is of `pane`: `on` sets `main map`, `off` sets
`main rf`, and a `main` set puts it back to its new default `auto`, which changes nothing. Stored
`map on` values, the agent's `view_set map on` and the typed `view set map on` keep working;
`view set main map` is the direct form. `main` joins `AGENT_VIEW_KEYS` and `VIEW_KEYS`
(presentation only).

**What the map draws**, beyond today's tracks, home and subjects:

- **Sites.** One `L.circleMarker` per lit site (class `map-site`, `data-agree` fits|disagrees|
  unknown, a `--site-brightness` custom property 0..1 the CSS maps to fill opacity), at
  `allocation.lat/lon`. A site that `disagrees` is a dashed ring with no fill (`map-site` with
  `data-agree="disagrees"`). Two assignments at one position (one site, several devices) are one
  marker whose popup lists them; the marker takes the best score.
- **Bearings.** One non-interactive `L.polyline` (class `map-bearing`, the same brightness) from
  the answer's `from` to each lit site that `fits` or is `unknown`, when `from` is set and the
  site has a position. None for `disagrees`, none when `from` is `null`. The line starts where
  the node says the signal was received, not at the page's `home`: for a Kiwi that is the Kiwi.
- **The receiver.** When a lit set's `from.kind` is `source`, a non-interactive marker (class
  `map-source`, labelled with the source id) at `from`, so the Kiwi is on the map beside the
  home; it goes with the set.
- **Hover.** A Leaflet tooltip: `<licensee>` / `<site>` / `<emission> (<words>)` /
  `<score> <agree>` / `<d> km, <bearing>°` (the last line only with a distance), plus
  `confirmed by <value>` when confirmed.
- **Click.** The popup shows the assignment and nothing else (item 12): the tooltip's lines,
  `licence <no>`, `<power> W`, the measured band and labels the lookup used, the source it came
  from, `from <station|source id>` when a vantage applied, and `modulation not measured` for an
  unknown. No buttons: nothing starts, stops or is looked up from the map. Unlike a subject
  marker, a click selects nothing in the Subjects store.
- **Brightness and fade.** `brightness = score × freshness`, `freshness = max(0, 1 − age /
  origin_fade_s)`, `age` the seconds since the lit set's signal was last active. For a job-bound
  set, active means any event frame of the job on the feed (a decoder's frames, a demodulator's
  squelch opening as the page already tracks it for the dot, `overlays.js lastEvent`) or a fresh
  `origin` event; for a one-shot set (a classify, a capture_classify, a band lookup) the lookup
  time, so it fades from the moment it lit. At zero the site goes. The clock is the page's,
  checked each second with the subjects' tick. `origin_fade_s` is a new view policy (a positive
  integer of seconds, default 300, the subjects' drop time; item 13), in
  `AGENT_VIEW_KEYS`/`VIEW_KEYS`.
- **Keys.** Lit sets are keyed by job id (one set per job; a new `origin` event replaces the
  job's set) or by a one-shot key (`classify:<t_ns>`, `capture:<id>`, `band:<t_ns>`); a job's set
  goes with the job (`map.sync`, as tracks do). Sets are kept while the tab is not shown, so what
  lit during a classify is there when the tab opens, within the fade.
- **Seeding.** On the first show and after each jobs poll, the page reads `job_context.origins`
  of running jobs whose context names it (the same poll that seeds squelch gates), so a reload or
  a second page shows what the node last answered, aged from the record's `t_ns`.
- **The tab's badge.** The Map tab shows the number of lit sites as a `span.count` (empty at
  zero), the Jobs pane's count's shape, so an answer that lit something is visible from the RF
  view without switching; the page never switches to the map on its own.
- **Centre.** Unchanged: once, on the first fix or the home. A lit site never moves the view.
- **Feed cost.** None new: `origin` events ride the source's feed the page already reads. A
  one-shot lookup is one op.

### 4.5 A click on a lit site

The popup is informational (4.4, "Click"). The earlier draft offered `Decode with <mode>`,
`Listen` and `Look it up` buttons; the operator decided against starting anything from the map
(item 12), and the register row's JSON is in the `origin` event and `job_context.origins` for
the agent, so `Look it up` goes too. Starting a decoder on a lit band is the box menu's job, on the
RF view, on the measured band as today (the no-grid rule).

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
`summary` `confirmed <value>: <licensee>, <site>`. A mismatch changes nothing (confirm-only, item
11): APRS, CB and repeater channels carry many stations, so a decoded callsign that is not the
licensee's proves only that someone else also transmits there. The page draws a confirmed site
with a ring in the selection colour (`map-site` with `data-confirmed`), and the tooltip's
`confirmed by <value>` line.

### 4.7 The dock

A new view policy `dock`: `auto | bottom | right`, default `auto`. `auto` resolves to the theme's
`layout.dock` when its `theme.json` has one, else `bottom`. It positions the pane host `#pane`,
whichever pane is shown (Subjects as the operator asked, and equally Map-less Explore and Bits:
one mechanism rather than a per-pane variant). The right column already exists, so the right
dock is a slot in it, not a second pane host:

- `bottom`: as today, `#pane` the bottom 40% of `#main`, the main view (`#waterfall` or `#map`,
  whichever `main` shows) lifted by 40% while a pane is selected (`html[data-strip]`).
- `right`: `#pane` is moved into `#side` as a third `section.pane.dock` (`id="pane-dock"`) with
  the column's shape: an `h2 > button.pane-head[aria-expanded][aria-controls=pane-dock]` whose
  text is the shown pane's label (`Subjects`, `Explore`, `Bits`) and, for Subjects, its count,
  and the pane host as its body. It stacks under the Transcript and above the Jobs pane,
  `flex: 1 1 var(--dock-share)` (a theme token, the core default 40% of the column), and folds
  like its neighbours: a click on its head toggles `section.pane[data-collapsed]` (kept in the
  page's memory, not a policy; the `side` policy's words stay `both|transcript|jobs|none` and
  name only the two panes they always named). The main view keeps its full height; `data-strip`
  is still set so a theme may read which pane is up. The section exists only while a pane is
  selected and the dock is right; `pane none` removes it. The move is a DOM move of the same
  `#pane` element (`appendChild`), so the panes' `el`, `onShow` and `onHide` are untouched and
  `panes.js` needs no change beyond `invalidate`-style redraws the panes already do on show.
- The narrow footprint ignores the dock: panes take the main area's place there, and `data-dock`
  is still set so a theme may read it.

The page sets `<html data-dock="bottom|right">` from the resolved value (so `style.css` has one
selector per dock) and re-resolves it on a theme change (`applyTheme`) and a `dock` change. `dock`
joins `AGENT_VIEW_KEYS` and `VIEW_KEYS` (presentation only). `theme.json` gains `layout.dock` as
an optional field (`bottom` when absent), refused with the field's name when it is neither word;
the built-in themes do not set it; the loader's `footprints` reservation stands, and when
footprints arrive they override `layout.dock` like the rest of `layout`. `docs/themes.md`'s table
gets the row, `html[data-dock]` and `section.pane.dock` join the hooks a theme may style against,
and `THEME_HOOKS` in the e2e gains them.

### 4.8 Degraded cases

- **No station.** For a local source `from` is `null`, `from_note` `"no station set"`. Sites are
  lit by frequency, width and designator; the reach term scores its unknown value; no bearings,
  no distances, the tooltip has no distance line. The register's query has no bounding box, so
  the rows are the 50 nearest in frequency anywhere in the region: the Map shows a note in its
  corner, `no station set: sites are not cut by reach (station_set)`, once per load. The map
  centres on the first lit site when there is no home and no fix.
- **No register provider.** Every answer has `origins: []` and a `providers` list without an
  assignment provider; the summary says `no register provider`. The Map shows subjects, tracks and
  the home as today; the Origins buttons (4.9) say the same in the transcript and light nothing.
  An attached but unreachable service is the same from the page's point of view (`providers`
  names `acma`, `origins` is empty, the node logs the failure as it does now) and the summary says
  `register gave no rows`; the distinction is in `providers`.
- **A remote source with a position** (a KiwiSDR whose status or flag gave one). The query is
  made from the Kiwi (4.1): rows are cut to the reach around it with the default antennas,
  distances and bearings are from it, `from` is `{kind: "source", source: <id>}`, the map draws
  the `map-source` marker and the bearings from it. HF rows are many (a frequency may be licensed
  to dozens of sites across the country), so the reach cut and the cap of 50 by score matter here.
- **A remote source without a position** (another node's source; a Kiwi whose status has no
  `gps=` line and no `--source-position`). No vantage: `from` is `null`, `from_note` `"remote source
  without a position"`, no bearings, no reach cut, the 50 nearest in frequency across the region.
  The map draws the sites; nothing says where the receiver is. The tooltip has no distance line.
- **A signal with no label.** `modulation` empty scores every designator `unknown` (0.2): sites
  light at middling brightness with `agree: unknown`, and the tooltip says `modulation not
  measured`. A classify on the band replaces the set with a scored one.
- **Tiles offline.** As today: Leaflet draws markers, lines and attribution over blank squares.

### 4.9 Where the page asks

- **The job row** (agent.js) gets an `Origins` button beside `graph` for any running job with a
  band: `origin_lookup {job}`; the answer's summary is the transcript line (the `origin` event
  provides it), the sites light, the Map tab's badge counts them. The page never switches to the
  Map on its own (the operator may be mid-box on the waterfall); TODO "UI: more actions on an
  existing job" is the home of the shared entry table this button will join.
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
- The operator's KiwiSDR: its host name is private and stays in the node's untracked unit or
  environment; tracked text says "the local KiwiSDR" or `kiwi-host`. The source's position is
  the Kiwi owner's published position (every public Kiwi shows it on its own page), and a test
  fixture uses a position near the demo station, never the operator's. A developer may run the
  Kiwi status reader against the local KiwiSDR from a shell to confirm the `/status` lines; the
  deployed node is never touched by the plan, and no fixture records the Kiwi's answer verbatim.
- Tiles stay OpenStreetMap's, fetched by the browser only while the map is on screen, with the
  attribution control as today and the CSP unchanged; no prefetch, no bulk download, one map per
  page. OSM's tile usage policy asks for exactly this (a valid referer and user agent, which the
  browser supplies, attribution, no heavy use). The guide's sentence on tiles is unchanged.

### 4.11 The source's position (the KiwiSDR)

Each KiwiSDR source carries its own position (item 10). This section is the single definition of
the source position, shared with plan 30 (section 4.5 of its spec): plan 32 (Task 2) owns the
implementation. If plan 30 lands on `main` first, it implements exactly this shape and plan 32
reuses it as landed; if plan 32 lands first, plan 30's task reduces to consuming
`SourceInfo.position`.

- **`Port::position`.** The trait gains `fn position(&self) -> Option<SourcePosition>` with a
  default of `None`: what the port itself knows (a Kiwi's status read), never the operator's
  flag. `SourcePosition {lat: f64, lon: f64, alt_m: Option<f64>, from: "status" | "configured"}`
  (`types.rs`, strict, nullable schema, `alt_m` required-and-nullable). `SourceInfo` gains
  `position: Option<SourcePosition>` (`#[serde(default)]`, nullable in the schema, added to
  `require_worker`'s list so the schema requires it; a plan-20 node attaching an older node's
  source reads `null`). `source_list` and the page's `source_list` poll carry it with no other
  change.
- **Where a source's position comes from.** Two inputs, in this order:
  1. the receiver's own status, for a Kiwi: on each connection, once, the session fetches
     `GET /status` (`KiwiConfig.status_url`, `http://<host>:<port>/status` by default) as one task
     bounded by `handshake_timeout_ms`, after `SET auth` is accepted and before the first `SND`,
     over a hand-written HTTP/1.0 request on a `TcpStream` (no HTTP client dependency), so a hang
     or a refusal neither delays the tune nor faults the source. `kiwi::status::parse(text) ->
     KiwiStatus {position: Option<(f64, f64)>, asl_m: Option<f64>, gps_good: Option<u32>, name:
     Option<String>}` reads the `gps=(<lat>, <lon>)`, `asl=` and `gps_good=` lines (a line it does
     not know is ignored; a `gps=` outside -90..90/-180..180 or `(0, 0)` is treated as none,
     since an unconfigured Kiwi reports the origin; a page with no `gps=` line gives none). A
     fetch that fails or times out leaves the position as it was and counts in
     `Status.status_errors`; the next reconnect tries again. The position is kept in
     `Status.position` with `from: "status"` and `alt_m` the status's `asl` when known, else
     `null` (the default antennas stand 5 m up regardless).
  2. the generic flag `--source-position ID=LAT,LON[,ALT_M]` (repeatable, any source kind: a
     Kiwi whose status is missing or lies, another node's source, a VRT stream; env- and
     unit-friendly, and no clash with a `/PASSWORD` tail, so `--source-kiwi` is unchanged and a
     password keeps its rules). It is kept in `NodeConfig.source_positions`; latitude must be in
     -90..=90 and longitude in -180..=180, and an id no source flag defines is refused, both at
     startup naming the flag. It is the position with `from: "configured"` where the port
     reports none, and shows in `source_list` before the first connection.
  The status wins when both exist (the operator's words: read from the Kiwi where available, else
  configured). The node's own position stays the station (`station_set`); a source with no
  position from either input is a remote source without a vantage when `Capabilities.remote`.
- **What uses it.** The origin query's vantage (4.1). The map's `map-source` marker and the
  bearings of a lit set from that source (4.4). The jobs pane's distance and bearing of a
  positioned job (`map.rowText`) stay from the page's home: a sonde heard through a Kiwi is still
  "that far from me". Plan 30 reads `SourceInfo.position` for nothing more than reporting which
  Kiwi heard an HFDL frame (its events carry no coordinates; provenance names the `source`).
- **The agent.** `AGENTS.md` gains the `--source-position` flag line and a sentence: a source
  reports its position in `source_list.position` when its status has one or the operator gave
  one; `origin_lookup` on a Kiwi's job is cut and drawn from there, so "where is that coming
  from" on HF is relative to the Kiwi, not the station.
- **What is not built.** The Kiwi's GPS *time* (the `SND` frames carry GPS seconds; `proto.rs`
  reads them) is not used for anything new. Only the Kiwi reads a position from its receiver
  (non-goals); every other kind has one only from `--source-position`.

### 4.12 The RF thumbnail (stretch, droppable)

When `main` is `map`, the RF view shrinks to a small live window in a corner of the map: a
`canvas#rf-thumb` (class `rf-thumb`, `--rf-thumb-w` and `--rf-thumb-h` theme tokens, the core
defaults 240 × 120 px, bottom-left over the map, above the attribution) that `waterfall.js`
paints with `drawImage` of its own waterfall canvas, scaled to fit, at most twice a second, only
while `main` is `map`. A click on it sets `main rf`. It is not the waterfall (no axis, no
outlines, no pointer handling beyond the click), its title names the shown source and its band,
and it is hidden with no painting cost while `main` is `rf`. No page or plan has a thumbnail
mechanism to reuse (none of the specs mentions one), so this is the minimal shape. The builder
may drop it (the plan's last task); dropping it changes nothing else, and `TODO.md` then keeps
the idea in the layout gist.

## 5. Docs, TODO and UAT

- `AGENTS.md` and `skills/zetasdr/SKILL.md` regenerate from `agents_md.rs`: an `origin_lookup`
  note (the two forms, what `modulation` null reads, the record's fields and what `score`, `agree`
  and `from` mean, that a lit site is a hypothesis), the `origin` protocol in the events list, the
  `view_set` keys `main`, `dock` and `origin_fade_s`, the `map` alias, the `--source-position`
  flag and `source_list.position`.
- `docs/agents/field-craft.md`: the register paragraph (section 4.9's sentences); the page
  paragraph (the Map tab beside the radios, `view set main map|rf`, `map on|off` as the alias,
  the dock, lit sites and what they mean, a Kiwi's position).
- `docs/guide.md`: the view-policy table (`main`, `map` as alias, `dock`, `origin_fade_s`), the
  map paragraph (the Map tab replaces the RF view; a source tab brings it back), `zeta origin
  lookup`, `--source-position`.
- `docs/themes.md`: `layout.dock`, `html[data-dock]`, `html[data-main]`, `section.pane.dock`,
  `--dock-share`, `--rf-thumb-w/h` (when built).
- `TODO.md`: fold "UI: the Subjects list must not take over the page" (done by the dock) and the
  map half of "UI: state-stream decoders beyond the first subjects"; add a sentence to
  "Architecture: fan-out, and the transcriber as part of its demodulator's graph (towards a
  ComfyUI-style editor)" recording the layout direction (section 2) and that `main` and `dock`
  are its first switches; add the leftovers: positions for node and VRT sources, two strip panes
  at once, a resizable dock, fuzzy name matching, demotion rules, the thumbnail if dropped.
- `UAT.md`: a section "Plan 32: the map layer" with the live checks of section 6.4.

## 6. Testing

### 6.1 Node (`crates/zeta-core`)

- `origin.rs` unit tests over synthetic `Allocation`s: the frequency gate; the width grades
  (0.4/0.2/0/0.1); the modulation term against each `DESIGNATOR_LABELS` letter, `fits`,
  `disagrees`, `unknown` (no designator, empty labels); the reach term with and without a
  distance; receivers and plan rows counted, not scored; dedup by `(licence_no, site,
  center_hz)`; the cap at 50 best first then nearest; a confirmation sets score 1 and keeps the
  terms; the summary's three forms; `vantage_for` on a local source (the station), a remote
  source with a position (`kind: source`) and one without (`null`, the note).
- `knowledge`: a provider's default `allocations_from` and `reach_from` call `allocations` and
  `reach`; a test provider records the `from` it was given.
- `ops`: `origin_lookup {job}` on a decoder job reads the row's labels, on `fm_demod` reads `fm`,
  on a `bits` job reads the chain's demodulator and the capture's band, on `mix` is refused
  `invalid`; the band form with and without `modulation`; a remote source without a position
  answers `from` null and `from_note` `remote source without a position`; a remote source with a
  position (a test port whose `position` is set) answers `from.kind` `source`; with no provider
  `providers` is empty and `origins` empty; the answer is in `job_context.origins` and one
  `origin` frame is on the job's stream and the source's feed. `Allocation` with `callsign`
  round-trips and an older record without it loads. `source_list` carries `position` (`null` for
  a file source; the test port's value when set) and the schema requires it.
- `identify` and `watch` (`--features test-fixtures`): a run publishes one `origin` event after
  its lookup and its verdict carries `origins`; the watch's `unknown` event carries `origins`
  beside `allocations`. Poll for the end state (the CI-only flake rule).
- Identity: a job with the APRS row and an `origins` context sees `src: "XX4EX-7"`; the origin
  with `callsign: "XX4EX"` is confirmed and republished; `src: "XX4ZZ"` changes nothing.
- `themes`: `layout.dock` parses `bottom` and `right`, is `bottom` when absent, and `"left"` is
  refused naming `layout.dock`.
- Schema: `schema` lists `origin_lookup` and the `origins` result; `agents-md --check` and
  `--skill --check` pass.

### 6.2 Provider and sources (`crates/zeta-ext`, `crates/zeta-source-kiwi`, `crates/zeta`)

- `zeta-ext/tests/acma.rs`: the fixed SQL names `CALL_SIGN`; a stub answer with the column fills
  `callsign`, one without it leaves `null`; the stub's columns list is extended;
  `allocations_from` with a station other than the closure's cuts and measures from it, and with
  `None` gives no box and null distances.
- `zeta-source-kiwi`: `status::parse` on a fixture text (a `gps=(-27.40, 153.10)` line among
  twenty others, `asl=12`, `gps_good=9`) gives the position and altitude; `gps=(0, 0)`, a missing
  line and an out-of-range pair give none; the session's status fetch against a one-shot HTTP
  stub (a `TcpListener` answering one `GET /status`) fills `Status.position` with `from:
  "status"`; a stub that refuses, or that accepts and never answers, leaves the position as it was
  (`None` from the port, so the node shows the configured one), counts `status_errors`, delays
  the tune by nothing and sets no fault; `KiwiSource::position` reports it. The fake Kiwi (`testing.rs`) is
  unchanged; the status fetch is given its own URL (`KiwiConfig.status_url`, derived from
  `host:port` by default) so the fake's tests point it at the stub or at nothing.
- `zeta/src/sources.rs` and `zeta/tests/cli.rs`: `--source-position k=-27.3842,153.1175,4` parses
  with `alt_m` 4 and `k=-27.3842,153.1175` with `alt_m` null; `k=-91,0`, `k=0` and a value that is
  not a number are refused naming the flag; an id no source flag defines is refused at startup
  (`--source-position: unknown source <id>`); `source_list` reports `from: "configured"` for a
  configured source and `null` for the rest; a status position wins over a configured one.

### 6.3 Page (`crates/zeta-ui/tests/e2e/ui.py`)

The harness (`crates/zeta/tests/ui.rs`) gains a register stub: the axum stub of
`zeta-ext/tests/acma.rs` moved to a shared test helper and run in-process on a loopback port,
passed as `--knowledge-acma http://127.0.0.1:<port>`, answering three synthetic rows for the tone
source's band (a `tx` `16K0F1D` 9 km north-west of the demo station with callsign `XX4EX`, a `tx`
`12K5A3E` 20 km east, an `rx` row) and nothing elsewhere; the node's station is the demo station
(`station_set` once at start). About a minute of new scenarios, each with its own timeout and
each restoring `view reset` in a `finally`:

- `scenario_map_is_a_source_tab`: the strip `#tabs` ends with `.src-tab.map-tab`; `#map-toggle`
  is absent; a click on it sets `data-main="map"`, `#map` is visible and `#waterfall` is not,
  `view show` lists `main map` and `map auto`; the tone's tab keeps class `kept` and `shown()`
  (through `window.zetaTest`) is still the tone; the waterfall's zoom (`ticks`) before and after
  the round trip is identical and the page's spectrum job id is the same; a click on the tone's
  tab returns to `data-main="rf"`; `view set map on` and `off` drive the same; the narrow
  footprint shows the map from the tab. The three existing map scenarios and the theme-shots
  scenario switch from `view set map on` to `view set main map` (or the tab) and keep their
  assertions; `THEME_HOOKS` gains `.map-tab`, `html[data-main]`, `html[data-dock]`.
- `scenario_origins_light_sites`: `fake_pos` runs; the row's `Origins` button lights two
  `path.map-site` (one `data-agree="fits"` with a `map-bearing`, one `disagrees` with none, the
  `rx` row unlit), the tooltip has licensee, site, designator and score, the popup has the licence
  line and **no button**, the transcript has the `origin` summary, the Map tab's badge reads `2`;
  `view set origin_fade_s 2` and the sites go within a few seconds of the job's last event;
  stopping the job drops them at once.
- `scenario_no_station_lights_without_bearings`: with `station_clear`, the same lookup lights the
  sites with no `map-bearing`, no distance line, and the corner note; `station_set` restores.
- `scenario_dock_right`: with Subjects selected, `view set dock right` puts `#pane` inside
  `#side section.pane.dock` between the transcript and the jobs sections, its head reading
  `Subjects`, `#waterfall`'s height the whole of `#main`; a click on the head folds it
  (`data-collapsed`) and a second unfolds; `bottom` puts `#pane` back under; a test theme with
  `layout.dock: right` and `dock auto` resolves right, and the console theme bottom; the narrow
  footprint ignores it.
- `scenario_classify_lights_sites`: What is this on a box over the tone lights sites from the
  classification's `origins` with a one-shot key that fades from the lookup.
- The agent's `view_set` with `main`, `dock` and `origin_fade_s` applies (the existing agent-view
  scenario's key list).
- The thumbnail, when built, joins `scenario_map_is_a_source_tab`: `#rf-thumb` visible with the
  map, hidden with the RF view, a click on it returns to `rf`.

No e2e for the Kiwi position: the harness has no Kiwi and must not gain the operator's. The
position's path is covered by the node and crate tests of 6.1 and 6.2 and the live check of 6.4.

### 6.4 Live (UAT, the field host)

1. With the register attached and a watch on 400-406 MHz, an `unknown` lights the sites the
   `register says` line names, bearings from the station, the fitting one brightest.
2. `Origins` on a running ACARS decoder lights the airport's SITA assignments as `fits`.
3. `Origins` on a CB transcriber at 477 MHz lights nothing or only `disagrees` rows (CB is a class
   licence): the summary says so.
4. The Map tab: with the CB plan running, select Map; the ribbons' feed keeps the Jobs pane and
   the badge current; select the B210's tab; the waterfall is as it was (zoom, pins, outlines).
5. `view set dock right` with the Subjects tab during an ADS-B run; the table reads in the
   column; fold and unfold it; the Transcript and Jobs panes behave as before.
6. On the local KiwiSDR (added to the operator's node by the operator; `source_list` shows its
   `position`, `from: status` or `configured`), a lookup on an HF broadcaster lights sites cut to
   the Kiwi's reach, with the `map-source` marker at the Kiwi and bearings from it.
7. An APRS run with a lit repeater site: a decoded `src` equal to its callsign confirms it (only if
   the register's `CALL_SIGN` is populated for it; otherwise note the finding).
8. The thumbnail, when built: the live RF view in the map's corner, a click returns.

## 7. Left to the plan

- The exact markup and CSS of the Map tab (`kept`, the badge), the site markers, the tooltip, the
  popup, the corner note, the source marker, the docked section and the thumbnail; the names of
  the scoring constants.
- How the page keys and stores lit sets (a `Map` in `MapPane` beside `tracks`), and how `agent.js`
  hands the squelch activity to the fade clock.
- The shared register stub's module and how `ui.rs` runs it (an in-process axum task is the
  intent; a `--knowledge-allocations FILE` flag feeding `StaticKnowledge::from_files`'s allocations
  is the fallback if the stub proves awkward in the harness).
- The `identity` block's schema in `modes/row.rs`, and where the node runs the match (a per-job
  event watcher shaped as `tee_transcripts`, or a small `origin_confirm` stage).
- The exact `origin_lookup` CLI words and the `--modulation` list parsing.
- The status reader's HTTP client (the crate already depends on `tokio-tungstenite`, which brings
  no HTTP client; a hand-written `GET` over `TcpStream` is enough for one line-oriented document,
  and avoids a new dependency).
- Whether the Explore table's licensee column reads from `origins` (best) or stays on
  `allocations` (nearest).

## 8. Open questions already answered (folded in)

- *Does the page score?* No: the node scores, the page draws (anti-goal). This is why `classify`
  and `capture_classify` return `origins` rather than the page reusing `allocations`.
- *Where does a job's modulation come from without a measurement?* Its row, its kind, its chain
  (4.2); the op never measures.
- *What is "quiet"?* The job's frames on the feed, or the lookup time for a one-shot (4.4).
- *Which band does a decoder start on?* Not from the map at all (item 12); from the box menu, the
  measured band as today.
- *What about Kiwi sources?* The Kiwi's own position is the vantage (4.11); a Kiwi without one is
  a remote source without a vantage (4.8).
- *How does a provider take a vantage?* A per-call parameter (`allocations_from`), since the
  station lives inside the `acma` provider, not in the trait (section 3 finding).
- *Does the fit test differ from identify's?* Same tests, graded (4.1); identify's candidate list
  and `allocation_term` are untouched.
- *Where does the right dock live?* In `#side`, as a third section, not a second pane host (4.7).

## 9. The operator's decisions (2026-11-06)

The reviewer's six questions went to the operator with the sprint's other questions; the
answers, numbered as in the answers file, and what each changed in this document:

- **2. Dock and map layout.** Subjects may sit underneath (as today) or move into the right-hand
  column with the Transcript and Jobs panes and stack there as another collapsible section. The
  Map is a tab along the RF sources (the sources strip `#tabs`), and when selected it replaces the
  RF view in the main area; "maybe the RF view could shrink to a smaller window somewhere" (a
  small live thumbnail: a droppable stretch item, the builder's call). Direction, not this plan's
  scope: a ComfyUI-style layout with operator flexibility. *Changed:* section 1, decisions 7-9,
  the goals, 4.4 rewritten around the sources strip and the `main` policy, 4.7 rewritten around
  `#side`, 4.12 added, the ComfyUI paragraph in the non-goals, the e2e scenarios, UAT 4, 5 and 8.
- **10. The Kiwi's position.** Yes, now: each KiwiSDR source carries its own position (read from
  the Kiwi's status/GPS where available, else configured), and signals received via it are
  placed, cut and drawn from the Kiwi's position, not the station's. Plan 30 shares the definition (4.11) and, whichever lands second, consumes it.
  The operator adds their own Kiwi to their node themselves; its host name stays private. *Changed:*
  decision 10, the goals, the non-goal removed, 4.1's vantage and `allocations_from`, 4.8, 4.11
  added, 6.1-6.2, UAT 6, the anti-goal on host names.
- **11. Demotion.** Not understood by the operator; the default stands: confirm-only. *Changed:*
  4.6 states it without the question.
- **12. A map click.** Shows the assignment only; no decoder start from the map. *Changed:* 4.5
  reduced to the informational popup, the Decode, Listen and Look it up buttons removed, the
  anti-goal reworded, `scenario_origins_light_sites` asserts no button.
- **13. The fade.** 300 s is fine; tweakable later via the view policy or a job param. *Changed:*
  nothing but the citation in 4.4 (`origin_fade_s`, default 300, is the view policy).
- **14. The Map tab.** Always present. *Changed:* 4.4 says so without the question.

## 10. Calls made in this document (check these while reading)

- The Map tab is a fixed last tab of the sources strip driven by a `main: rf|map` policy;
  `aria-selected` moves to it and the shown source's tab keeps a `kept` class; the shown source,
  its waterfall state and its spectrum job are untouched while the map is shown; the page never
  switches to the map on its own (a badge counts lit sites instead).
- The right dock is a DOM move of `#pane` into a third `section.pane.dock` in `#side`, folding
  like its neighbours with a page-memory `data-collapsed`; the `side` policy's words are
  unchanged. The section's share of the column is a theme token (`--dock-share`, 40%), not
  resizable in this plan.
- `dock auto` resolves to the theme's `layout.dock`: the first view policy with a theme default,
  on the `plan auto` precedent. The theme does not override an operator's explicit `bottom|right`.
- The `map` policy stays as an alias (now of `main`) with a new default `auto`, rather than being
  removed: stored values, the agent's `view_set map`, the guide and the field craft keep working.
- The vantage is a per-call parameter of the provider trait (`allocations_from`, `reach_from`,
  defaults to today's methods); a source position is wrapped as a `Station` with the default
  antennas so the register's reach rule applies unchanged.
- A source's position comes from the Kiwi's `/status` first and the generic `--source-position`
  flag second, as the operator said; a source with neither is a remote source without a vantage.
  The status is fetched once per connection with a hand-written `GET`; `--source-kiwi` is
  unchanged (no comma tail, so the password rules stand).
- `Port::position` is on the trait with a `None` default and only the Kiwi fills it; the jobs
  pane's distances stay from the page's home.
- Scoring grades `allocation_match`'s own tests (width 0.4, modulation 0.4, reach 0.2) and adds
  nothing new; a confirmation is score 1. Receivers are never origins. The cap is 50, apart from
  identify's 5.
- `origin_lookup` never measures; `classify` does, and so returns `origins`. A job's modulation
  is read from its row, kind or chain.
- The `origin` event goes on the job's stream and `job_context.origins` keeps the last answer;
  a band lookup has neither.
- The popup is informational; the register row's JSON reaches the agent through the event and the
  context, not through a page button.
- Identity matching is exact callsign only, declared on the row (`subject.identity`), run on the
  node, confirm-only. Only `aprs_decode` gets the field now; `CALL_SIGN` is to be confirmed
  against the register server, and the field is `null` when absent.
- Lit sites fade on the job's activity with `origin_fade_s` (default 300) as a view policy, not a
  constant; one-shot sets fade from their lookup.
- The thumbnail is a scaled `drawImage` of the waterfall's canvas, at most twice a second, click
  to return; droppable, last in the plan.
- The e2e's register is an in-process stub of the real MCP service with synthetic rows, so the
  provider's SQL and parsing run in the suite; the alternative static-file flag is the fallback.
  The e2e has no Kiwi.
- No real register extract in the repository; synthetic rows around the demo station only; no
  Kiwi host name anywhere tracked.
- Tiles, CSP and the Leaflet vendoring are unchanged.
