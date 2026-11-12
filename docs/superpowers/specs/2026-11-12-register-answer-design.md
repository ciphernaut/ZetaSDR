# Plan 37, the register answer: one licensed-environment answer per signal, shared by What is this?, Identify and the map

Design spec, 2026-11-12. From the operator's question of the same day ("what other query types
can bring value to our platform?") and the conversation that followed, settled as: one answer
from the licence register per signal, built once by the register service, shown by "What is
this?", used to order Identify's decoders, and plotted on a map whose sites can be selected
the way subjects are. Section 7 records the calls made here that the operator did not make.

Builds on the knowledge provider (plan 19.1, `docs/superpowers/specs/2026-11-02-layer4-explore-design.md`
5.3, `crates/zeta-ext/src/acma.rs`), the origin query and the map's lit sites (plan 32,
`docs/superpowers/specs/2026-11-06-map-layer-design.md`, `crates/zeta-core/src/node/origin.rs`,
`crates/zeta-ui/web/map.js`), ground stations (plan 35), subjects (`crates/zeta-ui/web/subjects.js`)
and reach (`crates/zeta-core/src/reach.rs`).

The register service is the operator's `acma-local` project (TypeScript, MCP, a local mirror of
the Australian licence register and spectrum plan). It is a separate repository. This plan
adds nothing to it: the answer is assembled from the calls the service already offers, through
named recipes held here as data. A function is added to the service only when it would serve
its other clients too, and none is in this plan.

## 1. Intent and why

An operator finds a signal they do not recognise. The register can say a great deal about it:
what the band is for, who is licensed there within radio range, from which site, with what
antenna pointed where, what kind of emission to expect and how strong, and what else belongs to
the same system. Today the node asks the register one narrow question (devices overlapping a
frequency inside a box, nearest 50) and three menu actions each use a little of the answer:

- "What is this?" nudges a decoder's score when a licence fits.
- "Origins" lists licensed sites and lights them on the map.
- "Identify (thorough)" adds a "register says" line, and a decoder whose row fits a licensed
  transmitter gets one flat bonus (`allocation_term`, `ALLOCATION_SCORE`), the same for the
  nearest strong site and a distant weak one.

The operator cannot tell these three apart, and none of them shows the band's purpose, the
emission in words, the partner frequency or the network. The range the node searches is a fixed
radius per band: station altitude, antenna height and the far site's height are stored and
never used, so a station on a hill or in an aircraft gets the same candidates as one in a
valley.

This is a framework, not a fix for one station. The node runs at any position in the region a
register covers, on any radio, at any altitude, and later on the move. Nothing here may assume
one station's place, one radio's tuning range or one band's ceiling.

## 2. Goals, non-goals, anti-goals

**Goals**

1. One register answer per signal, assembled from the register service's existing calls by
   named recipes, from: the signal's frequency and measured width, and the observer's position,
   altitude and antenna height.
2. The answer holds the band's purpose, ranked candidate transmitters with geometry and
   expected strength, what each belongs to, and the date of the register data.
3. Range comes from a propagation estimate that uses both ends' heights, with every assumption
   a named, overridable input that is echoed back in the answer.
4. "What is this?" shows the answer and plots its sites on the map. The separate "Origins" menu
   item goes away; nothing the operator can see today is lost.
5. Identify orders its decoders from the answer.
6. A verdict in plain words: agrees, disagrees, nothing licensed, or register unreachable.
7. Register sites on the map can be selected like subjects, and selection is shared: the map,
   the Subjects pane and the waterfall all follow it, in both directions.

**Non-goals (recorded in section 6 as later stages)**

- A moving position or a track. The observer is one position per lookup.
- Terrain.
- Register labels on scan rows, or changes to how a watch places decoders.
- A context menu on map sites.
- Licence condition text, change history, fleet questions, calibration references, training
  labels, publishing observations outwards, registers of other regulators.

**Anti-goals: the build has failed if any of these is true**

- The register filters what Identify tries. It may only reorder. A transmission may be
  unlicensed, or licensed and not behaving as its licence says; both must still be found.
- "Nothing licensed" and "the register could not be reached" look the same anywhere.
- A frequency ceiling, a floor, a region or a station position is written into the code or the
  tool as a constant.
- The plan needs a change to the register service before it works.
- A recipe is written into Rust code rather than held as data.
- Anything specific to this node enters the register service.

## 3. The parts

### 3.1 Recipes: the answer from the service's existing calls

The register service already offers a read-only SQL call, a spectrum-plan lookup, an emission
decoder, a site lookup and a schema description. Every part of the answer was probed through
them on the live register. A **recipe** is one named, parameterised use of those calls for one
common case.

- **Naming.** In files, code and operator text a recipe is called a *register query*
  (`registers/`, `zeta registers check`), because `RECIPES.md` already uses "recipe" for a node
  setup. This document keeps the word the design was settled in.
- **Recipes are data.** Like decoder modes, they are rows in files in this repository,
  embedded in the node, with the operator's own beside them. A recipe names its provider, the
  call it makes, its query text with named numeric parameters, its row cap, and how its columns
  map onto the answer. A new case is a new row, not a new build.
- **Only numbers are substituted** into a query (frequencies, widths, coordinates, bounds,
  ids the register itself returned). No text from a decode or from the operator ever enters one.
- **The first recipes**, one per common case:
  - `candidates`: devices whose licensed band overlaps the signal's, inside a bound, with the
    device, antenna, site, licence and holder columns of the answer;
  - `area_devices`: devices licensed for an area with no site;
  - `plan_span`: every spectrum-plan segment a band overlaps, with services and footnote text;
  - `licence_related`: the other frequencies on a licence, by role (a repeater's input and
    output);
  - `site_channels`: a holder's channels at a site, and all channels at a site;
  - `link_far_end`: the other end of a point-to-point link.
- **A recipe that fails** (a column the service's schema lacks, a timeout) fails by name in the
  answer's status; the rest of the answer still stands. The one-off `CALL_SIGN` probe in today's
  caller becomes this general rule.
- **Sharing.** Nothing specific to this node enters the service. A recipe that proves itself can
  later be offered to the service's own sample-query catalogue, where any client can use it;
  that is the operator's decision and not part of this plan.

**The answer the recipes fill**

- `as_of`: the register data's date, read from the service's health report.
- `plan`: every spectrum-plan segment the signal's band overlaps, each with its services
  (name, primary or secondary, qualifier) and the text of its footnotes.
- `candidates`: ranked transmitters and receivers whose licensed band overlaps the signal's,
  each with:
  - `device`: frequency, bandwidth, the emission designator raw and decoded, role (transmit or
    receive), power and EIRP in watts, antenna gain, azimuth, horizontal beamwidth,
    front-to-back ratio, height, tilt and polarisation where held;
  - `site`: id, name, position, the register's stated precision, elevation where held;
  - `licence`: number, service and subservice, status, dates;
  - `holder`: licensee, trading name, industry, client type;
  - `geometry`: distance and bearing from the observer, the angle off the device's beam, the
    radio horizon for the pair, and whether the path is beyond it;
  - `expected`: the propagation mode assumed, an estimated level at the observer in dBm into
    an isotropic receive antenna (the observer's own antenna gain is applied separately), and a
    list of what was unknown and defaulted;
  - `related`: other frequencies on the same licence paired by role, the holder's other
    channels at the same site (a count and a capped list), the far end of a link, call sign,
    class of station, nature of service, hours of operation. `related` is fetched for the top
    candidates only, not for every row.
- `area_candidates`: devices licensed for an area with no site. They have no geometry and no
  expected level; they are listed with their area description so "a mobile under an area
  licence" can be said.
- `matched`, `truncated`: how many devices matched before the cap, and whether the cap cut any.
  The cap applies after duplicates are merged (today's 200-row cap applies before, which loses
  sites).
- `assumptions`: the propagation variables as used, defaults and overrides alike.

With no station set there is no observer: the answer has `plan` and candidates with no geometry
and no expected level, ordered by distance in frequency, as today.

Candidates are ranked by expected level, strongest first; a candidate with no estimate ranks
after those with one, by distance.

### 3.2 Propagation: a first model whose every assumption is an input

The node computes, in `zeta-core` beside `reach.rs`: it holds the observer, and the recipes
bring the far end's heights, power and antenna. It is one pure function of the observer, a
candidate and the variables, so any provider's candidates can pass through it.

- **Line of sight.** Radio horizon from the two heights above mean sea level
  (`d_km = 4.12 * (sqrt(h_observer_m) + sqrt(h_site_m))` under the standard 4/3 earth; the
  factor is the variable `k_factor`). Free-space loss inside it.
- **Beyond the horizon.** The candidate is kept and flagged, with an added loss per kilometre
  past the horizon (`beyond_horizon_db_per_km`). This is a placeholder for diffraction and
  ducting, named as one.
- **Antenna pattern.** Inside half the horizontal beamwidth of the device's azimuth: full gain.
  Outside: gain less the front-to-back ratio. A device with no azimuth is omnidirectional.
- **Ground wave and skywave.** Below `skywave_min_hz` the path is ground wave with its own
  range variable. Between `skywave_min_hz` and `skywave_max_hz` the path may be skywave: range
  is unbounded and no level is estimated; the candidate says so.
- **Heights.** A site's height is its elevation plus the device's antenna height. Elevation is
  missing for most sites: the device height alone is then used and the candidate's `expected`
  lists the elevation as unknown.
- **Search bound.** The search reaches to the furthest distance any mode allows for this
  observer and frequency, with a margin (`search_margin`), or to the operator's `reach_km`
  override when given. The far site's height is not known before the query, so the bound
  assumes a tall one (`assumed_site_height_m`). There is no band-dependent constant and no
  frequency ceiling or floor.

- **In range.** A candidate is in range when its path is inside the horizon, or beyond it with
  an expected level above the variable `in_range_floor_dbm`. On a skywave path every candidate
  is `possible` rather than in range: the estimate cannot say more, and says that.

Every variable has a plain default, can be overridden in the provider's configuration, and is
echoed in `assumptions`. The estimate ranks candidates; it is not a prediction to quote. The node's and
the page's wording must say "expected about" and never state a level as fact.

### 3.3 The node: a neutral answer, an honest status

- **The answer type.** `zeta-core` gains a regulator-neutral register answer (the shape of
  3.1) on the `KnowledgeProvider` contract. A provider declares the regions it covers; the
  station's region picks the provider and its recipes. The `acma` provider stops being
  hard-wired to one region code in its caller.
- **The caller.** `crates/zeta-ext/src/acma.rs` stops holding one query template and becomes
  a runner of recipes over the service's existing calls. The service's emission decoder is not
  called per row (one round trip each); the node's own parser decodes designators.
- **Status.** Every answer carries `status`: `ok`, `unreachable` (with the reason), or
  `partial` (naming the recipes that failed). Callers and the page show it. An unreachable register never
  yields an empty `ok`.
- **The observer.** The node uses the station's position and `alt_m` and the height of the
  antenna that serves the frequency (`station.rs` `antenna_for`). A remote source's vantage is
  its own position, as origins already has it. The default antenna table's upper frequency
  bound is removed: the default for everything above HF has no ceiling.
- **Range.** The propagation estimate of 3.2 replaces `reach.rs`'s fixed table wherever a
  candidate brings the data for it. The table remains only as the fallback for a candidate or a
  provider that brings none.
- **What stays in the node from today's reach.** The station side is the node's knowledge, so
  it stays here, applied on top of the service's ranking:
  - the station antenna's azimuth still favours candidates it points at and disfavours those
    behind it (`km_toward`'s weighting, applied to the rank rather than to a radius);
  - the space class and its "may be a spacecraft" note still come from the plan's satellite
    services or an antenna elevated above 20 degrees;
  - the operator's `reach_km` override bounds the search;
  - with no station set, there is no observer and no geometry.
- **The wire shape is additive.** `classify`, the identify verdict and job contexts gain one new
  field holding the register answer and verdict. The existing `allocations` and `origins` fields
  keep their shapes and are filled from the same answer, so persisted job state, the map's site
  reader, `knowledge_lookup` and the agent's notes need no migration. In the origin score the
  `reach` term is graded from the answer's geometry when present (inside the horizon, by
  expected level; beyond it, falling off) and from distance over radius only on the fallback.
- **Level.** The node has an absolute level only from a source flagged calibrated. For such a
  source the verdict has a level term and the page prints "expected about" beside the measured
  level. For any other source there is no level term; the page prints the expected level as the
  register's estimate and the measured signal-to-noise as measured, and does not compare them.
- **The verdict.** Computed in the node, which holds the measurement. Per candidate: does the
  measured width fit the licensed emission, does the measured modulation fit, is the measured
  level within a wide tolerance of the expected one. Overall:
  - `agrees`: a candidate in range fits;
  - `disagrees`: candidates are in range and none fits. The summary names what is licensed
    and what was measured;
  - `unlicensed`: no candidate in range. The summary gives the band's purpose and whether
    area candidates exist;
  - on a skywave band, where candidates are only `possible`: `agrees` and `disagrees` are
    worded as weak ("a licence that fits exists; range cannot be bounded on this band"), and
    `unlicensed` means the register holds no licence overlapping the signal at all;
  - `unknown`: status is `unreachable`.
  This extends the scoring in `node/origin.rs` rather than adding a second scorer.
- **Round trips.** Identify asks the register twice today; with one shared answer it asks once
  per signal, and `classify`, `origin_lookup` and Identify read the same answer.

### 3.4 Operations

- `classify` ("What is this?") returns the register answer and the verdict beside its labels.
- `origin_lookup` stays as an operation, for the agent, the CLI and jobs that persist origins.
  It is served from the same answer. Only the page's menu item is removed.
- `identify`: the flat `ALLOCATION_SCORE` bonus becomes a graded one inside the existing score:
  a mode row that fits a candidate gains more the higher that candidate ranks, up to today's
  bonus and never above it. The measured terms (centre, width, the modulation label) still
  dominate, so a signal that does not behave as its licence says is still tried first as what
  it measures like. Every decoder that would have run still runs. The verdict
  goes in the job's summary, and a decode that contradicts the licence is reported as a
  finding ("licensed as …, decoded as …").
- New fields go in `ops.rs`; their notes for agents go in `crates/zeta/src/agents_md.rs` and
  `docs/agents/field-craft.md`, and `AGENTS.md` and the skill are regenerated.

### 3.5 The page

- **What is this?** The transcript shows, in this order: the verdict line with the register's
  date; the band's purpose; the top candidates (holder, site, distance and bearing, the
  emission in words, "expected about" beside the measured level, and whether the observer is
  inside the beam); what the top candidate belongs to. The sites are plotted on the map as
  origins are now.
- **Origins** leaves the context menu.
- **Shared selection.** The page's selection store holds one selection of a kind: a subject or
  a register site (keyed by provider and site id). The map, the Subjects pane and the waterfall
  subscribe to it.
  - A site marker on the map can be clicked to select it.
  - Selecting a site marks it on the map, shows it in the pane, and marks on the waterfall the
    site's licensed channels that fall inside the visible span, with the partner frequency of
    the selected signal's licence.
  - Selecting a job, a signal or a subject lights the sites its answer matched.
- A new pane view, if one is needed for sites, goes behind its own tab, not into the waterfall.

## 4. Constraints

- **One repository.** The register service is not changed. The plan works against the service
  as it runs today.
- **Public repository.** No real holders, site names, observed frequencies, distances or
  bearings in tracked files, tests or fixtures. Fixtures are synthetic. The demo station is the
  only real position.
- **Generated files.** `AGENTS.md` and `skills/zetasdr/SKILL.md` are regenerated, never edited.
- **Time.** An answer is several calls where today's is two, and must still fit the provider's
  deadline: the recipes that do not depend on each other run together, and `related` is fetched
  for the top candidates only. Span-and-area queries were probed at well under a second each on
  the live register.
- **Tests.** The propagation function alone: an elevated observer sees further than a low one,
  a directional device on and off its beam, a missing elevation, a path beyond the horizon, a
  skywave band. The recipes: each against a fake register service with a synthetic database,
  including a site-less device, a truncated result, a recipe that fails while the others stand,
  and an unreachable service. The verdict's outcomes; Identify's order changing while its set
  does not. The page: an end-to-end scenario for the transcript block and for selecting a site.
  The existing `zeta modes check` pattern is the model for a `recipes check` that runs each
  recipe against the configured service and reports which work.

## 5. Left to the builder

- The recipe file format and the exact field names, within the shape of 3.1.
- Each recipe's SQL, checked with the service's query-plan call so it uses the indexes it has.
- Default values for the propagation variables, each with a one-line reason beside it.
- The level tolerance in the verdict, and how the three per-candidate terms weigh against each
  other within the existing origin score.
- How a site's channels reach the waterfall when a site is selected from the map with no
  answer in hand (the `site_channels` recipe is there for it).
- Whether the Subjects pane lists a selected site inline or under its own heading.

## 6. Later stages, recorded and not built here

1. **Site context menu.** On a map site: any operation that applies, including starting a
   decoder for that site's channel and emission, and, where a directional antenna that covers
   the band is on a rotator, turning it to the site's bearing.
2. **A moving observer.** Position from a live feed, and ranking candidates by how well each
   fits the level measured along the track.
3. **Register labels on scan rows and smarter decoder placement in a watch.**
4. **Licence condition text and "new since" history** in the answer.
5. **Terrain** in the propagation estimate, and live conditions for ducting and skywave.
6. **Fleet questions, calibration references, training labels, publishing observations
   outwards.**
7. **Other regulators' registers and non-regulator sources** behind the same answer type, each
   with its own recipes.
8. **A larger antenna azimuth database** in the register service, as a refinement of the beam
   term. It multiplies the service's footprint many times over, so it is the operator's to
   schedule.
9. **Offering proven recipes to the register service's sample-query catalogue**, and any new
   function there that its other clients would also use.

Nothing in sections 3 and 4 may make these harder: the observer is an input, the answer is
regulator-neutral, and selection is one shared store.

## 7. Calls made here, not by the operator

- Recipes are data files in this repository, in the manner of decoder modes.
- The propagation estimate is computed in the node.
- Terrain is left out of the first build.
- `origin_lookup` stays as an operation; only its menu item goes.
- The node's own emission parser decodes designators; the service's decoder is not called per row.
- The fixed-radius reach table is kept as a fallback rather than deleted.
- `related` is fetched for the top candidates only.
- No job hands the verdict an absolute level yet (a selection is measured as a signal-to-noise
  ratio), so the level term is built and tested but not fed in this plan.
- Recipes are named register queries outside this document.
- The verdict is computed in the node and extends the origin score.
- The default antenna table loses its upper frequency bound in this plan.
- The expected level is in dBm into an isotropic antenna, and is compared with a measurement
  only on a calibrated source.
- "In range" is the horizon or a level floor; skywave candidates are only `possible`.
- The register's weight in Identify is graded and capped at today's flat bonus.
- The new answer is added beside `allocations` and `origins`, which keep their shapes.
- The station antenna's azimuth, the space class and the reach override stay in the node.
