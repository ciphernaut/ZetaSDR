//! The register answer: one licensed-environment answer for a signal (the band plan
//! segment, the licensed candidates with their geometry and expected level, and the
//! status of the register itself), in terms no regulator owns. A provider fills it; the
//! node, the agent and the page read it.
//!
//! The queries a provider runs are data: one JSON row each under `registers/<provider>/`
//! (embedded, then the operator's own directory). Only numbers and validated register
//! ids are ever substituted into a query's text.

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::emission::Emission;
use crate::knowledge::{Allocation, require_all};
use crate::propagation::{Expected, Geometry, Propagation};
use crate::types::Hz;

/// The most candidates one answer carries.
pub const REGISTER_MAX_CANDIDATES: usize = 50;
/// How many of the top candidates get a [`Related`] block.
pub const RELATED_TOP: usize = 3;
/// The most channels listed in [`Related::holder_channel_list`].
pub const RELATED_CHANNELS: usize = 12;
/// The query that lists the licensed candidates. When it fails, the answer cannot say
/// "nothing licensed" ([`RegisterStatus::licences_failed`]).
pub const CANDIDATES_QUERY: &str = "candidates";
/// The longest register id a query accepts.
const MAX_ID_LEN: usize = 64;
/// The largest row limit a query may ask for.
const MAX_QUERY_LIMIT: u64 = 5000;

/// Whether the register answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RegisterState {
    /// Every query answered; an empty candidate list means nothing is licensed.
    Ok,
    /// Some queries failed (see `failed`); the rest is real.
    Partial,
    /// The register could not be asked: nothing is known, which is not "nothing licensed".
    Unreachable,
}

/// What the node makes of an answer against the measured signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Agrees,
    Disagrees,
    Unlicensed,
    Unknown,
}

/// What may be substituted for a query parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    /// A finite number.
    Number,
    /// A register id: `^[0-9A-Za-z/_.-]{1,64}$`, rendered in single quotes.
    Id,
}

/// A signal to ask the register about: where it is and how wide.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Signal {
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
}

/// A register query that failed, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct FailedQuery {
    pub query: String,
    pub reason: String,
}

/// Whether the register answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct RegisterStatus {
    pub state: RegisterState,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub reason: Option<String>,
    pub failed: Vec<FailedQuery>,
}

impl RegisterStatus {
    /// Why the register could not list licences: the reason it was unreachable, or the
    /// failed [`CANDIDATES_QUERY`]'s; `None` when the candidates query answered (an
    /// empty list then means nothing is licensed).
    pub fn licences_failed(&self) -> Option<String> {
        if self.state == RegisterState::Unreachable {
            return Some(
                self.reason
                    .clone()
                    .unwrap_or_else(|| "no reason given".into()),
            );
        }
        self.failed
            .iter()
            .find(|f| f.query == CANDIDATES_QUERY)
            .map(|f| f.reason.clone())
    }
}

/// One service a plan segment allocates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct PlanService {
    pub name: String,
    pub primary: bool,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub qualifier: Option<String>,
}

/// A footnote of a plan segment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct PlanFootnote {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub text: Option<String>,
}

/// A band plan segment `[min_hz, max_hz]` and the services it allocates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct PlanSegment {
    pub min_hz: Hz,
    pub max_hz: Hz,
    pub services: Vec<PlanService>,
    pub footnotes: Vec<PlanFootnote>,
}

/// A licensed device: one frequency of a licence, with whatever the register holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Device {
    pub center_hz: Hz,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<Hz>")]
    pub bandwidth_hz: Option<Hz>,
    /// The designator as the register stores it, trimmed.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub emission: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<Emission>")]
    pub emission_decoded: Option<Emission>,
    /// `tx` or `rx`.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub role: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub power_w: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub eirp_w: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub gain_dbi: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub azimuth_deg: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub beamwidth_deg: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub front_to_back_db: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub height_m: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub tilt_deg: Option<f64>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub polarisation: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub callsign: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub station_class: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub nature_of_service: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub hours: Option<String>,
}

/// A site of the register.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Site {
    pub id: String,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub name: Option<String>,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub precision: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub elevation_m: Option<f64>,
}

/// A licence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Licence {
    pub number: String,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub service: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub subservice: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub status: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub issued: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub expires: Option<String>,
}

/// A licence holder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Holder {
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub name: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub trading_name: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub industry: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub client_type: Option<String>,
}

/// One channel of a licence or a holder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Channel {
    pub center_hz: Hz,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<Hz>")]
    pub bandwidth_hz: Option<Hz>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub role: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub emission: Option<String>,
}

/// One channel at a register site, with the licence and holder it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct SiteChannel {
    pub channel: Channel,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub licence_no: Option<String>,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub licensee: Option<String>,
}

/// Every channel the register holds at one site (`register_site`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct SiteAnswer {
    pub provider: String,
    pub site_id: String,
    /// By frequency; empty when the register holds nothing at the site.
    pub channels: Vec<SiteChannel>,
    /// The register's data as of this time, when it says.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub as_of: Option<String>,
}

/// The other end of a link: its site and how far it is from the first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct FarEnd {
    pub site: Site,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub distance_km: Option<f64>,
}

/// What else the register holds about a candidate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Related {
    /// Other frequencies on the same licence.
    pub paired: Vec<Channel>,
    /// The holder's channels at the site, counted.
    pub holder_channels: u32,
    /// Capped at [`RELATED_CHANNELS`].
    pub holder_channel_list: Vec<Channel>,
    #[serde(default)]
    #[schemars(required)]
    pub far_end: Option<FarEnd>,
}

/// A licensed device that may be the signal, with its site, licence and holder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Candidate {
    pub device: Device,
    pub site: Site,
    pub licence: Licence,
    pub holder: Holder,
    #[serde(default)]
    #[schemars(required)]
    pub geometry: Option<Geometry>,
    #[serde(default)]
    #[schemars(required)]
    pub expected: Option<Expected>,
    #[serde(default)]
    #[schemars(required)]
    pub related: Option<Related>,
    /// How many of the answer's sites, this one included, send the same thing on this
    /// frequency ([`count_networks`]): more than 1 is a same-frequency network, whose
    /// sites the register alone cannot tell apart (their `polarisation` may). 0 in an
    /// answer kept before the count.
    #[serde(default)]
    pub network_sites: u32,
}

/// A licence that covers an area rather than a site.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct AreaCandidate {
    pub device: Device,
    pub licence: Licence,
    pub holder: Holder,
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub area: Option<String>,
}

/// What the node makes of the register answer against the measured signal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct Verdict {
    pub kind: VerdictKind,
    pub weak: bool,
    pub summary: String,
}

/// The register's whole answer for a signal: the band plan, the candidates in rank order and the assumptions behind the estimates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct RegisterAnswer {
    pub provider: String,
    pub region: String,
    pub status: RegisterStatus,
    /// The register's data as of this time, when it says.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub as_of: Option<String>,
    pub plan: Vec<PlanSegment>,
    pub candidates: Vec<Candidate>,
    pub area_candidates: Vec<AreaCandidate>,
    /// How many assignments matched, before the cap.
    pub matched: u32,
    pub truncated: bool,
    pub assumptions: Propagation,
    /// Filled by the node.
    #[serde(default)]
    #[schemars(required)]
    pub verdict: Option<Verdict>,
}

/// A query parameter: its name and what may be substituted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct QueryParam {
    pub name: String,
    pub kind: ParamKind,
}

/// One register query as a data row (`registers/<provider>/<id>.json`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_all)]
pub struct RegisterQuery {
    pub id: String,
    pub provider: String,
    pub name: String,
    /// Only `execute_sql` is accepted.
    pub call: String,
    /// Placeholders are `{name}`.
    pub sql: String,
    pub params: Vec<QueryParam>,
    pub limit: u64,
    /// The result column aliases the query must return.
    pub columns: Vec<String>,
}

impl RegisterAnswer {
    /// An answer for a register that could not be asked: no rows, and the reason.
    pub fn unreachable(provider: &str, region: &str, reason: String) -> Self {
        Self {
            provider: provider.into(),
            region: region.into(),
            status: RegisterStatus {
                state: RegisterState::Unreachable,
                reason: Some(reason),
                failed: Vec::new(),
            },
            as_of: None,
            plan: Vec::new(),
            candidates: Vec::new(),
            area_candidates: Vec::new(),
            matched: 0,
            truncated: false,
            assumptions: Propagation::default(),
            verdict: None,
        }
    }

    /// The existing wire rows: one `assignment` per candidate in rank order, then one
    /// `plan` row per plan service, with the `service` and `note` text the acma provider
    /// has always written.
    pub fn allocations(&self) -> Vec<Allocation> {
        let assignments = self.candidates.iter().map(|c| {
            let d = &c.device;
            let half = d.bandwidth_hz.map_or(0, |b| b.0 / 2);
            let distance_km = c.geometry.as_ref().map(|g| g.distance_km);
            // A zero is the register's way of holding no figure (a broadcast licence
            // holds an EIRP and a zero transmitter power).
            let usable = |w: Option<f64>| w.filter(|w| w.is_finite() && *w > 0.0);
            let (power_w, eirp_w) = (usable(d.power_w), usable(d.eirp_w));
            let expected_level_dbm = c.expected.as_ref().and_then(|e| e.level_dbm);
            Allocation {
                provider: self.provider.clone(),
                region: self.region.clone(),
                min_hz: Hz(d.center_hz.0.saturating_sub(half)),
                max_hz: Hz(d.center_hz.0.saturating_add(half)),
                service: "assignment".into(),
                note: note_line(
                    c.holder.name.as_deref(),
                    c.site.name.as_deref(),
                    distance_km,
                    power_words(power_w, eirp_w),
                    d.emission.as_deref(),
                    d.emission_decoded.as_ref(),
                    expected_level_dbm,
                ),
                kind: Some("assignment".into()),
                licensee: c.holder.name.clone(),
                site: c.site.name.clone(),
                site_id: Some(c.site.id.clone()),
                lat: Some(c.site.lat),
                lon: Some(c.site.lon),
                distance_km,
                bearing_deg: c.geometry.as_ref().map(|g| g.bearing_deg),
                power_w,
                eirp_w,
                expected_level_dbm,
                center_hz: Some(d.center_hz),
                bandwidth_hz: d.bandwidth_hz,
                licence_no: Some(c.licence.number.clone()),
                callsign: d.callsign.clone(),
                role: d.role.clone(),
                emission: d.emission.clone(),
                emission_decoded: d.emission_decoded.clone(),
                as_of: self.as_of.clone(),
            }
        });
        let plan = self.plan.iter().flat_map(|p| {
            p.services.iter().map(|s| Allocation {
                provider: self.provider.clone(),
                region: self.region.clone(),
                min_hz: p.min_hz,
                max_hz: p.max_hz,
                service: format!(
                    "{} ({})",
                    s.name,
                    if s.primary { "primary" } else { "secondary" }
                ),
                note: s.qualifier.clone(),
                kind: Some("plan".into()),
                as_of: self.as_of.clone(),
                ..Allocation::default()
            })
        });
        assignments.chain(plan).collect()
    }
}

/// Fills each candidate's [`Candidate::network_sites`]: the distinct sites among the
/// transmitters that share its centre, its emission designator and its callsign (the
/// holder's name where neither has a callsign). A receiver, and a transmitter with
/// neither a callsign nor a holder's name, counts only itself.
pub fn count_networks(candidates: &mut [Candidate]) {
    // What a transmitter sends, as far as the register says: `None` for a row that
    // cannot be joined to another.
    fn sent(c: &Candidate) -> Option<(u64, Option<&str>, bool, &str)> {
        let d = &c.device;
        if d.role.as_deref() != Some("tx") {
            return None;
        }
        let (by_callsign, name) = match (d.callsign.as_deref(), c.holder.name.as_deref()) {
            (Some(cs), _) if !cs.is_empty() => (true, cs),
            (_, Some(h)) if !h.is_empty() => (false, h),
            _ => return None,
        };
        Some((d.center_hz.0, d.emission.as_deref(), by_callsign, name))
    }
    let counts: Vec<u32> = candidates
        .iter()
        .map(|c| {
            let Some(key) = sent(c) else {
                return 1;
            };
            let sites: std::collections::BTreeSet<&str> = candidates
                .iter()
                .filter(|o| sent(o) == Some(key))
                .map(|o| o.site.id.as_str())
                .collect();
            u32::try_from(sites.len()).unwrap_or(u32::MAX)
        })
        .collect();
    for (c, n) in candidates.iter_mut().zip(counts) {
        c.network_sites = n;
    }
}

/// `licensee, site, 10 km, 1 W, 16 kHz FM data, expected about -95 dBm`, the parts the
/// row lacks dropped; `power` as [`power_words`] gives it.
pub fn note_line(
    licensee: Option<&str>,
    site: Option<&str>,
    distance_km: Option<f64>,
    power: Option<String>,
    emission: Option<&str>,
    decoded: Option<&Emission>,
    expected_level_dbm: Option<f64>,
) -> Option<String> {
    let parts: Vec<String> = [
        licensee.map(str::to_string),
        site.map(str::to_string),
        distance_km.map(|d| format!("{d:.0} km")),
        power,
        decoded.map(emission_words).or(emission.map(str::to_string)),
        expected_level_dbm.map(level_words),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// A row's power in words: the transmitter's (`25 W`), else its radiated power, marked
/// (`50 kW EIRP`); `None` when it holds neither.
pub fn power_words(power_w: Option<f64>, eirp_w: Option<f64>) -> Option<String> {
    power_w
        .map(watts_words)
        .or(eirp_w.map(|p| format!("{} EIRP", watts_words(p))))
}

/// A power in watts as `25 W`, or `50 kW` from a kilowatt up.
pub fn watts_words(w: f64) -> String {
    if w >= 1e3 {
        format!("{} kW", trim_num(w / 1e3))
    } else {
        format!("{} W", trim_num(w))
    }
}

/// An expected level as `expected about -95 dBm`: an estimate, worded as one.
pub fn level_words(dbm: f64) -> String {
    format!("expected about {dbm:.0} dBm")
}

/// A number with at most three decimals and no trailing zeros.
pub fn trim_num(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// `16K0F1D` as `16 kHz FM data`.
pub fn emission_words(e: &Emission) -> String {
    let hz = e.bandwidth_hz.0 as f64;
    let bw = if hz < 1e3 {
        format!("{} Hz", trim_num(hz))
    } else if hz < 1e6 {
        format!("{} kHz", trim_num(hz / 1e3))
    } else {
        format!("{} MHz", trim_num(hz / 1e6))
    };
    let modulation = match e.modulation {
        'N' => "carrier",
        'A' => "AM",
        'H' | 'R' | 'J' => "SSB",
        'B' => "ISB",
        'C' => "VSB",
        'F' => "FM",
        'G' => "PM",
        'D' => "AM/PM",
        'P' | 'K' | 'L' | 'M' | 'Q' | 'V' => "pulse",
        'W' => "mixed",
        _ => "other",
    };
    let info = match e.info {
        'A' | 'B' => "telegraphy",
        'C' => "fax",
        'D' => "data",
        'E' => "voice",
        'F' => "video",
        'W' => "mixed",
        'X' => "other",
        _ => "",
    };
    [bw.as_str(), modulation, info]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A value for one query parameter.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Number(f64),
    Int(i64),
    Id(String),
}

/// The embedded register queries, one file each under `registers/` in the repo.
const EMBEDDED: [(&str, &str); 6] = [
    (
        "registers/acma/candidates.json",
        include_str!("../../../registers/acma/candidates.json"),
    ),
    (
        "registers/acma/area_devices.json",
        include_str!("../../../registers/acma/area_devices.json"),
    ),
    (
        "registers/acma/plan_span.json",
        include_str!("../../../registers/acma/plan_span.json"),
    ),
    (
        "registers/acma/licence_related.json",
        include_str!("../../../registers/acma/licence_related.json"),
    ),
    (
        "registers/acma/site_channels.json",
        include_str!("../../../registers/acma/site_channels.json"),
    ),
    (
        "registers/acma/link_far_end.json",
        include_str!("../../../registers/acma/link_far_end.json"),
    ),
];

/// The register queries by `(provider, id)`.
#[derive(Clone, Debug, Default)]
pub struct RegisterQueries {
    rows: BTreeMap<(String, String), RegisterQuery>,
}

impl RegisterQueries {
    /// The embedded rows. A row that does not parse or validate is a data error in the
    /// build, so it panics.
    pub fn builtin() -> Self {
        let mut rows = BTreeMap::new();
        for (file, text) in EMBEDDED {
            let q: RegisterQuery =
                serde_json::from_str(text).unwrap_or_else(|e| panic!("{file}: {e}"));
            q.validate().unwrap_or_else(|e| panic!("{file}: {e}"));
            rows.insert((q.provider.clone(), q.id.clone()), q);
        }
        Self { rows }
    }

    /// Adds the operator's rows from `<dir>/<provider>/<id>.json`: a row with an
    /// embedded row's `(provider, id)` replaces it. A missing `dir` adds nothing; a row
    /// that does not parse, validate or match its path is an error naming the file.
    pub fn with_dir(mut self, dir: &Path) -> Result<Self, String> {
        if !dir.is_dir() {
            return Ok(self);
        }
        let read = |p: &Path| -> Result<Vec<std::path::PathBuf>, String> {
            let mut v: Vec<_> = std::fs::read_dir(p)
                .map_err(|e| format!("{}: {e}", p.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .collect();
            v.sort();
            Ok(v)
        };
        for provider_dir in read(dir)?.into_iter().filter(|p| p.is_dir()) {
            let provider = provider_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for path in read(&provider_dir)? {
                if path.extension().is_none_or(|x| x != "json") || !path.is_file() {
                    continue;
                }
                let file = path.display().to_string();
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{file}: {e}"))?;
                let q: RegisterQuery =
                    serde_json::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
                q.validate().map_err(|e| format!("{file}: {e}"))?;
                let stem = path
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if q.provider != provider {
                    return Err(format!(
                        "{file}: provider `{}` is not the directory `{provider}`",
                        q.provider
                    ));
                }
                if q.id != stem {
                    return Err(format!("{file}: id `{}` is not the file name", q.id));
                }
                self.rows.insert((q.provider.clone(), q.id.clone()), q);
            }
        }
        Ok(self)
    }

    /// The query `id` of `provider`.
    pub fn get(&self, provider: &str, id: &str) -> Option<&RegisterQuery> {
        self.rows.get(&(provider.to_string(), id.to_string()))
    }

    /// Every query of `provider`, by id.
    pub fn for_provider(&self, provider: &str) -> Vec<&RegisterQuery> {
        self.rows
            .iter()
            .filter(|((p, _), _)| p == provider)
            .map(|(_, q)| q)
            .collect()
    }
}

/// Whether `n` has a placeholder name's shape.
fn is_placeholder_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The `{name}` placeholders in `sql`, in order.
fn placeholders(sql: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = sql;
    while let Some(i) = rest.find('{') {
        rest = &rest[i + 1..];
        if let Some(j) = rest.find('}')
            && is_placeholder_name(&rest[..j])
        {
            out.push(&rest[..j]);
            rest = &rest[j + 1..];
        }
    }
    out
}

/// `s` checked as a register id, the only text a query's [`ParamKind::Id`] parameter
/// accepts; the error names `field`.
pub fn check_register_id(field: &str, s: &str) -> Result<(), String> {
    if is_register_id(s) {
        return Ok(());
    }
    Err(format!(
        "{field}: {s:?} is not a register id (1-{MAX_ID_LEN} letters, digits, `/`, `_`, `.` or `-`)"
    ))
}

/// Whether `s` is a register id: `^[0-9A-Za-z/_.-]{1,64}$`.
fn is_register_id(s: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
}

impl RegisterQuery {
    /// Checks the row; the error names the field.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("id: empty".into());
        }
        if self.provider.is_empty() {
            return Err("provider: empty".into());
        }
        if self.call != "execute_sql" {
            return Err(format!(
                "call: `{}` is not `execute_sql`, the only call a register query makes",
                self.call
            ));
        }
        let head = self.sql.trim_start().to_ascii_uppercase();
        if !(head.starts_with("SELECT") || head.starts_with("WITH")) {
            return Err("sql: must start with SELECT or WITH".into());
        }
        if self.sql.contains(';') {
            return Err("sql: must not contain `;`".into());
        }
        let used = placeholders(&self.sql);
        for name in &used {
            if !self.params.iter().any(|p| p.name == *name) {
                return Err(format!("sql: `{{{name}}}` is not declared in params"));
            }
        }
        for p in &self.params {
            if !used.contains(&p.name.as_str()) {
                return Err(format!("params: `{}` is not used in sql", p.name));
            }
        }
        if !(1..=MAX_QUERY_LIMIT).contains(&self.limit) {
            return Err(format!(
                "limit: {} is not in 1..={MAX_QUERY_LIMIT}",
                self.limit
            ));
        }
        if self.columns.is_empty() {
            return Err("columns: empty".into());
        }
        Ok(())
    }

    /// The query text with every `{name}` replaced by its value: a finite number as is,
    /// an id in single quotes. A missing or ill-shaped value is an error.
    pub fn render(&self, values: &BTreeMap<&str, ParamValue>) -> Result<String, String> {
        let mut out = String::with_capacity(self.sql.len());
        let mut rest = self.sql.as_str();
        while let Some(i) = rest.find('{') {
            out.push_str(&rest[..i]);
            let tail = &rest[i + 1..];
            let name = tail
                .find('}')
                .map(|j| &tail[..j])
                .filter(|n| is_placeholder_name(n));
            let Some(name) = name else {
                out.push('{');
                rest = tail;
                continue;
            };
            let param = self
                .params
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| format!("sql: `{{{name}}}` is not declared in params"))?;
            let value = values
                .get(name)
                .ok_or_else(|| format!("params: no value for `{name}`"))?;
            match (param.kind, value) {
                (ParamKind::Number, ParamValue::Number(x)) if x.is_finite() => {
                    out.push_str(&x.to_string());
                }
                (ParamKind::Number, ParamValue::Number(x)) => {
                    return Err(format!("params: `{name}` is not finite ({x})"));
                }
                (ParamKind::Number, ParamValue::Int(n)) => out.push_str(&n.to_string()),
                (ParamKind::Id, ParamValue::Id(s)) if is_register_id(s) => {
                    out.push('\'');
                    out.push_str(s);
                    out.push('\'');
                }
                (ParamKind::Id, ParamValue::Id(s)) => {
                    return Err(format!("params: `{name}` is not a register id ({s:?})"));
                }
                _ => return Err(format!("params: `{name}` has the wrong kind of value")),
            }
            rest = &tail[name.len() + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emission::parse_emission;

    fn num(name: &str) -> QueryParam {
        QueryParam {
            name: name.into(),
            kind: ParamKind::Number,
        }
    }

    fn id(name: &str) -> QueryParam {
        QueryParam {
            name: name.into(),
            kind: ParamKind::Id,
        }
    }

    fn row() -> RegisterQuery {
        RegisterQuery {
            id: "q".into(),
            provider: "acma".into(),
            name: "A query".into(),
            call: "execute_sql".into(),
            sql: "SELECT 1 AS a".into(),
            params: Vec::new(),
            limit: 10,
            columns: vec!["a".into()],
        }
    }

    fn q() -> RegisterQuery {
        RegisterQuery {
            sql: "SELECT 1 WHERE f BETWEEN {lo} AND {hi} AND l = {licence}".into(),
            params: vec![num("lo"), num("hi"), id("licence")],
            ..row()
        }
    }

    fn values<'a>(v: &[(&'a str, ParamValue)]) -> BTreeMap<&'a str, ParamValue> {
        v.iter().cloned().collect()
    }

    fn all_values(licence: &str) -> BTreeMap<&'static str, ParamValue> {
        values(&[
            ("lo", ParamValue::Int(1)),
            ("hi", ParamValue::Int(2)),
            ("licence", ParamValue::Id(licence.into())),
        ])
    }

    #[test]
    fn the_builtin_queries_load_and_validate() {
        let q = RegisterQueries::builtin();
        let ids = [
            "area_devices",
            "candidates",
            "licence_related",
            "link_far_end",
            "plan_span",
            "site_channels",
        ];
        for id in ids {
            let c = q.get("acma", id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(c.call, "execute_sql");
            c.validate().unwrap_or_else(|e| panic!("{id}: {e}"));
            // Every declared column is selected under its alias.
            for col in &c.columns {
                assert!(c.sql.contains(&format!(" AS {col}")), "{id}: {col}");
            }
        }
        let all: Vec<&str> = q
            .for_provider("acma")
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(all, ids);
        assert!(q.for_provider("nowhere").is_empty());
    }

    #[test]
    fn numbers_and_ids_are_substituted_and_nothing_else() {
        let sql = q()
            .render(&values(&[
                ("lo", ParamValue::Int(100)),
                ("hi", ParamValue::Number(2.5)),
                ("licence", ParamValue::Id("9000001/1".into())),
            ]))
            .unwrap();
        assert_eq!(
            sql,
            "SELECT 1 WHERE f BETWEEN 100 AND 2.5 AND l = '9000001/1'"
        );
    }

    #[test]
    fn an_id_with_a_quote_is_refused() {
        for bad in ["9000001' OR 1=1 --", "a;b", "a b", "", &"x".repeat(65)] {
            assert!(q().render(&all_values(bad)).is_err(), "{bad}");
        }
        assert!(q().render(&all_values(&"x".repeat(64))).is_ok());
        let e = check_register_id("site_id", "9001' OR 1=1 --").unwrap_err();
        assert!(e.starts_with("site_id: "), "{e}");
        check_register_id("site_id", "9001").unwrap();
    }

    #[test]
    fn a_missing_or_undeclared_parameter_is_an_error() {
        let missing = values(&[
            ("hi", ParamValue::Int(2)),
            ("licence", ParamValue::Id("1".into())),
        ]);
        let e = q().render(&missing).unwrap_err();
        assert!(e.contains("lo"), "{e}");
        let undeclared = RegisterQuery {
            sql: "SELECT {other}".into(),
            ..row()
        };
        let e = undeclared.validate().unwrap_err();
        assert!(e.contains("sql") && e.contains("other"), "{e}");
        let unused = RegisterQuery {
            params: vec![num("lo")],
            ..row()
        };
        assert!(unused.validate().unwrap_err().contains("params"));
        q().validate().unwrap();
    }

    #[test]
    fn a_non_finite_number_is_refused() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut v = all_values("1");
            v.insert("lo", ParamValue::Number(bad));
            assert!(q().render(&v).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_value_of_the_wrong_kind_is_refused() {
        let mut v = all_values("1");
        v.insert("lo", ParamValue::Id("1".into()));
        assert!(q().render(&v).is_err());
        let mut v = all_values("1");
        v.insert("licence", ParamValue::Number(1.0));
        assert!(q().render(&v).is_err());
    }

    #[test]
    fn a_query_that_is_not_execute_sql_or_not_a_select_is_invalid() {
        let sync = RegisterQuery {
            call: "sync_data".into(),
            ..row()
        };
        assert!(sync.validate().unwrap_err().starts_with("call"));
        for bad in [
            "DELETE FROM t",
            "SELECT 1; DROP TABLE t",
            "  update t set a=1",
        ] {
            let r = RegisterQuery {
                sql: bad.into(),
                ..row()
            };
            assert!(r.validate().unwrap_err().starts_with("sql"), "{bad}");
        }
        let with = RegisterQuery {
            sql: "with t as (select 1) select * from t".into(),
            ..row()
        };
        with.validate().unwrap();
        for limit in [0, 5001] {
            let r = RegisterQuery { limit, ..row() };
            assert!(r.validate().unwrap_err().starts_with("limit"));
        }
        let r = RegisterQuery {
            columns: Vec::new(),
            ..row()
        };
        assert!(r.validate().unwrap_err().starts_with("columns"));
    }

    #[test]
    fn an_operator_row_replaces_the_builtin_of_the_same_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("acma")).unwrap();
        let mine = RegisterQuery {
            id: "candidates".into(),
            sql: "SELECT 2 AS center_hz".into(),
            columns: vec!["center_hz".into()],
            ..row()
        };
        std::fs::write(
            dir.path().join("acma/candidates.json"),
            serde_json::to_string(&mine).unwrap(),
        )
        .unwrap();
        let both = RegisterQueries::builtin().with_dir(dir.path()).unwrap();
        assert_eq!(
            both.get("acma", "candidates").unwrap().sql,
            "SELECT 2 AS center_hz"
        );
        assert_eq!(
            both.for_provider("acma").len(),
            RegisterQueries::builtin().for_provider("acma").len()
        );
        // A broken operator row is an error naming the file.
        std::fs::write(dir.path().join("acma/bad.json"), "{}").unwrap();
        let e = RegisterQueries::builtin().with_dir(dir.path()).unwrap_err();
        assert!(e.contains("bad.json"), "{e}");
        // A missing directory adds nothing.
        RegisterQueries::builtin()
            .with_dir(&dir.path().join("nope"))
            .unwrap();
    }

    #[test]
    fn an_operator_row_in_the_wrong_place_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("other")).unwrap();
        std::fs::write(
            dir.path().join("other/q.json"),
            serde_json::to_string(&row()).unwrap(),
        )
        .unwrap();
        let e = RegisterQueries::default().with_dir(dir.path()).unwrap_err();
        assert!(e.contains("directory"), "{e}");
    }

    const OLD_ALLOCATION_JSON: &str = r#"{"provider":"acma","region":"AU","min_hz":1,"max_hz":2,
        "service":"assignment","note":null,"licence_no":"0000001"}"#;

    #[test]
    fn an_allocation_written_before_site_id_loads() {
        let a: Allocation = serde_json::from_str(OLD_ALLOCATION_JSON).unwrap();
        assert_eq!(a.site_id, None);
        let v = serde_json::to_value(&a).unwrap();
        assert!(v.as_object().unwrap().contains_key("site_id"));
        let schema = serde_json::to_value(schemars::schema_for!(Allocation)).unwrap();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r == "site_id")
        );
    }

    #[test]
    fn an_unreachable_answer_says_so_and_has_no_rows() {
        let a = RegisterAnswer::unreachable("acma", "AU", "timeout".into());
        assert_eq!(a.status.state, RegisterState::Unreachable);
        assert_eq!(a.status.reason.as_deref(), Some("timeout"));
        assert!(a.candidates.is_empty() && a.allocations().is_empty());
    }

    fn candidate(center: u64, licence: &str, km: Option<f64>) -> Candidate {
        let emission = "16K0F1D";
        Candidate {
            device: Device {
                center_hz: Hz(center),
                bandwidth_hz: Some(Hz(16_000)),
                emission: Some(emission.into()),
                emission_decoded: parse_emission(emission),
                role: Some("tx".into()),
                power_w: Some(1.0),
                eirp_w: None,
                gain_dbi: None,
                azimuth_deg: None,
                beamwidth_deg: None,
                front_to_back_db: None,
                height_m: None,
                tilt_deg: None,
                polarisation: None,
                callsign: Some("XX4EX".into()),
                station_class: None,
                nature_of_service: None,
                hours: None,
            },
            site: Site {
                id: "S1".into(),
                name: Some("Hill".into()),
                lat: -27.5,
                lon: 153.0,
                precision: None,
                elevation_m: None,
            },
            licence: Licence {
                number: licence.into(),
                service: None,
                subservice: None,
                status: None,
                issued: None,
                expires: None,
            },
            holder: Holder {
                name: Some("Example Pty Ltd".into()),
                trading_name: None,
                industry: None,
                client_type: None,
            },
            geometry: km.map(|d| Geometry {
                distance_km: d,
                bearing_deg: 45.0,
                off_beam_deg: None,
                horizon_km: None,
                beyond_horizon: false,
            }),
            expected: None,
            related: None,
            network_sites: 0,
        }
    }

    #[test]
    fn allocations_are_the_candidates_in_rank_order_then_the_plan() {
        let mut a = RegisterAnswer::unreachable("acma", "AU", String::new());
        a.status = RegisterStatus {
            state: RegisterState::Ok,
            reason: None,
            failed: Vec::new(),
        };
        a.as_of = Some("2026-11-01".into());
        a.candidates = vec![
            candidate(450_000_000, "9000001/1", Some(10.4)),
            candidate(450_100_000, "9000002/1", None),
        ];
        a.plan = vec![PlanSegment {
            min_hz: Hz(450_000_000),
            max_hz: Hz(451_000_000),
            services: vec![
                PlanService {
                    name: "LAND MOBILE".into(),
                    primary: true,
                    qualifier: Some("AUS 12".into()),
                },
                PlanService {
                    name: "FIXED".into(),
                    primary: false,
                    qualifier: None,
                },
            ],
            footnotes: Vec::new(),
        }];
        let rows = a.allocations();
        assert_eq!(rows.len(), 4);
        let first = &rows[0];
        assert_eq!(first.kind.as_deref(), Some("assignment"));
        assert_eq!(first.service, "assignment");
        assert_eq!(first.role.as_deref(), Some("tx"));
        assert_eq!(first.site.as_deref(), Some("Hill"));
        assert_eq!(first.site_id.as_deref(), Some("S1"));
        assert_eq!(first.licensee.as_deref(), Some("Example Pty Ltd"));
        assert_eq!((first.lat, first.lon), (Some(-27.5), Some(153.0)));
        assert_eq!(first.distance_km, Some(10.4));
        assert_eq!(first.bearing_deg, Some(45.0));
        assert_eq!(first.power_w, Some(1.0));
        assert_eq!(first.center_hz, Some(Hz(450_000_000)));
        assert_eq!(first.bandwidth_hz, Some(Hz(16_000)));
        assert_eq!(
            (first.min_hz, first.max_hz),
            (Hz(449_992_000), Hz(450_008_000))
        );
        assert_eq!(first.licence_no.as_deref(), Some("9000001/1"));
        assert_eq!(first.callsign.as_deref(), Some("XX4EX"));
        assert_eq!(first.emission.as_deref(), Some("16K0F1D"));
        assert!(first.emission_decoded.is_some());
        assert_eq!(first.as_of.as_deref(), Some("2026-11-01"));
        assert_eq!(
            first.note.as_deref(),
            Some("Example Pty Ltd, Hill, 10 km, 1 W, 16 kHz FM data")
        );
        assert_eq!(first.provider, "acma");
        assert_eq!(first.region, "AU");
        assert_eq!(rows[1].licence_no.as_deref(), Some("9000002/1"));
        assert_eq!(rows[1].distance_km, None);
        assert_eq!(
            rows[1].note.as_deref(),
            Some("Example Pty Ltd, Hill, 1 W, 16 kHz FM data")
        );
        assert_eq!(rows[2].kind.as_deref(), Some("plan"));
        assert_eq!(rows[2].service, "LAND MOBILE (primary)");
        assert_eq!(rows[2].note.as_deref(), Some("AUS 12"));
        assert_eq!(
            (rows[2].min_hz, rows[2].max_hz),
            (Hz(450_000_000), Hz(451_000_000))
        );
        assert_eq!(rows[3].service, "FIXED (secondary)");
        assert_eq!(rows[3].note, None);
    }

    /// An `ok` answer holding `candidates`.
    fn answer_of(candidates: Vec<Candidate>) -> RegisterAnswer {
        let mut a = RegisterAnswer::unreachable("acma", "AU", String::new());
        a.status = RegisterStatus {
            state: RegisterState::Ok,
            reason: None,
            failed: Vec::new(),
        };
        a.candidates = candidates;
        a
    }

    #[test]
    fn a_row_with_only_an_eirp_carries_it_and_a_zero_power_is_not_known() {
        // A broadcast licence as the register holds it: no transmitter power, an EIRP.
        let mut c = candidate(613_500_000, "9000003/1", Some(42.0));
        c.device.power_w = Some(0.0);
        c.device.eirp_w = Some(50_000.0);
        let row = &answer_of(vec![c.clone()]).allocations()[0];
        assert_eq!(row.power_w, None);
        assert_eq!(row.eirp_w, Some(50_000.0));
        assert_eq!(
            row.note.as_deref(),
            Some("Example Pty Ltd, Hill, 42 km, 50 kW EIRP, 16 kHz FM data")
        );
        // Both held: the note keeps the transmitter power, the row carries both.
        c.device.power_w = Some(25.0);
        let row = &answer_of(vec![c.clone()]).allocations()[0];
        assert_eq!((row.power_w, row.eirp_w), (Some(25.0), Some(50_000.0)));
        assert!(row.note.as_deref().unwrap().contains(", 25 W, "), "{row:?}");
        // Neither usable: no figure, and none in the note.
        c.device.power_w = Some(0.0);
        c.device.eirp_w = Some(0.0);
        let row = &answer_of(vec![c]).allocations()[0];
        assert_eq!((row.power_w, row.eirp_w), (None, None));
        assert_eq!(
            row.note.as_deref(),
            Some("Example Pty Ltd, Hill, 42 km, 16 kHz FM data")
        );
    }

    #[test]
    fn a_row_carries_its_candidates_expected_level() {
        let mut c = candidate(450_000_000, "9000001/1", Some(10.4));
        c.expected = Some(Expected {
            mode: crate::propagation::PathMode::LineOfSight,
            level_dbm: Some(-42.4),
            in_range: crate::propagation::InRange::Yes,
            unknown: Vec::new(),
        });
        let rows = answer_of(vec![c, candidate(450_100_000, "9000002/1", None)]).allocations();
        assert_eq!(rows[0].expected_level_dbm, Some(-42.4));
        assert_eq!(
            rows[0].note.as_deref(),
            Some("Example Pty Ltd, Hill, 10 km, 1 W, 16 kHz FM data, expected about -42 dBm")
        );
        assert_eq!(rows[1].expected_level_dbm, None);
    }

    #[test]
    fn sites_sending_the_same_thing_on_one_frequency_are_counted_as_a_network() {
        // One licence per site, as a register holds a broadcast network: what joins
        // them is the frequency, the emission and the callsign.
        let site = |licence: &str, id: &str, callsign: Option<&str>| {
            let mut c = candidate(613_500_000, licence, Some(10.0));
            c.site.id = id.into();
            c.device.callsign = callsign.map(str::to_string);
            c
        };
        let mut other_holder = site("5/1", "S5", None);
        other_holder.holder.name = Some("Another Pty Ltd".into());
        let mut receiver = site("6/1", "S6", Some("NET1"));
        receiver.device.role = Some("rx".into());
        let mut cs = vec![
            site("1/1", "S1", Some("NET1")),
            site("2/1", "S2", Some("NET1")),
            // The same site twice is one site.
            site("2/2", "S2", Some("NET1")),
            site("3/1", "S3", Some("NET2")),
            // No callsign: the holder's name joins them.
            site("4/1", "S4", None),
            site("4/2", "S7", None),
            other_holder,
            receiver,
            // Another frequency is another network.
            {
                let mut c = site("8/1", "S8", Some("NET1"));
                c.device.center_hz = Hz(620_500_000);
                c
            },
        ];
        count_networks(&mut cs);
        let counts: Vec<u32> = cs.iter().map(|c| c.network_sites).collect();
        assert_eq!(counts, [2, 2, 2, 1, 2, 2, 1, 1, 1]);
    }

    #[test]
    fn the_answer_round_trips_and_its_schema_requires_every_property() {
        let mut a = RegisterAnswer::unreachable("acma", "AU", "x".into());
        a.candidates = vec![candidate(450_000_000, "1", Some(1.0))];
        a.plan = vec![PlanSegment {
            min_hz: Hz(1),
            max_hz: Hz(2),
            services: Vec::new(),
            footnotes: vec![PlanFootnote {
                reference: "AUS 1".into(),
                text: None,
            }],
        }];
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["plan"][0]["footnotes"][0]["ref"], "AUS 1");
        assert!(v["candidates"][0]["related"].is_null());
        assert!(v["verdict"].is_null());
        let back: RegisterAnswer = serde_json::from_value(v).unwrap();
        assert_eq!(back, a);
        let schema = serde_json::to_value(schemars::schema_for!(RegisterAnswer)).unwrap();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "verdict"), "{required:?}");
    }
}
