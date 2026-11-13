# Plan 37: The Register Answer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One licensed-environment answer per signal, assembled from the register service's existing calls by data-defined queries, shown by "What is this?", used to order Identify's decoders, and plotted on a map whose sites can be selected like subjects.

**Architecture:** `zeta-core` gains a propagation estimate (a pure function), a regulator-neutral answer type and a registry of register queries held as JSON rows. `zeta-ext`'s `acma` provider stops holding one SQL template and runs those queries over the service's `execute_sql`. The node fetches one answer per signal and derives the existing `allocations` and `origins` shapes from it, so nothing persisted migrates. The page reads the new field and gains one shared selection store.

**Tech Stack:** Rust 1.97 (tokio, serde, schemars, reqwest), plain JavaScript modules and Leaflet in `crates/zeta-ui/web`, Playwright for the page tests, SQLite (via `rusqlite`, test stub only).

**Spec:** `docs/superpowers/specs/2026-11-12-register-answer-design.md`. Read it first. Section numbers below (3.1, 3.2 ...) are the spec's.

**Naming:** the spec's "recipes" are called **register queries** in files, code and operator text, because `RECIPES.md` already uses "recipe" for a node setup. Directory `registers/`, type `RegisterQuery`, CLI `zeta registers check`.

## Global Constraints

- The register service is not changed. Every query goes through its existing `execute_sql` tool; the data date comes from its `GET /health` (`dataAsOf`).
- No frequency ceiling or floor, region, or station position as a constant in code, queries or tests' logic. The observer and every propagation variable are inputs.
- The register may only reorder what Identify tries; the set of decoders tried must not change.
- "Nothing licensed" and "register unreachable" must be distinguishable in every result, summary and page line.
- A register query is a JSON row under `registers/`, never SQL in Rust. Only numbers and validated register ids are substituted into query text.
- Wire shapes are additive: `Allocation`, `OriginAnswer`, `Classification` and job contexts keep every existing field; new fields carry `#[serde(default)]`.
- This repository is public: fixtures are synthetic (made-up holders, sites, licences, call signs); the demo station (-27.3842, 153.1175, 4 m) is the only real position. No real observation in tracked files or commit messages.
- `AGENTS.md` and `skills/zetasdr/SKILL.md` are regenerated (`cargo run -q -p zeta -- agents-md > AGENTS.md`, `... --skill > skills/zetasdr/SKILL.md`), never edited.
- Wording on the page and in summaries: an estimated level is "expected about N dBm", never a fact.
- Builds use a private target directory: `export CARGO_TARGET_DIR=$HOME/.cache/zeta-target/plan37` in every shell. Formatting is checked per crate (`cargo fmt -p <crate> -- --check`): the worktree is nested in the checkout.
- Commit messages: `Plan 37: <what changed>`, roles not host names, no attribution trailer.

## Review Focus

1. **No station set.** A lookup with no observer must still answer: plan and candidates with no geometry and no expected level, ordered by distance in frequency (Task 4's `without_an_observer_candidates_have_no_geometry`).
2. **A register row with holes.** Missing EIRP, power, azimuth, beamwidth, elevation or bandwidth must give an estimate with the gap named in `unknown`, never a panic or a NaN (Task 1's `a_candidate_with_nothing_known_is_estimated_from_defaults_and_says_so`).
3. **One query failing.** A column the service lacks, or a timeout on one query, must leave the others standing and name the failed one (Task 4's `a_failing_query_is_named_and_the_rest_stands`).
4. **State written before this plan.** Job contexts, burst records and SigMF files holding `Allocation` and `OriginAnswer` without the new fields must load (Task 2's `an_allocation_written_before_site_id_loads`).
5. **A hostile or odd register id.** A licence number holding a quote, a semicolon or a space must be refused before any query is sent (Task 2's `an_id_with_a_quote_is_refused`).

---

### Task 1: The propagation estimate

**Files:**
- Create: `crates/zeta-core/src/propagation.rs`
- Modify: `crates/zeta-core/src/lib.rs` (add `pub mod propagation;`), `crates/zeta-core/src/station.rs:58-97` (the default antennas)

**Interfaces:**
- Produces:

```rust
/// Every assumption of the estimate (spec 3.2). `Default` gives the values below.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct Propagation {
    pub k_factor: f64,                 // 4.0 / 3.0   standard atmosphere
    pub beyond_horizon_db_per_km: f64, // 0.5         a placeholder between diffraction and troposcatter
    pub off_beam_default_db: f64,      // 15.0        used when a directional device has no front-to-back ratio
    pub ground_wave_km: f64,           // 300.0       the node's range below skywave_min_hz until now
    pub skywave_min_hz: u64,           // 2_000_000
    pub skywave_max_hz: u64,           // 30_000_000
    pub in_range_floor_dbm: f64,       // -120.0      about the noise floor of a narrow channel
    pub search_margin: f64,            // 1.5
    pub assumed_site_height_m: f64,    // 1500.0      a tall site, for the bound before any row is known
}
pub struct Observer { pub lat: f64, pub lon: f64, pub alt_m: f64, pub antenna_height_m: f64 }
pub struct Emitter {
    pub lat: f64, pub lon: f64, pub freq_hz: u64,
    pub elevation_m: Option<f64>, pub height_m: Option<f64>,
    pub eirp_w: Option<f64>, pub power_w: Option<f64>, pub gain_dbi: Option<f64>,
    pub azimuth_deg: Option<f64>, pub beamwidth_deg: Option<f64>, pub front_to_back_db: Option<f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathMode { LineOfSight, BeyondHorizon, GroundWave, Skywave }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InRange { Yes, No, Possible }
/// Serialize, Deserialize, JsonSchema, deny_unknown_fields; Options written as null.
pub struct Geometry {
    pub distance_km: f64, pub bearing_deg: f64,
    pub off_beam_deg: Option<f64>, pub horizon_km: Option<f64>, pub beyond_horizon: bool,
}
pub struct Expected {
    pub mode: PathMode, pub level_dbm: Option<f64>, pub in_range: InRange,
    /// Names of inputs that were missing: "eirp", "site_elevation", "antenna_height",
    /// "front_to_back", "level" (skywave or nothing to compute from).
    pub unknown: Vec<String>,
}
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64;
pub fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64;
pub fn horizon_km(h1_m: f64, h2_m: f64, k_factor: f64) -> f64;
pub fn estimate(o: &Observer, e: &Emitter, p: &Propagation) -> (Geometry, Expected);
/// The furthest any mode reaches for this observer; None when unbounded (skywave).
pub fn search_km(o: &Observer, freq_hz: u64, p: &Propagation) -> Option<f64>;
```

- [ ] **Step 1: Write the failing tests** in `propagation.rs`'s `tests` module:

```rust
fn obs(alt_m: f64) -> Observer { Observer { lat: -27.3842, lon: 153.1175, alt_m, antenna_height_m: 5.0 } }
fn site(km_north: f64) -> Emitter { /* lat = -27.3842 + km_north / 111.195, same lon, freq 450 MHz,
    elevation 0, height 30, eirp 25 W, no azimuth */ }

#[test] fn the_horizon_is_4_12_root_h_under_the_standard_atmosphere() {
    assert!((horizon_km(100.0, 0.0, 4.0 / 3.0) - 41.2).abs() < 0.1);
    assert!((horizon_km(9.0, 100.0, 4.0 / 3.0) - (12.36 + 41.2)).abs() < 0.2);
}
#[test] fn an_elevated_observer_sees_a_site_a_low_one_does_not() {
    let p = Propagation::default();
    let (g_low, e_low) = estimate(&obs(4.0), &site(120.0), &p);
    let (g_high, e_high) = estimate(&obs(3000.0), &site(120.0), &p);
    assert!(g_low.beyond_horizon && !g_high.beyond_horizon);
    assert_eq!(e_high.mode, PathMode::LineOfSight);
    assert!(e_high.level_dbm.unwrap() > e_low.level_dbm.unwrap());
}
#[test] fn free_space_level_inside_the_horizon() {
    // 25 W EIRP = 43.98 dBm; FSPL(10 km, 450 MHz) = 32.45 + 20 + 53.06 = 105.51 dB
    let (_, e) = estimate(&obs(4.0), &site(10.0), &Propagation::default());
    assert!((e.level_dbm.unwrap() - (43.98 - 105.51)).abs() < 0.2);
    assert_eq!(e.in_range, InRange::Yes);
}
#[test] fn a_directional_device_is_weaker_off_its_beam() {
    // the site is north of the observer, so the observer is on bearing 180 from it
    let on = Emitter { azimuth_deg: Some(180.0), beamwidth_deg: Some(30.0), front_to_back_db: Some(20.0), ..site(10.0) };
    let off = Emitter { azimuth_deg: Some(0.0), ..on.clone() };
    let p = Propagation::default();
    let (g_on, e_on) = estimate(&obs(4.0), &on, &p);
    let (g_off, e_off) = estimate(&obs(4.0), &off, &p);
    assert!(g_on.off_beam_deg.unwrap() < 1.0 && (g_off.off_beam_deg.unwrap() - 180.0).abs() < 1.0);
    assert!((e_on.level_dbm.unwrap() - e_off.level_dbm.unwrap() - 20.0).abs() < 0.01);
}
#[test] fn beyond_the_horizon_the_level_falls_per_km_and_ends_out_of_range() {
    let p = Propagation::default();
    let (g, e) = estimate(&obs(4.0), &site(400.0), &p);
    assert!(g.beyond_horizon);
    assert_eq!(e.mode, PathMode::BeyondHorizon);
    assert_eq!(e.in_range, InRange::No); // 0.5 dB/km over ~370 km past the horizon
}
#[test] fn a_skywave_band_has_no_level_and_is_only_possible() {
    let hf = Emitter { freq_hz: 8_000_000, ..site(2000.0) };
    let (_, e) = estimate(&obs(4.0), &hf, &Propagation::default());
    assert_eq!((e.mode, e.level_dbm, e.in_range), (PathMode::Skywave, None, InRange::Possible));
    assert!(e.unknown.contains(&"level".to_string()));
    assert_eq!(search_km(&obs(4.0), 8_000_000, &Propagation::default()), None);
}
#[test] fn below_the_skywave_band_the_path_is_ground_wave() {
    let mf = Emitter { freq_hz: 500_000, ..site(100.0) };
    let (_, e) = estimate(&obs(4.0), &mf, &Propagation::default());
    assert_eq!(e.mode, PathMode::GroundWave);
    assert_eq!(e.in_range, InRange::Yes); // inside ground_wave_km
    assert_eq!(search_km(&obs(4.0), 500_000, &Propagation::default()), Some(300.0 * 1.5));
}
#[test] fn a_candidate_with_nothing_known_is_estimated_from_defaults_and_says_so() {
    let bare = Emitter { elevation_m: None, height_m: None, eirp_w: None, power_w: None, gain_dbi: None,
        azimuth_deg: Some(0.0), beamwidth_deg: None, front_to_back_db: None, ..site(10.0) };
    let (g, e) = estimate(&obs(4.0), &bare, &Propagation::default());
    assert!(g.distance_km.is_finite() && g.horizon_km.unwrap().is_finite());
    assert_eq!(e.level_dbm, None);
    for k in ["eirp", "site_elevation", "antenna_height"] { assert!(e.unknown.contains(&k.to_string())); }
}
#[test] fn the_search_bound_grows_with_altitude_and_has_no_band_constant() {
    let p = Propagation::default();
    let low = search_km(&obs(4.0), 450_000_000, &p).unwrap();
    let high = search_km(&obs(10_000.0), 450_000_000, &p).unwrap();
    assert!(high > low * 2.0);
    assert_eq!(search_km(&obs(4.0), 40_000_000_000, &p), Some(low)); // any frequency above skywave_max_hz
}
#[test] fn the_default_antenna_above_hf_has_no_ceiling() {
    let s = Station { lat: 0.0, lon: 0.0, alt_m: 0.0, region: "AU".into(), antennas: vec![] };
    assert_eq!(s.antenna_for(Hz(40_000_000_000)).id, "discone");
    assert_eq!(Station::default_antennas(0.0)[1].max_hz, Hz(u64::MAX));
}
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --lib propagation` — expected: does not compile (`propagation` not found).

- [ ] **Step 3: Implement `propagation.rs`.** The rules the signatures do not pin:
  - Observer height = `alt_m + antenna_height_m`. Emitter height = `elevation_m.unwrap_or(0) + height_m.unwrap_or(0)`, naming each missing one in `unknown`. Negative heights clamp to 0.
  - `horizon_km = 3.57 * sqrt(k_factor) * (sqrt(h1) + sqrt(h2))` (4.12 at 4/3).
  - EIRP in dBm = `10 log10(eirp_w * 1000)`; without `eirp_w`, from `power_w` and `gain_dbi` (gain 0 when missing, and `unknown` gains `"eirp"`); with neither, `level_dbm` is `None`.
  - Free-space loss dB = `32.45 + 20 log10(km) + 20 log10(MHz)`, distance floored at 0.01 km.
  - Off-beam: with an azimuth, `off_beam_deg` is the angle between it and the bearing from the emitter to the observer; outside `beamwidth_deg / 2` (beamwidth 360 when missing) subtract `front_to_back_db`, else `off_beam_default_db` naming `"front_to_back"`.
  - Mode: `freq < skywave_min_hz` ground wave (`Yes` inside `ground_wave_km`, else `No`; level by free space); `skywave_min_hz..=skywave_max_hz` skywave; above, line of sight inside the horizon, else beyond it with `beyond_horizon_db_per_km * (distance - horizon)` added to the loss.
  - `in_range` above the skywave band: `Yes` inside the horizon; beyond it `Yes` only when `level_dbm >= in_range_floor_dbm`; `No` otherwise (a level of `None` beyond the horizon is `No`).
  - `search_km` above the skywave band: `horizon_km(observer, assumed_site_height_m) * search_margin`; ground wave: `ground_wave_km * search_margin`; skywave: `None`.
  - Move `haversine_km` and `bearing_deg` here from `crates/zeta-ext/src/acma.rs` (re-export there until Task 4 removes the uses).

- [ ] **Step 4: Remove the default discone's ceiling** in `station.rs`: `max_hz: Hz(u64::MAX)`, and its doc comment says "from 30 MHz up". Fix any test in the workspace that asserted `6_000_000_000` (`grep -rn "6_000_000_000" crates`).

- [ ] **Step 5: Run** `cargo test -p zeta-core --lib propagation station reach` — expected: PASS.

- [ ] **Step 6: Commit** `Plan 37: a propagation estimate from both ends' heights, and no ceiling on the default antenna`.

---

### Task 2: The answer type, the register queries as data, and the provider contract

**Files:**
- Create: `crates/zeta-core/src/register.rs`, `registers/acma/` (six files, filled in Task 4; this task adds `registers/acma/candidates.json` only as the loader's first row)
- Modify: `crates/zeta-core/src/lib.rs`, `crates/zeta-core/src/knowledge.rs` (`Allocation`, `KnowledgeProvider`)

**Interfaces:**
- Consumes: `propagation::{Geometry, Expected, Propagation}` (Task 1).
- Produces (all `Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema`, `deny_unknown_fields`, every `Option` written as `null` in the manner of `Allocation`):

```rust
pub struct Signal { pub center_hz: Hz, pub bandwidth_hz: Hz }

pub enum RegisterState { Ok, Partial, Unreachable }            // snake_case
pub struct FailedQuery { pub query: String, pub reason: String }
pub struct RegisterStatus { pub state: RegisterState, pub reason: Option<String>, pub failed: Vec<FailedQuery> }

pub struct PlanService { pub name: String, pub primary: bool, pub qualifier: Option<String> }
pub struct PlanFootnote { pub reference: String, pub text: Option<String> }
pub struct PlanSegment { pub min_hz: Hz, pub max_hz: Hz, pub services: Vec<PlanService>, pub footnotes: Vec<PlanFootnote> }

pub struct Device {
    pub center_hz: Hz, pub bandwidth_hz: Option<Hz>, pub emission: Option<String>,
    pub emission_decoded: Option<Emission>, pub role: Option<String>,          // "tx" | "rx"
    pub power_w: Option<f64>, pub eirp_w: Option<f64>, pub gain_dbi: Option<f64>,
    pub azimuth_deg: Option<f64>, pub beamwidth_deg: Option<f64>, pub front_to_back_db: Option<f64>,
    pub height_m: Option<f64>, pub tilt_deg: Option<f64>, pub polarisation: Option<String>,
    pub callsign: Option<String>, pub station_class: Option<String>,
    pub nature_of_service: Option<String>, pub hours: Option<String>,
}
pub struct Site { pub id: String, pub name: Option<String>, pub lat: f64, pub lon: f64,
                  pub precision: Option<String>, pub elevation_m: Option<f64> }
pub struct Licence { pub number: String, pub service: Option<String>, pub subservice: Option<String>,
                     pub status: Option<String>, pub issued: Option<String>, pub expires: Option<String> }
pub struct Holder { pub name: Option<String>, pub trading_name: Option<String>,
                    pub industry: Option<String>, pub client_type: Option<String> }
pub struct Channel { pub center_hz: Hz, pub bandwidth_hz: Option<Hz>, pub role: Option<String>, pub emission: Option<String> }
pub struct FarEnd { pub site: Site, pub distance_km: Option<f64> }
pub struct Related {
    pub paired: Vec<Channel>,            // other frequencies on the same licence
    pub holder_channels: u32,            // the holder's channels at the site, counted
    pub holder_channel_list: Vec<Channel>, // capped at RELATED_CHANNELS
    pub far_end: Option<FarEnd>,
}
pub struct Candidate {
    pub device: Device, pub site: Site, pub licence: Licence, pub holder: Holder,
    pub geometry: Option<Geometry>, pub expected: Option<Expected>, pub related: Option<Related>,
}
pub struct AreaCandidate { pub device: Device, pub licence: Licence, pub holder: Holder, pub area: Option<String> }

pub enum VerdictKind { Agrees, Disagrees, Unlicensed, Unknown }   // snake_case
pub struct Verdict { pub kind: VerdictKind, pub weak: bool, pub summary: String }

pub struct RegisterAnswer {
    pub provider: String, pub region: String, pub status: RegisterStatus, pub as_of: Option<String>,
    pub plan: Vec<PlanSegment>, pub candidates: Vec<Candidate>, pub area_candidates: Vec<AreaCandidate>,
    pub matched: u32, pub truncated: bool, pub assumptions: Propagation,
    pub verdict: Option<Verdict>,        // filled by the node (Task 5)
}
impl RegisterAnswer {
    pub fn unreachable(provider: &str, region: &str, reason: String) -> Self;
    /// The existing wire rows: one `assignment` per candidate in rank order, then one `plan` row per
    /// plan service, with the same `service`/`note` text the acma provider writes today.
    pub fn allocations(&self) -> Vec<Allocation>;
}
pub const REGISTER_MAX_CANDIDATES: usize = 50;
pub const RELATED_TOP: usize = 3;
pub const RELATED_CHANNELS: usize = 12;

// --- register queries as data ---
pub enum ParamKind { Number, Id }                                   // snake_case
pub struct QueryParam { pub name: String, pub kind: ParamKind }
pub struct RegisterQuery {
    pub id: String, pub provider: String, pub name: String,
    pub call: String,                 // only "execute_sql" is accepted
    pub sql: String,                  // placeholders `{name}`
    pub params: Vec<QueryParam>, pub limit: u64,
    pub columns: Vec<String>,         // the result column aliases the query must return
}
pub enum ParamValue { Number(f64), Int(i64), Id(String) }
pub struct RegisterQueries { /* by (provider, id) */ }
impl RegisterQueries {
    pub fn builtin() -> Self;                                   // include_str! of registers/**.json, like sets.rs
    pub fn with_dir(self, dir: &Path) -> Result<Self, String>;  // operator rows; same (provider, id) replaces
    pub fn get(&self, provider: &str, id: &str) -> Option<&RegisterQuery>;
    pub fn for_provider(&self, provider: &str) -> Vec<&RegisterQuery>;
}
impl RegisterQuery {
    pub fn validate(&self) -> Result<(), String>;   // error names the field, as mode rows do
    pub fn render(&self, values: &BTreeMap<&str, ParamValue>) -> Result<String, String>;
}
```

  `Allocation` gains `#[serde(default)] pub site_id: Option<String>` (schema-required, nullable, like `callsign`).

  `KnowledgeProvider` gains two defaulted methods:

```rust
/// The regions this provider answers for; empty means any.
fn regions(&self) -> Vec<String> { Vec::new() }
/// The register's whole answer for a signal as heard from `from`; None from a provider that has none.
async fn describe(&self, _region: &str, _signal: &Signal, _from: Option<&Station>) -> Option<RegisterAnswer> { None }
```

- [ ] **Step 1: Write the failing tests** in `register.rs`:

```rust
#[test] fn the_builtin_queries_load_and_validate() {
    let q = RegisterQueries::builtin();
    let c = q.get("acma", "candidates").expect("candidates");
    assert_eq!(c.call, "execute_sql");
    c.validate().unwrap();
}
#[test] fn numbers_and_ids_are_substituted_and_nothing_else() {
    let q = RegisterQuery { sql: "SELECT 1 WHERE f BETWEEN {lo} AND {hi} AND l = {licence}".into(),
        params: vec![num("lo"), num("hi"), id("licence")], ..row() };
    let sql = q.render(&values(&[("lo", ParamValue::Int(100)), ("hi", ParamValue::Number(2.5)),
                                 ("licence", ParamValue::Id("9000001/1".into()))])).unwrap();
    assert_eq!(sql, "SELECT 1 WHERE f BETWEEN 100 AND 2.5 AND l = '9000001/1'");
}
#[test] fn an_id_with_a_quote_is_refused() {
    for bad in ["9000001' OR 1=1 --", "a;b", "a b", "", &"x".repeat(65)] {
        assert!(q().render(&values(&[("licence", ParamValue::Id(bad.into()))])).is_err(), "{bad}");
    }
}
#[test] fn a_missing_or_undeclared_parameter_is_an_error() { /* render without "lo"; sql naming {other} fails validate() */ }
#[test] fn a_non_finite_number_is_refused() { /* NaN and infinity */ }
#[test] fn a_query_that_is_not_execute_sql_or_not_a_select_is_invalid() { /* call "sync_data"; sql "DELETE ..." */ }
#[test] fn an_operator_row_replaces_the_builtin_of_the_same_id() { /* tempdir with acma/candidates.json */ }
#[test] fn an_allocation_written_before_site_id_loads() {
    // the JSON of an Allocation exactly as knowledge.rs's allocation_without_callsign test writes it
    let a: Allocation = serde_json::from_str(OLD_ALLOCATION_JSON).unwrap();
    assert_eq!(a.site_id, None);
}
#[test] fn an_unreachable_answer_says_so_and_has_no_rows() {
    let a = RegisterAnswer::unreachable("acma", "AU", "timeout".into());
    assert_eq!(a.status.state, RegisterState::Unreachable);
    assert!(a.candidates.is_empty() && a.allocations().is_empty());
}
#[test] fn allocations_are_the_candidates_in_rank_order_then_the_plan() { /* two candidates, one segment of
    two services: 2 assignment rows (kind, role, site, site_id, lat, lon, distance_km, bearing_deg,
    power_w, center_hz, bandwidth_hz, licence_no, callsign, emission, emission_decoded, as_of) then
    2 plan rows "NAME (primary)" / "NAME (secondary)" */ }
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --lib register` — expected: does not compile.

- [ ] **Step 3: Implement.** Decisions:
  - An `Id` value must match `^[0-9A-Za-z/_.-]{1,64}$` and is rendered in single quotes. A `Number` must be finite; an `Int` renders without a fraction.
  - `validate`: `call == "execute_sql"`; `sql` trimmed starts with `SELECT` or `WITH` (case-insensitive) and holds no `;`; every `{name}` is declared and every declared param is used; `limit` in 1..=5000; `columns` non-empty.
  - `builtin()` embeds each file with `include_str!` in a table, as `crates/zeta-core/src/sets.rs` does; a row that fails `validate` panics in `builtin()` (a build-time data error) and is an `Err` from `with_dir`.
  - `registers/acma/candidates.json` in this task is a valid placeholder row (`"sql": "SELECT 1 AS center_hz"`, `"columns": ["center_hz"]`); Task 4 replaces it.

- [ ] **Step 4: Run** `cargo test -p zeta-core --lib register knowledge` — expected: PASS. Then `cargo build --workspace` — fix every `Allocation { .. }` literal that now lacks `site_id` (add `site_id: None`).

- [ ] **Step 5: Commit** `Plan 37: the register answer type, register queries as data rows, and describe on the provider contract`.

---

### Task 3: A register stub that runs the queries for real

**Files:**
- Modify: `crates/zeta-ext/Cargo.toml` (`rusqlite`, its current release, `features = ["bundled"]`, `optional = true`, added to the `test-fake` feature; MIT, and the bundled SQLite is public domain), `crates/zeta-ext/src/testing/acma_stub.rs`
- Test: `crates/zeta-ext/tests/acma_stub.rs` (new)

**Interfaces:**
- Produces:

```rust
/// A synthetic register in an in-memory SQLite database with the service's table and column names.
pub struct Fixture { /* rusqlite::Connection behind a Mutex */ }
impl Fixture {
    pub fn empty() -> Fixture;                          // every table below, no rows
    pub fn from_rows(rows: &[Value]) -> Fixture;        // the 12-column COLUMNS rows tests use today
    pub fn sql(&self, statement: &str);                 // INSERTs for a test's own rows
    pub fn drop_column(&self, table: &str, column: &str);
}
pub async fn serve_fixture(fixture: Fixture, mode: Mode) -> (String, Arc<Stub>);
// unchanged: serve(rows, mode), serve_on_thread(rows, mode), Mode, Stub's counters, COLUMNS
```

  `Stub` gains `pub health: Mutex<Value>` (default `{"status":"ok","dataAsOf":"2026-01-01T00:00:00.000Z"}`) and `pub fail_matching: Mutex<Option<String>>` (an `execute_sql` whose text contains it answers an error).

  The schema (names exactly as the service's; types as SQLite stores them):
  - `device_details(SDD_ID, LICENCE_NO, FREQUENCY, BANDWIDTH, EMISSION, DEVICE_TYPE, TRANSMITTER_POWER, TRANSMITTER_POWER_UNIT, EIRP, EIRP_UNIT, SITE_ID, ANTENNA_ID, POLARISATION, AZIMUTH, HEIGHT, TILT, CALL_SIGN, CLASS_OF_STATION_CODE, NATURE_OF_SERVICE_ID, HOURS_OF_OPERATION, AREA_DESCRIPTION, STATION_NAME)`
  - `site(SITE_ID, LATITUDE, LONGITUDE, NAME, STATE, POSTCODE, SITE_PRECISION, ELEVATION)`
  - `licence(LICENCE_NO, CLIENT_NO, SV_ID, SS_ID, LICENCE_TYPE_NAME, DATE_ISSUED, DATE_OF_EXPIRY, STATUS, STATUS_TEXT)`
  - `client(CLIENT_NO, LICENCEE, TRADING_NAME, CAT_ID, CLIENT_TYPE_ID)`, `client_type(TYPE_ID, NAME)`, `industry_cat(CAT_ID, DESCRIPTION, NAME)`
  - `licence_service(SV_ID, SV_NAME)`, `licence_subservice(SS_ID, SV_SV_ID, SS_NAME)`
  - `antenna(ANTENNA_ID, GAIN, FRONT_TO_BACK, H_BEAMWIDTH, V_BEAMWIDTH, ANTENNA_TYPE, MODEL)`
  - `class_of_station(CODE, DESCRIPTION)`, `nature_of_service(CODE, DESCRIPTION)`
  - `spectrum_allocations(freq_start_hz, freq_end_hz, unit, page, services_json, footnotes_json, raw)`, `spectrum_australian_footnotes(footnote_ref, footnote_text, page)`, `spectrum_international_footnotes(footnote_ref, footnote_text, page)`
  - view `v_links(id, lat_a, lon_a, label_a, lat_b, lon_b, label_b, site_id_a, site_id_b, frequency_hz, service, licensee)` over a table `link_pairs` of the same columns
  - `Fixture::empty()` seeds one plan segment 403-406 MHz (the stub's existing one) with services `METEOROLOGICAL AIDS` primary and `FIXED` secondary and one footnote.

- [ ] **Step 1: Write the failing tests** in `crates/zeta-ext/tests/acma_stub.rs`: `execute_sql_runs_a_select_on_the_fixture_and_answers_columns_and_rows` (the envelope `{"columns":[..],"rows":[[..]],"rowCount":n,"truncated":bool}` the live service returns, `limit` honoured, `truncated` true when cut); `a_write_statement_is_refused`; `a_dropped_column_makes_the_query_an_error_naming_it`; `health_reports_the_data_date`; `from_rows_gives_each_row_a_site_a_licence_and_a_client`.

- [ ] **Step 2: Run** `cargo test -p zeta-ext --test acma_stub` — expected: does not compile.

- [ ] **Step 3: Implement.** `execute_sql` prepares and runs the statement on the fixture (refuse anything not starting `SELECT`/`WITH`); the existing band-overlap filter and the fixed plan answer go away for `execute_sql` but `get_frequency_allocation` keeps answering from `spectrum_allocations` until Task 4 stops calling it. `from_rows` invents `SITE_ID` (`"S<n>"`), `CLIENT_NO` and an antenna row per input row. Keep `Mode`'s existing variants working (hang, error, bad body).

- [ ] **Step 4: Run** `cargo test -p zeta-ext` — expected: the new file PASSES; `tests/acma.rs` still passes (the provider still sends its old SQL, which the fixture's tables satisfy).

- [ ] **Step 5: Commit** `Plan 37: the register stub answers queries from a synthetic SQLite register`.

---

### Task 4: The six register queries and the provider that runs them

**Files:**
- Create: `registers/acma/{candidates,area_devices,plan_span,licence_related,site_channels,link_far_end}.json`
- Modify: `crates/zeta-ext/src/acma.rs` (most of the file), `crates/zeta-core/src/register.rs` (the embed table)
- Test: `crates/zeta-ext/tests/acma.rs` (rewritten around `serve_fixture`), `crates/zeta-ext/tests/acma_describe.rs` (new)

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: `AcmaKnowledge::new(client, override_km, station, queries: Arc<RegisterQueries>)`; `with_propagation(self, p: Propagation) -> Self`; `impl KnowledgeProvider` with `regions() == ["AU"]`, `describe(..)`, and `allocations_from` / `reach_from` served from `describe`. `pub async fn run_query(&self, id: &str, values: &BTreeMap<&str, ParamValue>) -> Result<Vec<serde_json::Map<String, Value>>, String>` (rows keyed by column alias), used by Task 7.

  **The queries.** Parameters are `Number` unless marked. Each `columns` list is exactly the aliases named:
  - `candidates` — params `lo_hz, hi_hz, lat_min, lat_max, lon_min, lon_max, center_hz`; limit 2000. Devices with a site whose `FREQUENCY ± COALESCE(BANDWIDTH,0)/2` overlaps `lo_hz..hi_hz` inside the box, `ORDER BY ABS(FREQUENCY - center_hz)`. Aliases: `center_hz, bandwidth_hz, emission, device_type, power, power_unit, eirp, eirp_unit, gain_dbi, azimuth_deg, beamwidth_deg, front_to_back_db, height_m, tilt_deg, polarisation, callsign, station_class, nature_of_service, hours, site_id, site_name, lat, lon, site_precision, elevation_m, licence_no, service, subservice, licence_status, issued, expires, licensee, trading_name, industry, client_type`. Left joins for everything but `site`.
  - `area_devices` — params `lo_hz, hi_hz`; limit 200. Same overlap, `SITE_ID IS NULL`. Aliases: the device, licence and holder aliases above plus `area`.
  - `plan_span` — params `lo_hz, hi_hz`; limit 50. Segments overlapping the span, ascending. Aliases: `min_hz, max_hz, services_json, footnotes_json` where `footnotes_json` is a JSON array of `{"ref","text"}` built with `json_each` over the segment's footnote refs against both footnote tables (text `null` when neither holds the ref).
  - `licence_related` — params `licence_no` (Id); limit 50. Aliases: `center_hz, bandwidth_hz, device_type, emission, site_id`.
  - `site_channels` — params `site_id` (Id); limit 500. Aliases: `center_hz, bandwidth_hz, device_type, emission, licence_no, licensee`.
  - `link_far_end` — params `site_id` (Id), `center_hz`; limit 5. From `v_links` where either end is the site and `frequency_hz = center_hz`. Aliases: `far_site_id, far_name, far_lat, far_lon`.

- [ ] **Step 1: Write the failing tests** in `tests/acma_describe.rs` against `serve_fixture`. Fixture: a station at the demo position, four synthetic sites at 5 km, 40 km, 120 km and 600 km north on one channel, one of them directional pointing away, one area-wide device, a licence with a transmit and a receive frequency, one link pair.

```rust
async fn the_answer_holds_the_plan_the_ranked_candidates_and_the_data_date()
    // status Ok; as_of == "2026-01-01T00:00:00.000Z"; plan has the segment with footnote text;
    // candidates ordered by expected.level_dbm descending; each has geometry and expected;
    // assumptions == Propagation::default()
async fn the_600_km_site_is_fetched_only_when_the_observer_is_high_enough()
    // alt 4 m: absent (outside the search box); alt 9000 m: present and inside the horizon
async fn the_top_candidates_carry_what_they_belong_to()
    // related on the first RELATED_TOP only: paired holds the rx frequency, holder_channels counted,
    // far_end names the link's other site; the fourth candidate's related is None
async fn area_devices_are_listed_without_geometry()
async fn duplicates_merge_before_the_cap_and_truncation_is_reported()
    // 60 sites on the channel + each inserted twice: candidates.len() == 50, matched == 120, truncated
async fn without_an_observer_candidates_have_no_geometry()
    // station None: geometry and expected None, ordered by |center - signal centre|, no box in the SQL
async fn a_failing_query_is_named_and_the_rest_stands()
    // stub.fail_matching = "spectrum_allocations": status Partial, failed == [{query: "plan_span", ..}],
    // candidates still present. And fixture.drop_column("site", "ELEVATION"): status Partial naming
    // "candidates", the plan still present
async fn a_service_that_does_not_answer_is_unreachable_not_empty()
    // Mode hang and Mode error: describe() returns Some with state Unreachable and a reason; never None
async fn another_region_is_not_asked()          // describe("NZ", ..) is None and the stub saw no call
async fn the_whole_answer_fits_one_deadline()   // with_timeout(300 ms) on a hanging stub returns within 600 ms
async fn a_station_near_a_pole_or_the_antimeridian_gets_a_finite_box()
    // observers at lat 89.9 and lon 179.9: the rendered SQL's four bounds are finite, lat within ±90,
    // lon within ±180 (read from stub.sql)
async fn the_operator_reach_override_bounds_the_search()   // override_km 10: only the 5 km site
```

  Port `tests/acma.rs` to the new constructor and `serve_fixture`; delete the three `CALL_SIGN` probe tests (the probe is gone) and keep the rest asserting the same `Allocation` rows through `allocations_from`.

- [ ] **Step 2: Run** `cargo test -p zeta-ext --test acma_describe` — expected: does not compile.

- [ ] **Step 3: Write the six query files**, each validated by Task 2's `the_builtin_queries_load_and_validate` extended to all six ids. `EMISSION` is trimmed in SQL. `device_type` stays `T`/`R` (the provider maps it to `tx`/`rx`).

- [ ] **Step 4: Rewrite the provider.**
  - `describe`: refuse a region not in `regions()` (`None`). Build the `Observer` from `from` (`alt_m`, and `antenna_for(center).height_m`); none without a station. Bound = `override_km` else `propagation::search_km`; the box is the bound in degrees (111 km per degree of latitude, longitude divided by `cos(lat)` floored at 0.01), clamped to ±90 and ±180; with no observer or no bound the box is the whole globe.
  - Run `candidates`, `area_devices`, `plan_span` and the health read together (`tokio::join!`) under one `timeout`. Every query failing, or the transport failing, is `Unreachable` with the first error; some failing is `Partial` naming each.
  - Rows to candidates: power and EIRP to watts with the existing `to_watts`; designators through `parse_emission`; merge duplicates on (licence, site id, centre); `propagation::estimate` per candidate when there is an observer; rank by `expected.level_dbm` descending, then candidates with no level by `distance_km`, then by distance in frequency; with an observer drop candidates whose `in_range` is `No` **and** which lie outside the bound; cut to `REGISTER_MAX_CANDIDATES`. `matched` is the row count before merging; `truncated` is `rows == limit || merged > cap`.
  - Then `licence_related`, `site_channels`, `link_far_end` for the first `RELATED_TOP` candidates, all together, inside what is left of the deadline; a failure there is `Partial`, the candidate's `related` stays `None`.
  - The health read is memoised for `ACMA_PLAN_TTL`.
  - `allocations_from` = `describe(..).map(|a| a.allocations())`; `reach_from` = a `Reach` whose `km` is the bound, `class` from `reach_for`, kept so `OriginAnswer.reach` stays filled. The space class and note still come from `plan_span`'s services (`SPACE_SERVICES`) or the antenna's elevation.
  - Delete `assignments_sql`, the `CALL_SIGN` probe, `fetch_plan`/`parse_plan`, `PlanMemo`, `ACMA_SQL_LIMIT`, `KM_PER_DEG_LAT` use sites replaced above, and the now-unused stub `get_frequency_allocation` handler.
  - `fail()`'s once-a-minute log stays.

- [ ] **Step 5: Wire the constructor** in `crates/zeta/src/node_setup.rs` (`attach_acma`): pass `Arc::new(RegisterQueries::builtin())`. Operator rows come in Task 7.

- [ ] **Step 6: Run** `cargo test -p zeta-ext && cargo test -p zeta --test origin_identity` — expected: PASS.

- [ ] **Step 7: Commit** `Plan 37: the acma provider answers a signal from six register queries held as data`.

---

### Task 5: One answer per signal in the node, the verdict, and the wire fields

**Files:**
- Modify: `crates/zeta-core/src/node/origin.rs` (`origins`, `score_row`, new `described`, `verdict`), `crates/zeta-core/src/ops.rs` (`OpResult::Classification`, `classify` at ~6718), `crates/zeta-core/src/classify.rs` (`Classification`), `crates/zeta-core/src/node/identify.rs:560-705`, `crates/zeta-core/src/node/watch.rs:2268` (`register_says`), `crates/zeta/src/agents_md.rs`, `docs/agents/field-craft.md`
- Test: `crates/zeta-core/src/node/origin.rs` tests, `crates/zeta-core/tests/identify.rs`

**Interfaces:**
- Consumes: `KnowledgeProvider::describe`, `RegisterAnswer`, `Verdict`, `InRange`.
- Produces:

```rust
/// What every provider said about a band from a source's vantage, fetched once.
pub struct Described {
    pub vantage: Vantage, pub region: String, pub providers: Vec<String>,
    pub register: Option<RegisterAnswer>,   // the first provider, in order, that describes
    pub rows: Vec<Allocation>,              // every provider's rows (a describing one's via allocations())
    pub reach: Option<Reach>, pub register_rows: Option<u32>,
}
pub async fn described(inner: &Inner, source: &SourceId, center_hz: Hz, bandwidth_hz: Hz) -> Described;
pub fn origins_of(q: &OriginQuery, d: &Described) -> OriginAnswer;       // `origins()` = described + this
pub fn verdict(answer: &RegisterAnswer, origins: &OriginAnswer, measured_dbm: Option<f64>) -> Verdict;
pub const VERDICT_LEVEL_TOLERANCE_DB: f64 = 20.0;
pub const ORIGIN_LEVEL_SPAN_DB: f64 = 60.0;
```

  `score_row` gains a last parameter `expected: Option<(&Geometry, &Expected)>`. `Classification` and `OpResult::Classification` gain `#[serde(default)] register: Option<RegisterAnswer>` (nullable in the schema). The identify verdict JSON gains `"register"`.

- [ ] **Step 1: Write the failing tests.**

```rust
// origin.rs, with a test provider whose describe() returns a canned answer and counts its calls
async fn classify_and_origins_share_one_register_lookup()        // one describe() per classify
async fn the_reach_term_follows_the_expected_level_when_there_is_geometry()
    // in range at floor + 60 dB or more: ORIGIN_REACH_IN; at the floor: ORIGIN_REACH_IN * ORIGIN_REACH_FLOOR;
    // InRange::No: 0.0; InRange::Possible: ORIGIN_REACH_UNKNOWN; no geometry: today's distance-over-reach value
async fn a_station_beam_halves_the_term_of_a_site_behind_it()
    // station antenna azimuth 0, sector SECTOR_DEG: a site on bearing 180 gets half the term of one on 0
async fn a_fitting_candidate_in_range_agrees()
async fn candidates_in_range_that_do_not_fit_disagree_and_the_summary_names_both()
    // summary contains the licensed emission words and the measured label
async fn no_candidate_in_range_is_unlicensed_and_gives_the_band_purpose()
    // summary contains the plan's primary service names; mentions area candidates when there are any
async fn an_unreachable_register_is_unknown_not_unlicensed()
async fn on_a_skywave_band_the_verdict_is_weak()                 // candidates Possible: weak == true
async fn a_measured_level_far_from_the_expected_one_does_not_fit()
    // measured_dbm Some(expected - 40): that candidate is not a fit; None: level is not considered
// tests/identify.rs
async fn the_verdict_carries_the_register_answer_and_its_verdict()
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --lib origin` — expected: does not compile.

- [ ] **Step 3: Implement.**
  - `described`: the body of today's `origins` up to `answer(..)`, calling `describe` on each provider whose `regions()` is empty or holds the region and falling back to `allocations_from` + `reach_from` when it returns `None`. A provider whose `regions()` excludes the region is not asked at all.
  - `score_row` with `expected`: `InRange::No` gives 0; `Possible` gives `ORIGIN_REACH_UNKNOWN`; `Yes` with a level gives `ORIGIN_REACH_IN * (ORIGIN_REACH_FLOOR + (1 - ORIGIN_REACH_FLOOR) * clamp((level - floor) / ORIGIN_LEVEL_SPAN_DB, 0, 1))`; `Yes` without a level gives `ORIGIN_REACH_IN`. Then, when the vantage's antenna has an azimuth and the row's bearing is outside `SECTOR_DEG` of it, halve the term. Round as today. `origins_of` finds a row's candidate by (licence, site id, centre).
  - `verdict`: unreachable status is `Unknown`. A candidate *fits* when its origin's width term is above 0 and its `agree` is not `Disagrees`, and, with `measured_dbm` and an expected level, the two are within `VERDICT_LEVEL_TOLERANCE_DB`. In range means `InRange::Yes`; with no geometry at all (no station) every candidate counts as in range and the verdict is `weak`. `Agrees` when a candidate in range fits; `Disagrees` when some are in range and none fits; else, if any candidate is `Possible`, the same two outcomes with `weak: true`; else `Unlicensed`. Summaries, exactly:
    - agrees: `register agrees: <licensee>, <site>, <emission words>, <d> km` (` (range cannot be bounded on this band)` appended when weak)
    - disagrees: `register disagrees: licensed here as <emission words> (<licensee>); measured <label>, <width> kHz`
    - unlicensed: `nothing licensed in range; the band is <primary services joined by ", ">` (`; <n> area-wide licence(s) cover it` appended when any)
    - unknown: `register unreachable: <reason>`
  - No caller passes a `measured_dbm` yet: `classify` and Identify hold only a signal-to-noise ratio (spec 3.3 "Level"). Pass `None` with that comment.
  - `classify` (ops.rs): call `described` once; the classifiers get `d.rows` cut as `nearest_allocations` cuts them (extract that cut into `knowledge::nearest_of(rows: Vec<Allocation>) -> Vec<Allocation>` which keeps `rows`' order for assignments, so rank order survives); `score_classification` takes the `Described` and fills `origins` and `register` (with `verdict` set).
  - Identify: the same single `described`; `allocations` from it; the verdict payload and both `json!` verdicts gain `"register"`; the job summary uses `verdict.summary` in place of `register_says(..)` when there is a register answer.
  - `register_says` stays for the watch.
  - Agents notes: in `agents_md.rs` add to `classify`'s and `identify`'s notes what `register` holds, the four verdict kinds, that `unknown` means the register could not be asked, and that an expected level is an estimate. In `field-craft.md`, beside the existing "consistent with" guidance, add: quote `as_of`; say "expected about"; an `unlicensed` verdict is not evidence of wrongdoing (class-licensed and mobile use are not in a register). Regenerate both generated files.

- [ ] **Step 4: Run** `cargo test -p zeta-core --lib && cargo test -p zeta-core --features test-fixtures --test identify && cargo run -q -p zeta -- agents-md --check && cargo run -q -p zeta -- agents-md --skill --check` — expected: PASS and both checks clean.

- [ ] **Step 5: Commit** `Plan 37: one register answer per signal, a verdict in plain words, and the reach term from geometry`.

---

### Task 6: Identify's decoder order follows the candidates' rank, never its set

**Files:**
- Modify: `crates/zeta-core/src/modes/score.rs:79-88`
- Test: `crates/zeta-core/src/modes/score.rs` tests, `crates/zeta-core/tests/identify.rs`

**Interfaces:**
- Produces: `pub fn allocation_term(row: &LoadedMode, allocs: &[Allocation]) -> f32` (same signature): `ALLOCATION_SCORE / (1 + rank)` where `rank` counts the `tx` assignments before the first one that fits `row` (0 for the first), 0.0 when none fits.

- [ ] **Step 1: Write the failing tests.**

```rust
#[test] fn a_row_fitting_the_top_candidate_gets_the_whole_bonus_and_lower_ranks_get_less() {
    // allocs: [am 6 kHz tx, fm 16 kHz tx]; acars (am) -> ALLOCATION_SCORE, pocsag (fm) -> ALLOCATION_SCORE / 2.0
}
#[test] fn the_bonus_never_exceeds_todays() { /* every row, any allocs: term <= ALLOCATION_SCORE */ }
#[test] fn receivers_and_plan_rows_do_not_count_toward_rank() {}
// tests/identify.rs
async fn the_register_reorders_the_decoders_and_drops_none() {
    // run identify twice on the same synthetic band: once with no register, once with a register whose
    // top candidate fits the row that scored second. Assert: the sets of tried mode ids are equal;
    // the order differs; a row whose measured score led by more than ALLOCATION_SCORE still leads.
}
```

- [ ] **Step 2: Run** `cargo test -p zeta-core --lib score` — expected: FAIL on the first test (`ALLOCATION_SCORE` returned for both).

- [ ] **Step 3: Implement** the graded term; `allocation_match` is unchanged.

- [ ] **Step 4: Run** `cargo test -p zeta-core --lib score && cargo test -p zeta-core --features test-fixtures --test identify && cargo test -p zeta-dsp --features test-fixtures identify` — expected: PASS (update any test that assumed the flat bonus for a second-ranked row).

- [ ] **Step 5: Commit** `Plan 37: the register's weight on a decoder follows its candidate's rank, capped at the old bonus`.

---

### Task 7: The operator's surface: a site's channels, the operator's own queries, and a check

**Files:**
- Modify: `crates/zeta-core/src/ops.rs` (new op), `crates/zeta-core/src/knowledge.rs` (one more defaulted method), `crates/zeta-ext/src/acma.rs`, `crates/zeta/src/cli.rs`, `crates/zeta/src/main.rs`, `crates/zeta/src/node_setup.rs`, `crates/zeta/src/agents_md.rs`
- Create: `crates/zeta/src/registers_check.rs`
- Test: `crates/zeta/tests/registers_check.rs` (new), `crates/zeta-ext/tests/acma_describe.rs`

**Interfaces:**
- Produces:
  - `KnowledgeProvider::site(&self, site_id: &str) -> Option<Result<SiteAnswer, String>>` (default `None`), with `pub struct SiteAnswer { pub provider: String, pub site_id: String, pub channels: Vec<SiteChannel>, pub as_of: Option<String> }` and `pub struct SiteChannel { pub channel: Channel, pub licence_no: Option<String>, pub licensee: Option<String> }` in `register.rs`.
  - `Op::RegisterSite { provider: String, site_id: String }` (`register_site`), result `OpResult::RegisterSite(SiteAnswer)`; an unknown provider or one with no `site` is `OpError` `not_found`; a failed query is `OpError` `unavailable` carrying the reason.
  - CLI: `--registers-dir DIR` (env `ZETA_REGISTERS_DIR`), loaded with `RegisterQueries::with_dir`; `zeta registers check [PROVIDER]`; `zeta register site PROVIDER SITE_ID`.
  - `zeta registers check` prints one line per query: `<provider>/<id>: ok <rows> rows <ms> ms`, `<provider>/<id>: fail: <reason>`, or `<provider>: unreachable: <reason>`, then `<provider>: answer <ms> ms of <deadline> ms` for one whole `describe` at the station (or with no observer); exits 0 when nothing failed. It runs each query with sample values: the frequency span 400-401 MHz, the station's position when set, and for the `Id` queries the first `licence_no` / `site_id` the `candidates` run returned (skipped, and saying so, when it returned none).

- [ ] **Step 1: Write the failing tests:** `acma_describe.rs`: `a_sites_channels_come_from_the_site_channels_query`, `a_site_id_with_a_quote_is_refused_before_any_call` (the stub saw no `execute_sql`). `tests/registers_check.rs`: against `acma_stub::serve_on_thread` — `every_builtin_query_passes_on_the_stub`, `a_query_the_register_rejects_fails_by_name_and_exits_nonzero` (`--registers-dir` with a `candidates.json` selecting a column that does not exist), `an_unreachable_register_is_reported_as_such`.

- [ ] **Step 2: Run** `cargo test -p zeta --test registers_check` — expected: does not compile.

- [ ] **Step 3: Implement.** `registers check` is local-only like `modes check` (the same refusal with `--node` in `transport.rs`). The op's note in `agents_md.rs`: what it returns, that it reads the register live, and that `site_id` comes from an allocation's `site_id`.

- [ ] **Step 4: Run** `cargo test -p zeta --test registers_check && cargo test -p zeta --test cli && cargo run -q -p zeta -- agents-md > AGENTS.md && cargo run -q -p zeta -- agents-md --skill > skills/zetasdr/SKILL.md` — expected: PASS.

- [ ] **Step 5: Commit** `Plan 37: register_site, the operator's own register queries, and zeta registers check`.

---

### Task 8: "What is this?" shows the answer; Origins leaves the menu

**Files:**
- Modify: `crates/zeta-ui/web/menu.js` (the `classify` handler, the entry list at 303-304, the `origins` branch at ~994, the header comment), `crates/zeta-ui/web/units.js`, `crates/zeta-ui/web/explore.js:431` (use the verdict when present), `crates/zeta-ui/tests/e2e/ui.py`, `crates/zeta/tests/ui.rs` (`fake_register_rows`)

**Interfaces:**
- Consumes: `Classification.register` (Task 5).
- Produces in `units.js`: `export function registerLines(register, { measured } = {})` returning an array of transcript lines; `export const REGISTER_TOP = 3`.

- [ ] **Step 1: Write the failing scenario** `scenario_what_is_this_shows_the_register_answer` in `ui.py` (and add it to a shard): select the tone's band, choose "What is this?", then poll the transcript for, in this order:
  - a line starting `register agrees:` or `register disagrees:` ending with `(register data as of 2026-01-01)`
  - `band: ` followed by the stub plan's primary service
  - up to three candidate lines of the form `<licensee>, <site>, <d> km <bearing>°, <emission words>, expected about <n> dBm`, with `, pointed away` appended when `off_beam_deg` exceeds half the beamwidth
  - for the first candidate, `belongs to: ` and its paired frequency in MHz
  - and assert the map shows `.map-site` markers for the answer and the context menu holds no `[data-action="origins"]` item.
  Also `scenario_an_unreachable_register_says_so`: with the register stub stopped, the transcript reads `register unreachable:` and never `nothing licensed`.

- [ ] **Step 2: Run** `ZETA_UI_ONLY=what_is_this_shows_the_register_answer,an_unreachable_register_says_so cargo test -p zeta --test ui -- --nocapture` — expected: both scenarios FAIL (check the output does not say `ui e2e skipped`).

- [ ] **Step 3: Implement.** `registerLines` writes the lines above; with `register` null (an older node) the caller keeps today's `registerText(allocations)` line. A partial status adds `register partly answered: <query>: <reason>` per failed query. With no expected level the candidate line omits that part; on a calibrated source nothing is compared yet (no measured level reaches the page). The `classify` handler keeps calling `onOrigins(key, v.origins)` so the map is lit as now. Remove the `origins` entry and its branch from `menu.js`; the Origins buttons in `bits.js` and `explore.js` stay. Extend `fake_register_rows` only as needed for a paired frequency.

- [ ] **Step 4: Run** the two scenarios again, then `scenario_what_is_this_classifies` and every scenario whose name holds `origin` — expected: PASS.

- [ ] **Step 5: Commit** `Plan 37: What is this? shows the register's answer and plots it; Origins leaves the box menu`.

---

### Task 9: One shared selection: sites selectable like subjects

**Files:**
- Create: `crates/zeta-ui/web/selection.js`
- Modify: `crates/zeta-ui/web/subjects.js` (`select`, `selected`, `setSelected`, `onSelect` at 422-450; the pane render at ~649), `crates/zeta-ui/web/map.js` (site markers at ~750, `markSelected` at 955, the header comment's "a click selects nothing"), `crates/zeta-ui/web/overlays.js`, `crates/zeta-ui/web/app.js` (wiring), `crates/zeta-ui/web/style.css`, `crates/zeta-ui/tests/e2e/ui.py`

**Interfaces:**
- Produces:

```js
// selection.js
// A selection is null or one of:
//   { kind: "subject", job, group, key }
//   { kind: "site", provider, siteId, name, lat, lon, set }   // `set` is the lit set's key
export class Selection { get(); set(sel); onChange(cb); }
```

  `SubjectStore` takes the shared `Selection` in its constructor; `select`, `selected`, `setSelected` and `onSelect` keep their signatures and delegate to it (`selected()` returns the subject form or null, so existing callers are unchanged). `overlays`: `ov.siteChannels(key, channels)` draws one thin marker per channel inside the visible span (class `site-channel`, `data-role` tx|rx, title `<MHz> <emission>`), `ov.clearSiteChannels()` removes them.

- [ ] **Step 1: Write the failing scenarios** in `ui.py`:
  - `scenario_a_map_site_can_be_selected`: after "What is this?", click a `.map-site`; it gains class `selected`; the Subjects pane shows a row `tr.site-selected` naming the site; the waterfall shows `.site-channel` markers for the site's channels in view; clicking a subject's marker moves the selection (the site loses `selected`, the markers go).
  - `scenario_selecting_a_subject_lights_its_sites`: with a decoder job that has origins and a subject, select the subject in the pane; the sites of that job's lit set gain class `matched`.

- [ ] **Step 2: Run** `ZETA_UI_ONLY=a_map_site_can_be_selected,selecting_a_subject_lights_its_sites cargo test -p zeta --test ui -- --nocapture` — expected: FAIL.

- [ ] **Step 3: Implement.** A site marker's click sets the site selection (its popup still opens). On a site selection the page applies `register_site` once (cached per site id for the page's life), passes the channels to `ov.siteChannels`, and the pane renders one row under a "Site" heading inside the existing Subjects pane (no new slice of the waterfall). On a subject selection, `map.markSelected` also adds `matched` to the sites of the set keyed by `sel.job`. A signal the operator asked about is already lit by "What is this?" (its set is keyed by the selection), so it needs no selection kind of its own. A selection whose job is gone is cleared by `map.sync()`. `register_site` failing notes `site <name>: <error>` in the transcript and draws nothing.

- [ ] **Step 4: Run** the two scenarios, then every scenario whose name holds `subject` or `map` — expected: PASS.

- [ ] **Step 5: Commit** `Plan 37: one selection shared by the map, the Subjects pane and the waterfall; register sites can be selected`.

---

### Task 10: Acceptance checks, the open work, and the gates

**Files:**
- Modify: `UAT.md` (a "Plan 37" section), `TODO.md` (its "Register answer: later stages" entry holds the later stages; close the items this plan settles), `docs/superpowers/specs/2026-11-12-register-answer-design.md` (only if a decision changed while building: record it in section 7)

- [ ] **Step 1: Add the acceptance checks** to `UAT.md`, worded against roles, no real sites or frequencies:
  - 37.1 `zeta registers check` on the field host: every query `ok`, and the whole answer inside the deadline. Record the milliseconds.
  - 37.2 "What is this?" on a known licensed channel: verdict `agrees`, the band's purpose, the holder's site on the map, the data date.
  - 37.3 On a class-licensed band: verdict `unlicensed`, worded as the band's purpose.
  - 37.4 With the register service stopped: `register unreachable`, never `nothing licensed`.
  - 37.5 Identify on a signal whose licence names another emission: the decoder that fits what was measured is still tried, and the summary says `disagrees`.
  - 37.6 Select a site on the map: its channels are marked on the waterfall; select a subject: its sites are marked.
  - 37.7 Raise the station's altitude with `station_set` and repeat 37.2: more distant candidates appear, with the same code.

- [ ] **Step 2: Settle `TODO.md`.** Mark as done by plan 37, with one line each: the 200-row cap before dedup, the `CALL_SIGN` probe that never resets, `as_of` always null, and identify's two register round trips. Leave "which site sent this burst" open, noting that the expected level now exists per candidate.

- [ ] **Step 3: Run every gate** with the private target directory:

```bash
for c in zeta-core zeta-ext zeta zeta-ui zeta-dsp; do cargo fmt -p $c -- --check; done
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium
cargo test -p zeta --test ui -- --nocapture          # must not print "ui e2e skipped"
cargo run -q -p zeta -- agents-md --check
cargo run -q -p zeta -- agents-md --skill --check
cargo deny check licenses bans sources               # when cargo-deny is installed; CI runs it either way
bash scripts/public-check.sh
bash scripts/check-doc-links.sh
```

  Expected: all clean.

- [ ] **Step 4: Commit** `Plan 37: acceptance checks and the TODO items it settles`, then delete the private target directory.
