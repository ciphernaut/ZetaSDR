//! The origin query (plan 32, design 4.1): which licensed transmitters a signal on a band
//! may come from, seen from where it was received.

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::persist::{now, persist};
use super::{Inner, Node, NodeError};
use crate::bursts::CaptureRecord;
use crate::classify::Classification;
use crate::job::JobRecord;
use crate::knowledge::{Allocation, DEFAULT_REGION, KnowledgeProvider};
use crate::modes::score::{designator_fits, row_width};
use crate::modes::{Identity, ModeRegistry};
use crate::ops::{BITS_KIND, GRAPH_KIND, SignalBand};
use crate::reach::Reach;
use crate::station::{Antenna, Station};
use crate::stream::{Frame, Payload};
use crate::types::{Baud, Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};

/// At most this many origins in one answer, best score first, then nearest.
pub const ORIGIN_SITES: usize = 50;
/// Width term when the row's width is within [`ORIGIN_WIDTH_RANGE`] of the band's.
pub const ORIGIN_WIDTH_FULL: f32 = 0.4;
/// Width term when the row's width is within [`ORIGIN_WIDTH_LOOSE`] of the band's.
pub const ORIGIN_WIDTH_HALF: f32 = 0.2;
/// Width term when the row has no width at all.
pub const ORIGIN_WIDTH_NONE: f32 = 0.1;
/// Row width over band width for the full width term.
pub const ORIGIN_WIDTH_RANGE: (f64, f64) = (0.5, 1.5);
/// Row width over band width for the half width term.
pub const ORIGIN_WIDTH_LOOSE: (f64, f64) = (0.25, 4.0);
/// Modulation term when the designator's letter is compatible with a measured label.
pub const ORIGIN_MODULATION_FITS: f32 = 0.4;
/// Modulation term when there is no designator or no label.
pub const ORIGIN_MODULATION_UNKNOWN: f32 = 0.2;
/// Reach term when the site's distance is known and within the reach, at the station
/// (distance 0) or for a reach with no radius; graded down toward the limit
/// ([`ORIGIN_REACH_FLOOR`]).
pub const ORIGIN_REACH_IN: f32 = 0.2;
/// The share of [`ORIGIN_REACH_IN`] a site exactly at the reach limit keeps: the term
/// falls linearly from the full term at the station to this share at the limit.
pub const ORIGIN_REACH_FLOOR: f32 = 0.5;
/// Reach term when the site's distance is not known (no vantage).
pub const ORIGIN_REACH_UNKNOWN: f32 = 0.1;
/// Decimal places an origin's score is rounded to.
pub const ORIGIN_SCORE_DECIMALS: i32 = 2;
/// How many sites [`summary`] names.
pub const ORIGIN_SUMMARY_SITES: usize = 3;

/// The signal as measured or declared, to find the transmitters it may come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct OriginQuery {
    /// The band's source; its kind decides the vantage ([`vantage_for`]).
    pub source: SourceId,
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
    /// Layer-1 labels (`fm`, `fsk`, ...); empty when not known.
    pub modulation: Vec<String>,
    #[schemars(required, schema_with = "crate::types::nullable::<Baud>")]
    pub symbol_rate_baud: Option<Baud>,
    #[schemars(required, schema_with = "crate::types::nullable::<JobId>")]
    pub job: Option<JobId>,
    pub at: Timestamp,
}

/// How a row's emission designator agrees with the measured modulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Agree {
    Fits,
    Disagrees,
    Unknown,
}

/// The graded terms an origin's score sums.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Terms {
    pub width: f32,
    pub modulation: f32,
    pub reach: f32,
}

/// The decoded identity that confirmed an origin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Confirmed {
    pub field: String,
    pub value: String,
}

/// One lit site: a register row and how well it fits the signal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Origin {
    /// The row as `knowledge_lookup` gives it.
    pub allocation: Allocation,
    /// 0..1: the sum of `terms`, or 1 once `confirmed`.
    pub score: f32,
    pub agree: Agree,
    pub terms: Terms,
    #[schemars(required, schema_with = "crate::types::nullable::<Confirmed>")]
    pub confirmed: Option<Confirmed>,
}

/// What a vantage was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FromKind {
    Station,
    Source,
}

/// Where the query was made from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct From {
    pub lat: f64,
    pub lon: f64,
    pub kind: FromKind,
    /// The source whose position it was; `null` for the station.
    #[schemars(required, schema_with = "crate::types::nullable::<SourceId>")]
    pub source: Option<SourceId>,
}

/// The origin query's answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct OriginAnswer {
    pub origins: Vec<Origin>,
    pub band: SignalBand,
    pub source: SourceId,
    pub modulation: Vec<String>,
    #[schemars(required, schema_with = "crate::types::nullable::<Baud>")]
    pub symbol_rate_baud: Option<Baud>,
    #[schemars(required, schema_with = "crate::types::nullable::<From>")]
    pub from: Option<From>,
    /// Why `from` is `null`: `no station set`, `remote source without a position`.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub from_note: Option<String>,
    /// The providers asked.
    pub providers: Vec<String>,
    /// Rows the providers that give assignments (a licence register,
    /// `KnowledgeProvider::gives_assignments`) returned in all, before any scoring;
    /// `null` when none of the providers asked gives assignments.
    #[schemars(required, schema_with = "crate::types::nullable::<u32>")]
    pub register_rows: Option<u32>,
    /// Receiver rows seen and not scored.
    pub receivers: u32,
    /// Plan rows seen and not scored.
    pub plans: u32,
    #[schemars(required, schema_with = "crate::types::nullable::<Reach>")]
    pub reach: Option<Reach>,
    pub t_ns: Timestamp,
}

/// Where a source's signals are heard from.
#[derive(Clone, Debug, PartialEq)]
pub enum Vantage {
    /// The node's station (a local radio, a recording).
    Station(Station),
    /// A remote source's own position, with the default antennas.
    Source(Station, SourceId),
    /// No vantage, and why.
    None(&'static str),
}

impl Vantage {
    /// The station value the providers are asked from, if any.
    pub fn station(&self) -> Option<&Station> {
        match self {
            Vantage::Station(s) | Vantage::Source(s, _) => Some(s),
            Vantage::None(_) => None,
        }
    }
}

/// The vantage of `source`: a local source (not `Capabilities.remote`, or not attached,
/// such as a capture's source gone since) is heard at the station; a remote one at its
/// position ([`Inner::source_position`]), with the default antennas and the station's
/// region (else [`DEFAULT_REGION`]).
pub fn vantage_for(inner: &Inner, source: &SourceId) -> Vantage {
    let station = inner.station.lock().unwrap().clone();
    let remote = inner.port(source).is_some_and(|p| p.capabilities().remote);
    if !remote {
        return match station {
            Some(s) => Vantage::Station(s),
            None => Vantage::None("no station set"),
        };
    }
    match inner.source_position(source) {
        Some(p) => {
            let alt_m = p.alt_m.unwrap_or(0.0);
            Vantage::Source(
                Station {
                    lat: p.lat,
                    lon: p.lon,
                    alt_m,
                    region: station.map_or_else(|| DEFAULT_REGION.to_string(), |s| s.region),
                    antennas: Station::default_antennas(alt_m),
                },
                source.clone(),
            )
        }
        None => Vantage::None("remote source without a position"),
    }
}

/// Ask every provider for `q`'s band from `q.source`'s vantage, all at once, and score
/// the rows into an answer ([`answer`]); the reach is that of the first provider (in
/// order) that has one. The region is the vantage's, else the station's, else
/// [`DEFAULT_REGION`].
pub async fn origins(inner: &Inner, q: &OriginQuery) -> OriginAnswer {
    let vantage = vantage_for(inner, &q.source);
    let region = match vantage.station() {
        Some(s) => s.region.clone(),
        None => inner
            .station
            .lock()
            .unwrap()
            .as_ref()
            .map_or_else(|| DEFAULT_REGION.to_string(), |s| s.region.clone()),
    };
    let providers: Vec<Arc<dyn KnowledgeProvider>> = inner.knowledge.read().unwrap().clone();
    let names = providers.iter().map(|p| p.name().to_string()).collect();
    let registers = providers.iter().any(|p| p.gives_assignments());
    let from = vantage.station().cloned();
    // A JoinSet aborts the lookups still running when the caller is dropped.
    let mut set = tokio::task::JoinSet::new();
    let mut ids = HashMap::new();
    for (i, p) in providers.iter().enumerate() {
        let (p, region, from) = (p.clone(), region.clone(), from.clone());
        let (c, b) = (q.center_hz, q.bandwidth_hz);
        let handle = set.spawn(async move {
            let rows = p.allocations_from(&region, c, b, from.as_ref()).await;
            let reach = p.reach_from(&region, c, b, from.as_ref()).await;
            (i, rows, reach)
        });
        ids.insert(handle.id(), i);
    }
    let mut answers: Vec<Option<(Vec<Allocation>, Option<Reach>)>> = vec![None; providers.len()];
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((i, r, re)) => answers[i] = Some((r, re)),
            // A provider that failed answers nothing; say which.
            Err(e) => {
                let name = ids.get(&e.id()).map_or("?", |&i| providers[i].name());
                eprintln!("zeta: origin: provider {name}: lookup failed ({e})");
            }
        }
    }
    let (mut rows, mut reach, mut register_rows) = (Vec::new(), None, 0);
    for (p, a) in providers.iter().zip(answers) {
        let Some((r, re)) = a else { continue };
        if p.gives_assignments() {
            register_rows += r.len() as u32;
        }
        rows.extend(r);
        // The first provider, in order, that has a reach.
        reach = reach.or(re);
    }
    let register_rows = registers.then_some(register_rows);
    answer(q, rows, reach, &vantage, names, register_rows)
}

/// The deduplication key of an origin: licence, site and resolved centre.
type SiteKey = (Option<String>, Option<String>, u64);

/// A row's centre: its `center_hz`, else the middle of `min_hz..max_hz`.
fn row_center(a: &Allocation) -> u64 {
    a.center_hz.map_or(a.min_hz.0 / 2 + a.max_hz.0 / 2, |h| h.0)
}

/// Score `rows` against `q` into an answer: `tx` assignments scored ([`score_row`]),
/// receivers (`rx` assignments) and plan rows counted, deduplicated by `(licence_no,
/// site, centre)` (the centre as [`score_row`] resolves it; a row with neither licence
/// nor site is never folded) keeping the best score, best first then nearest (no
/// distance last), at most [`ORIGIN_SITES`]. `providers` and `register_rows` are kept as given.
pub fn answer(
    q: &OriginQuery,
    rows: Vec<Allocation>,
    reach: Option<Reach>,
    vantage: &Vantage,
    providers: Vec<String>,
    register_rows: Option<u32>,
) -> OriginAnswer {
    let (mut receivers, mut plans) = (0, 0);
    let mut best: HashMap<SiteKey, Origin> = HashMap::new();
    let mut unnamed: Vec<Origin> = Vec::new();
    // The vantage's antenna the reach was computed for.
    let antenna = reach.as_ref().and_then(|r| {
        vantage
            .station()
            .and_then(|s| s.antennas.iter().find(|a| a.id == r.antenna))
    });
    for a in rows {
        if a.kind.as_deref() != Some("assignment") {
            plans += 1;
            continue;
        }
        if a.role.as_deref() == Some("rx") {
            receivers += 1;
            continue;
        }
        let Some(o) = score_row(&a, q, reach.as_ref(), antenna) else {
            continue;
        };
        if a.licence_no.is_none() && a.site.is_none() {
            unnamed.push(o);
            continue;
        }
        let key = (a.licence_no.clone(), a.site.clone(), row_center(&a));
        if best.get(&key).is_none_or(|kept| o.score > kept.score) {
            best.insert(key, o);
        }
    }
    let mut origins: Vec<Origin> = best.into_values().chain(unnamed).collect();
    origins.sort_by(origin_order);
    origins.truncate(ORIGIN_SITES);
    let (from, from_note) = match vantage {
        Vantage::Station(s) => (
            Some(From {
                lat: s.lat,
                lon: s.lon,
                kind: FromKind::Station,
                source: None,
            }),
            None,
        ),
        Vantage::Source(s, id) => (
            Some(From {
                lat: s.lat,
                lon: s.lon,
                kind: FromKind::Source,
                source: Some(id.clone()),
            }),
            None,
        ),
        Vantage::None(note) => (None, Some((*note).to_string())),
    };
    OriginAnswer {
        origins,
        band: SignalBand {
            center_hz: q.center_hz,
            bandwidth_hz: q.bandwidth_hz,
        },
        source: q.source.clone(),
        modulation: q.modulation.clone(),
        symbol_rate_baud: q.symbol_rate_baud,
        from,
        from_note,
        providers,
        register_rows,
        receivers,
        plans,
        reach,
        t_ns: now(),
    }
}

/// The answer's order: best score first, a confirmed origin before an equal score, then
/// nearest, then by licence, site and centre.
fn origin_order(a: &Origin, b: &Origin) -> std::cmp::Ordering {
    let (x, y) = (&a.allocation, &b.allocation);
    b.score
        .total_cmp(&a.score)
        .then_with(|| b.confirmed.is_some().cmp(&a.confirmed.is_some()))
        .then_with(|| nearest_first(x.distance_km, y.distance_km))
        .then_with(|| x.licence_no.cmp(&y.licence_no))
        .then_with(|| x.site.cmp(&y.site))
        .then_with(|| x.center_hz.map(|h| h.0).cmp(&y.center_hz.map(|h| h.0)))
}

/// Known distances first, nearest first.
fn nearest_first(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// `a` scored against `q` (design 4.1); `None` unless it is a `tx` assignment whose
/// centre and the band overlap: `|centre - q.center_hz| <= row_width/2 + bandwidth/2`,
/// `row_width` the decoded designator's, else the row's `bandwidth_hz`, else 0 (the
/// centre is the row's `center_hz`, else the middle of `min_hz..max_hz`). The terms:
/// width, by the ratio of `row_width` to the band's ([`ORIGIN_WIDTH_RANGE`],
/// [`ORIGIN_WIDTH_LOOSE`]), times `1 - |centre - q.center_hz| / (row_width/2 +
/// bandwidth/2)` (1 on the band's centre, 0 at the gate's edge) and rounded like the
/// score, so an on-channel row outscores one that only overlaps the band's edge; modulation, the designator's letter against `q.modulation`
/// by `DESIGNATOR_LABELS`; reach, the row's `distance_km` against the reach toward the
/// site ([`Reach::km_toward`] with the row's `bearing_deg` and `antenna`, the vantage's
/// antenna the reach was computed for; `reach.km` when either is missing), graded
/// `ORIGIN_REACH_IN * (1 - (1 - ORIGIN_REACH_FLOOR) * distance / reach)` and rounded
/// (0.2 at the station, 0.1 at the limit, 0 beyond). A reach with no radius, or none at
/// all, holds any known distance at the full term.
pub fn score_row(
    a: &Allocation,
    q: &OriginQuery,
    reach: Option<&Reach>,
    antenna: Option<&Antenna>,
) -> Option<Origin> {
    if a.kind.as_deref() != Some("assignment") || a.role.as_deref() != Some("tx") {
        return None;
    }
    let width = row_width(a).map(|h| h.0);
    let center = row_center(a);
    let offset = center.abs_diff(q.center_hz.0);
    let gate = width.unwrap_or(0) / 2 + q.bandwidth_hz.0 / 2;
    if offset > gate {
        return None;
    }
    // How far inside the gate the row's centre sits: 1 on the band's centre, 0 at the
    // gate's edge, so a row that only overlaps the band's edge does not tie the one on it.
    let centred = if gate == 0 {
        1.0
    } else {
        1.0 - offset as f32 / gate as f32
    };
    let grade = match width {
        None => ORIGIN_WIDTH_NONE,
        Some(w) => {
            let r = w as f64 / q.bandwidth_hz.0 as f64;
            let within = |(lo, hi): (f64, f64)| (lo..=hi).contains(&r);
            if within(ORIGIN_WIDTH_RANGE) {
                ORIGIN_WIDTH_FULL
            } else if within(ORIGIN_WIDTH_LOOSE) {
                ORIGIN_WIDTH_HALF
            } else {
                0.0
            }
        }
    };
    let width_term = round_score(grade * centred);
    let (agree, modulation_term) = match &a.emission_decoded {
        Some(e) if !q.modulation.is_empty() => match designator_fits(e.modulation, &q.modulation) {
            true => (Agree::Fits, ORIGIN_MODULATION_FITS),
            false => (Agree::Disagrees, 0.0),
        },
        _ => (Agree::Unknown, ORIGIN_MODULATION_UNKNOWN),
    };
    let reach_term = match a.distance_km {
        None => ORIGIN_REACH_UNKNOWN,
        Some(d) => match reach.and_then(|r| match (a.bearing_deg, antenna) {
            (Some(b), Some(ant)) => r.km_toward(b, ant),
            _ => r.km,
        }) {
            Some(km) if d > km => 0.0,
            // Graded by distance over the reach: the full term at the station, the
            // floor's share at the limit, so a near site outranks a far one.
            Some(km) if km > 0.0 => {
                let share = 1.0 - (1.0 - ORIGIN_REACH_FLOOR) * (d / km) as f32;
                round_score(ORIGIN_REACH_IN * share)
            }
            _ => ORIGIN_REACH_IN,
        },
    };
    let terms = Terms {
        width: width_term,
        modulation: modulation_term,
        reach: reach_term,
    };
    Some(Origin {
        allocation: a.clone(),
        score: round_score(terms.width + terms.modulation + terms.reach),
        agree,
        terms,
        confirmed: None,
    })
}

/// `score` to [`ORIGIN_SCORE_DECIMALS`] places, so equal sums of the terms compare
/// equal whatever order f32 added them in.
fn round_score(score: f32) -> f32 {
    let scale = 10f32.powi(ORIGIN_SCORE_DECIMALS);
    (score * scale).round() / scale
}

/// `hz` in MHz without trailing zeros: `403.025`.
fn mhz(hz: Hz) -> String {
    let s = format!("{:.6}", hz.0 as f64 / 1e6);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// One line for an answer (design 4.3): `<n> sites on <f> MHz: <licensee>, <site>
/// <d> km <agree> <score>; ...` naming the first [`ORIGIN_SUMMARY_SITES`] (the
/// distance left out when unknown); `no licensed transmitter on <f> MHz` when nothing
/// is lit (whatever provider lit them). With nothing lit, `register_rows` picks the
/// words: `no register provider` when none of the providers asked gives assignments,
/// `register gave no rows` when they returned none at all (an attached register that
/// is unreachable, or empty there), else `no licensed transmitter on <f> MHz`.
pub fn summary(a: &OriginAnswer) -> String {
    let f = mhz(a.band.center_hz);
    if a.origins.is_empty() {
        return match a.register_rows {
            None => "no register provider".to_string(),
            Some(0) => "register gave no rows".to_string(),
            Some(_) => format!("no licensed transmitter on {f} MHz"),
        };
    }
    let sites: Vec<String> = a
        .origins
        .iter()
        .take(ORIGIN_SUMMARY_SITES)
        .map(|o| {
            let al = &o.allocation;
            let name: Vec<&str> = [al.licensee.as_deref(), al.site.as_deref()]
                .into_iter()
                .flatten()
                .collect();
            let mut s = match name.is_empty() {
                true => al.service.clone(),
                false => name.join(", "),
            };
            if let Some(d) = al.distance_km {
                s.push_str(&format!(" {d:.0} km"));
            }
            let agree = match o.agree {
                Agree::Fits => "fits",
                Agree::Disagrees => "disagrees",
                Agree::Unknown => "unknown",
            };
            format!("{s} {agree} {:.1}", o.score)
        })
        .collect();
    let n = a.origins.len();
    let noun = if n == 1 { "site" } else { "sites" };
    format!("{n} {noun} on {f} MHz: {}", sites.join("; "))
}

/// Confirm every origin whose `callsign` equals `value` (both trimmed, case ignored):
/// each one's score becomes 1 and `confirmed` `{field, value}`, its terms stay. The
/// indices of those origins, in answer order; empty, changing nothing, when none
/// matches. The order is left as it was: a caller re-sorts if the order matters.
pub fn confirm(a: &mut OriginAnswer, field: &str, value: &str) -> Vec<usize> {
    let want = value.trim();
    let mut hit = Vec::new();
    for (i, o) in a.origins.iter_mut().enumerate() {
        let matches = o
            .allocation
            .callsign
            .as_deref()
            .is_some_and(|c| c.trim().eq_ignore_ascii_case(want));
        if matches {
            o.score = 1.0;
            o.confirmed = Some(Confirmed {
                field: field.to_string(),
                value: value.to_string(),
            });
            hit.push(i);
        }
    }
    hit
}

/// The identity an event's `identity.field` value `v` carries (design 4.6): a string,
/// trimmed, upper-cased, with an AX.25 `-N` SSID suffix (`N` digits) removed when
/// `strip` is `ssid`; `None` for a non-string or a value left blank.
pub fn identity_value(identity: &Identity, v: &serde_json::Value) -> Option<String> {
    let mut s = v.as_str()?.trim();
    if identity.strip.as_deref() == Some("ssid")
        && let Some((call, n)) = s.rsplit_once('-')
        && !n.is_empty()
        && n.bytes().all(|b| b.is_ascii_digit())
    {
        s = call.trim_end();
    }
    (!s.is_empty()).then(|| s.to_ascii_uppercase())
}

/// [`confirm`] `value` (from the event field `field`) in `a`, then re-sort `a` so the
/// confirmed origins, score 1, lead. The republished `summary`,
/// `confirmed <value>: <licensee>, <site>` naming the first confirmed origin after the
/// sort (the parts its row has; its service when it has neither). `None`, changing
/// nothing, when no origin's callsign is `value`, or when every one whose callsign is
/// already has `confirmed.value` `value` (a repeat changes nothing).
pub fn confirm_identity(a: &mut OriginAnswer, field: &str, value: &str) -> Option<String> {
    let want = value.trim();
    let mut matching = a.origins.iter().filter(|o| {
        o.allocation
            .callsign
            .as_deref()
            .is_some_and(|c| c.trim().eq_ignore_ascii_case(want))
    });
    if matching.all(|o| o.confirmed.as_ref().is_some_and(|c| c.value == value)) {
        // No origin matches (`all` of none), or every match is confirmed already.
        return None;
    }
    confirm(a, field, value);
    a.origins.sort_by(origin_order);
    let al = &a
        .origins
        .iter()
        .find(|o| o.confirmed.as_ref().is_some_and(|c| c.value == value))?
        .allocation;
    let name: Vec<&str> = [al.licensee.as_deref(), al.site.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    let name = match name.is_empty() {
        true => al.service.clone(),
        false => name.join(", "),
    };
    Some(format!("confirmed {value}: {name}"))
}

/// Re-apply to `fresh` each confirmation `kept` holds ([`confirm`] by its field and
/// value), then re-sort `fresh` when any landed: a fresh lookup replacing a kept answer
/// does not lose an identity confirmed while it was out.
pub fn carry_confirmed(kept: &OriginAnswer, fresh: &mut OriginAnswer) {
    let mut hit = false;
    for c in kept.origins.iter().filter_map(|o| o.confirmed.as_ref()) {
        hit |= !confirm(fresh, &c.field, &c.value).is_empty();
    }
    if hit {
        fresh.origins.sort_by(origin_order);
    }
}

/// Job kinds with no band to look up: the refusal names the kind.
pub const ORIGIN_NO_BAND_KINDS: [&str; 3] = ["mix", "spectrum", "scan"];

/// The refusal of an `origin_lookup` given both forms, neither, or a part of the band.
const ORIGIN_FORMS: &str = "origin_lookup: give job, or source, center_hz and bandwidth_hz";

/// The layer-1 labels a job states by what it runs (design 4.2): a decoder row's
/// `modulation`; a demodulator's kind (`fm_demod`/`nbfm_demod` → `fm`, `am_demod` →
/// `am`, `ssb_demod` → `ssb`, `wfm_demod` → `wfm`); a `bits` or `graph` job's chain
/// demodulator (`fsk_demod` → `fsk`, `psk_demod` → `psk`, `ook_demod` → `ook`); else
/// none (unknown).
pub fn job_modulation(rec: &JobRecord, modes: &ModeRegistry) -> Vec<String> {
    if let Some(m) = modes.get(&rec.kind) {
        return m.row.modulation.clone();
    }
    let label = match rec.kind.as_str() {
        "fm_demod" | "nbfm_demod" => Some("fm"),
        "am_demod" => Some("am"),
        "ssb_demod" => Some("ssb"),
        "wfm_demod" => Some("wfm"),
        k if k == BITS_KIND || k == GRAPH_KIND => {
            rec.graph.nodes.iter().find_map(|n| match n.kind.as_str() {
                "fsk_demod" => Some("fsk"),
                "psk_demod" => Some("psk"),
                "ook_demod" => Some("ook"),
                _ => None,
            })
        }
        _ => None,
    };
    label.map(|l| vec![l.to_string()]).unwrap_or_default()
}

/// The modulation a measurement gives the origin query from its layer-1 `label`
/// (`fm`, `fsk`, ...): that label, none for no label or for `noise`.
pub fn measured_modulation(label: Option<&str>) -> Vec<String> {
    label
        .filter(|l| *l != "noise")
        .map(|l| vec![l.to_string()])
        .unwrap_or_default()
}

impl Inner {
    /// `c` with its origins scored: the band `center_hz` and `bandwidth_hz` of `source`,
    /// the modulation `c` measured ([`measured_modulation`]). A `source` the node no
    /// longer has reads as local ([`vantage_for`]).
    pub(crate) async fn score_classification(
        &self,
        mut c: Classification,
        source: &SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
    ) -> Classification {
        let q = OriginQuery {
            source: source.clone(),
            center_hz,
            bandwidth_hz,
            modulation: measured_modulation(
                c.labels
                    .iter()
                    .find(|l| l.layer == 1)
                    .map(|l| l.name.as_str()),
            ),
            symbol_rate_baud: None,
            job: None,
            at: now(),
        };
        c.origins = Some(origins(self, &q).await);
        c
    }

    /// [`crate::ops::classify_capture`] with the origins scored from the capture's own
    /// source and band.
    pub(crate) async fn classify_capture_scored(
        &self,
        rec: &CaptureRecord,
    ) -> Result<Classification, String> {
        let set = self.classifiers.read().unwrap().clone();
        let c = crate::ops::classify_capture(&set, rec).await?;
        Ok(self
            .score_classification(c, &rec.source, rec.center_hz, rec.bandwidth_hz)
            .await)
    }
}

impl Node {
    /// [`Inner::score_classification`].
    pub(crate) async fn score_classification(
        &self,
        c: Classification,
        source: &SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
    ) -> Classification {
        self.inner
            .score_classification(c, source, center_hz, bandwidth_hz)
            .await
    }

    /// [`Inner::classify_capture_scored`].
    pub(crate) async fn classify_capture_scored(
        &self,
        rec: &CaptureRecord,
    ) -> Result<Classification, String> {
        self.inner.classify_capture_scored(rec).await
    }

    /// The `origin_lookup` op (design 4.2): by `job` alone, or by `source`, `center_hz`
    /// and `bandwidth_hz` together (anything else is refused, [`ORIGIN_FORMS`]). The band
    /// form only answers, `modulation` `None` reading as unknown, and refuses a source
    /// the node does not have ([`NodeError::UnknownSource`]) and a band of no width. The
    /// job form reads the band from the job ([`Inner::origin_job`]), publishes the answer
    /// and keeps it.
    pub async fn origin_lookup(
        &self,
        job: Option<JobId>,
        source: Option<SourceId>,
        center_hz: Option<Hz>,
        bandwidth_hz: Option<Hz>,
        modulation: Option<Vec<String>>,
    ) -> Result<OriginAnswer, NodeError> {
        match (job, source, center_hz, bandwidth_hz) {
            (Some(job), None, None, None) => self.inner.origin_job(&job, modulation).await,
            (None, Some(source), Some(center_hz), Some(bandwidth_hz)) => {
                if self.inner.port(&source).is_none() {
                    return Err(NodeError::UnknownSource(source));
                }
                if bandwidth_hz.0 == 0 {
                    return Err(NodeError::Other(
                        "origin_lookup: a band of no width".to_string(),
                    ));
                }
                let q = OriginQuery {
                    source,
                    center_hz,
                    bandwidth_hz,
                    modulation: modulation.unwrap_or_default(),
                    symbol_rate_baud: None,
                    job: None,
                    at: now(),
                };
                Ok(origins(&self.inner, &q).await)
            }
            _ => Err(NodeError::Other(ORIGIN_FORMS.to_string())),
        }
    }
}

impl Inner {
    /// The job form of `origin_lookup`: the band is the job's `tuning` (an attached
    /// job copies its input's; a `bits` job's is its capture's, and the vantage is the
    /// capture's source, `context.bits.source`, its symbol rate the plan's);
    /// `modulation` `None` is [`job_modulation`]. The answer is published as one
    /// `origin` event ([`Inner::publish_origin`]) and merged into the job's context as
    /// `origins`. `UnknownJob` for a job the node does not know; `Other` for a kind in
    /// [`ORIGIN_NO_BAND_KINDS`] or a band of no width.
    async fn origin_job(
        &self,
        id: &JobId,
        modulation: Option<Vec<String>>,
    ) -> Result<OriginAnswer, NodeError> {
        let rec = self
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.record.clone())
            .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
        if ORIGIN_NO_BAND_KINDS.contains(&rec.kind.as_str()) || rec.tuning.bandwidth_hz.0 == 0 {
            return Err(NodeError::Other(format!(
                "origin_lookup: a {} job has no band",
                rec.kind
            )));
        }
        let bits = (rec.kind == BITS_KIND)
            .then(|| rec.context.get("bits"))
            .flatten();
        let source = bits
            .and_then(|b| serde_json::from_value::<SourceId>(b["source"].clone()).ok())
            .unwrap_or_else(|| rec.source.clone());
        let symbol_rate_baud =
            bits.and_then(|b| serde_json::from_value::<Baud>(b["symbol_rate_baud"].clone()).ok());
        let q = OriginQuery {
            source,
            center_hz: rec.tuning.center_hz,
            bandwidth_hz: rec.tuning.bandwidth_hz,
            modulation: modulation.unwrap_or_else(|| job_modulation(&rec, &self.modes())),
            symbol_rate_baud,
            job: Some(id.clone()),
            at: now(),
        };
        let a = origins(self, &q).await;
        self.keep_origin(&rec.id, &rec.source, &a)
    }

    /// Merge `a` into job `id`'s context as `origins`, then publish it as one `origin`
    /// event (design 4.3) on the job's event output, and so on its source's feed; nothing
    /// is published when the merge fails (the job is gone). A job with no event output
    /// registered now (an audio demodulator, a job that ended) publishes on a stream
    /// registered for the one frame, with the job's provenance, so the feed still
    /// carries it. `source` is the job's own source.
    /// The kept answer's confirmations carry over ([`carry_confirmed`]), so one that
    /// landed during the register round trip is not lost; the answer as kept.
    pub(super) fn keep_origin(
        &self,
        id: &JobId,
        source: &SourceId,
        a: &OriginAnswer,
    ) -> Result<OriginAnswer, NodeError> {
        let mut out = a.clone();
        self.update_origins(id, source, |kept| {
            if let Some(kept) = kept {
                carry_confirmed(&kept, &mut out);
            }
            Some((out.clone(), summary(&out)))
        })?;
        Ok(out)
    }

    /// Under one hold of the jobs lock: hand `update` job `id`'s kept `origins` (`None`
    /// when there are none, or they do not parse) and, when it gives an answer and a
    /// summary, keep that answer as `origins` (persisted) and publish it as one `origin`
    /// event ([`Inner::publish_origin`]). Holding the lock throughout, a lookup and a
    /// confirmation never overwrite each other, and the frames go out in the order the
    /// context changed. `Ok(false)` when `update` gave nothing; `UnknownJob` for a job
    /// the node does not know; a failed persist publishes nothing.
    fn update_origins(
        &self,
        id: &JobId,
        source: &SourceId,
        update: impl FnOnce(Option<OriginAnswer>) -> Option<(OriginAnswer, String)>,
    ) -> Result<bool, NodeError> {
        let mut jobs = self.jobs.lock().unwrap();
        let entry = jobs
            .get_mut(id)
            .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
        let kept = entry
            .record
            .context
            .get("origins")
            .and_then(|v| serde_json::from_value::<OriginAnswer>(v.clone()).ok());
        let Some((a, summary)) = update(kept) else {
            return Ok(false);
        };
        let ctx = &mut entry.record.context;
        if !ctx.is_object() {
            *ctx = serde_json::json!({});
        }
        let value = serde_json::to_value(&a).expect("an origin answer serialises");
        ctx.as_object_mut()
            .expect("context is an object")
            .insert("origins".into(), value);
        persist(&self.jobs_dir, &entry.record)?;
        let out = entry
            .outputs
            .iter()
            .find(|(k, s)| *k == StreamKind::Event && self.bus.receiver_count(s).is_some())
            .map(|(_, s)| s.clone());
        self.publish_origin(out, id, source, &a, summary);
        Ok(true)
    }

    /// A decoded identity `value` (from the event field `field`) seen by job `id`
    /// (design 4.6): every kept origin whose callsign is `value` is confirmed
    /// ([`confirm_identity`]), the answer kept and republished with `summary`
    /// `confirmed <value>: <licensee>, <site>`; true then. False, changing nothing, for
    /// no kept `origins`, no match (confirm-only), or matches all confirmed by `value`
    /// already (a repeat).
    pub(super) fn confirm_origin(
        &self,
        id: &JobId,
        source: &SourceId,
        field: &str,
        value: &str,
    ) -> bool {
        self.update_origins(id, source, |kept| {
            let mut a = kept?;
            let summary = confirm_identity(&mut a, field, value)?;
            Some((a, summary))
        })
        .unwrap_or(false)
    }

    /// Publish `a` as one `origin` event on `out`, the job's event output, or, with none
    /// (an audio demodulator, a job that ended), on a stream registered for the one frame
    /// with the job's provenance, so the source's feed still carries it.
    fn publish_origin(
        &self,
        out: Option<StreamId>,
        id: &JobId,
        source: &SourceId,
        a: &OriginAnswer,
        summary: String,
    ) {
        let payload = serde_json::json!({
            "protocol": "origin",
            "job": id.0,
            "source": a.source.0,
            "band": a.band,
            "modulation": a.modulation,
            "from": a.from,
            "from_note": a.from_note,
            "origins": a.origins,
            "providers": a.providers,
            "register_rows": a.register_rows,
            "receivers": a.receivers,
            "plans": a.plans,
            "reach": a.reach,
            "summary": summary,
            "t_ns": a.t_ns.0,
        });
        let prov = Provenance {
            source: source.clone(),
            job: Some(id.clone()),
            channel: None,
            span: Span {
                start_ns: a.t_ns,
                end_ns: a.t_ns,
            },
        };
        let (stream, transient) = match out {
            Some(s) => (s, false),
            None => (self.bus.register(StreamKind::Event, prov.clone()), true),
        };
        self.bus
            .publish(Frame::new(stream.clone(), prov, Payload::Event(payload)));
        if transient {
            self.bus.unregister(&stream);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emission::parse_emission;
    use crate::modes::score::DESIGNATOR_LABELS;
    use crate::port::{IqReceiver, Port, PortError, Tuning};
    use crate::types::{Capabilities, Claim, PositionFrom, SampleFormat, SourcePosition};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    const CENTER: u64 = 403_025_000;

    fn tx(emission: Option<&str>, bw: Option<u64>, center: u64, km: Option<f64>) -> Allocation {
        Allocation {
            provider: "acma".into(),
            region: "AU".into(),
            min_hz: Hz(center),
            max_hz: Hz(center),
            service: "Land Mobile".into(),
            note: None,
            kind: Some("assignment".into()),
            licensee: Some("Example Water Authority".into()),
            site: Some("Example Hill".into()),
            lat: Some(-27.35),
            lon: Some(153.08),
            distance_km: km,
            bearing_deg: km.map(|_| 315.0),
            power_w: None,
            center_hz: Some(Hz(center)),
            bandwidth_hz: bw.map(Hz),
            licence_no: Some("0000001".into()),
            callsign: None,
            role: Some("tx".into()),
            emission: emission.map(str::to_string),
            emission_decoded: emission.and_then(parse_emission),
            as_of: None,
        }
    }

    fn q(modulation: &[&str], center: u64, bw: u64) -> OriginQuery {
        OriginQuery {
            source: SourceId("radio".into()),
            center_hz: Hz(center),
            bandwidth_hz: Hz(bw),
            modulation: modulation.iter().map(|m| m.to_string()).collect(),
            symbol_rate_baud: None,
            job: None,
            at: Timestamp(0),
        }
    }

    fn reach(km: Option<f64>) -> Reach {
        Reach {
            km,
            class: crate::reach::ReachClass::Uhf,
            antenna: "discone".into(),
            note: None,
        }
    }

    fn demo_station() -> Station {
        Station {
            lat: -27.3842,
            lon: 153.1175,
            alt_m: 4.0,
            region: "AU".into(),
            antennas: vec![],
        }
    }

    fn empty_answer(origins: Vec<Origin>, register_rows: Option<u32>) -> OriginAnswer {
        OriginAnswer {
            origins,
            band: SignalBand {
                center_hz: Hz(CENTER),
                bandwidth_hz: Hz(12_500),
            },
            source: SourceId("radio".into()),
            modulation: vec![],
            symbol_rate_baud: None,
            from: None,
            from_note: None,
            providers: register_rows.map_or_else(Vec::new, |_| vec!["acma".into()]),
            register_rows,
            receivers: 0,
            plans: 0,
            reach: None,
            t_ns: Timestamp(0),
        }
    }

    #[test]
    fn the_frequency_gate_uses_the_designator_width_then_the_row_width_then_zero() {
        let band = q(&[], CENTER, 12_500);
        // 16K0: 8000 + 6250 >= 10000.
        assert!(
            score_row(
                &tx(Some("16K0F1D"), None, CENTER + 10_000, None),
                &band,
                None,
                None
            )
            .is_some()
        );
        // The designator wins over the row's own width.
        assert!(
            score_row(
                &tx(Some("16K0F1D"), Some(1_000), CENTER + 10_000, None),
                &band,
                None,
                None
            )
            .is_some()
        );
        // The row's width when there is no designator: 10000 + 6250 >= 10000.
        assert!(
            score_row(
                &tx(None, Some(20_000), CENTER + 10_000, None),
                &band,
                None,
                None
            )
            .is_some()
        );
        // No width at all: 0 + 6250 < 7000.
        assert!(score_row(&tx(None, None, CENTER + 7_000, None), &band, None, None).is_none());
        assert!(score_row(&tx(None, None, CENTER + 6_000, None), &band, None, None).is_some());
        assert!(score_row(&tx(None, None, CENTER - 6_000, None), &band, None, None).is_some());
    }

    #[test]
    fn width_grades_are_0_4_0_2_0_and_0_1() {
        let band = q(&[], CENTER, 12_500);
        let width = |bw: Option<u64>| {
            score_row(&tx(None, bw, CENTER, None), &band, None, None)
                .unwrap()
                .terms
                .width
        };
        assert_eq!(width(Some(12_500)), ORIGIN_WIDTH_FULL);
        assert_eq!(width(Some(37_500)), ORIGIN_WIDTH_HALF);
        assert_eq!(width(Some(100_000)), 0.0);
        assert_eq!(width(None), ORIGIN_WIDTH_NONE);
        assert_eq!(
            (ORIGIN_WIDTH_FULL, ORIGIN_WIDTH_HALF, ORIGIN_WIDTH_NONE),
            (0.4, 0.2, 0.1)
        );
    }

    /// The field host's 2026-11-08 band lookup: a 200 kHz band measured at 97.893 MHz gave
    /// the on-channel 97.9 MHz station and the 97.7 MHz rows (193 kHz off, inside the gate
    /// only by their edge) the same 0.80. The width term is graded by how far the row's
    /// centre sits inside the gate, so the fitting site is the brightest.
    #[test]
    fn an_on_channel_row_outscores_one_that_only_overlaps_the_band_edge() {
        let band = q(&["fm"], 97_893_000, 200_000);
        let row = |center| {
            score_row(
                &tx(Some("200KF8E"), None, center, Some(10.0)),
                &band,
                Some(&reach(Some(100.0))),
                None,
            )
            .unwrap()
        };
        let on = row(97_900_000);
        let edge = row(97_700_000);
        assert_eq!(on.agree, Agree::Fits);
        assert_eq!(edge.agree, Agree::Fits);
        // 7 kHz off, against a 200 kHz reach: the width term barely moves.
        assert!(on.terms.width > 0.38, "{:?}", on.terms);
        // 193 kHz off, only just inside: next to nothing.
        assert!(edge.terms.width < 0.05, "{:?}", edge.terms);
        assert!(
            on.score > edge.score + 0.3,
            "{} vs {}",
            on.score,
            edge.score
        );
        // The other terms keep their weights (10 km of a 100 km reach: 0.19), and the
        // score stays rounded.
        assert_eq!((on.terms.modulation, on.terms.reach), (0.4, 0.19));
        assert_eq!((edge.terms.modulation, edge.terms.reach), (0.4, 0.19));
        assert_eq!(on.score, round_score(on.score));
        // At the band's centre the term is the full grade.
        assert_eq!(row(97_893_000).terms.width, ORIGIN_WIDTH_FULL);
    }

    /// The field host's 2026-11-08 FM lookup: a 97.9 MHz station 5 km away tied at 0.79
    /// with one 266 km away against a 300 km reach. The reach term is graded by distance
    /// over the reach: full at the station, half at the limit, none beyond.
    #[test]
    fn the_near_on_channel_row_outranks_the_far_one_within_reach() {
        let band = q(&["wfm"], 97_893_000, 200_000);
        let r = reach(Some(300.0));
        let row = |km| {
            score_row(
                &tx(Some("200KF8E"), None, 97_900_000, Some(km)),
                &band,
                Some(&r),
                None,
            )
            .unwrap()
        };
        let (near, far) = (row(5.0), row(266.0));
        assert!(near.score > far.score, "{} vs {}", near.score, far.score);
        assert!(
            near.terms.reach > far.terms.reach,
            "{:?} {:?}",
            near.terms,
            far.terms
        );
        // Full at the station, half the term at the reach limit, none beyond.
        assert_eq!(row(0.0).terms.reach, ORIGIN_REACH_IN);
        assert_eq!(
            row(300.0).terms.reach,
            round_score(ORIGIN_REACH_IN * ORIGIN_REACH_FLOOR)
        );
        assert_eq!(row(301.0).terms.reach, 0.0);
        assert_eq!(row(150.0).terms.reach, 0.15);
        // The term and the score stay rounded.
        assert_eq!(far.terms.reach, round_score(far.terms.reach));
        assert_eq!(far.score, round_score(far.score));
        // A reach with no radius holds any known distance at the full term.
        let any = score_row(
            &tx(Some("200KF8E"), None, 97_900_000, Some(266.0)),
            &band,
            Some(&reach(None)),
            None,
        )
        .unwrap();
        assert_eq!(any.terms.reach, ORIGIN_REACH_IN);
    }

    #[test]
    fn modulation_fits_disagrees_unknown_against_each_letter() {
        let all: Vec<&str> = DESIGNATOR_LABELS
            .iter()
            .flat_map(|(_, l)| l.iter().copied())
            .collect();
        for (letter, labels) in DESIGNATOR_LABELS {
            let designator = format!("16K0{letter}3E");
            let row = tx(Some(&designator), None, CENTER, None);
            assert_eq!(row.emission_decoded.as_ref().unwrap().modulation, *letter);
            let fit = score_row(&row, &q(&[labels[0]], CENTER, 16_000), None, None).unwrap();
            assert_eq!(
                (fit.agree, fit.terms.modulation),
                (Agree::Fits, ORIGIN_MODULATION_FITS),
                "{letter}"
            );
            let other = all.iter().find(|l| !labels.contains(l)).unwrap();
            let bad = score_row(&row, &q(&[other], CENTER, 16_000), None, None).unwrap();
            assert_eq!(
                (bad.agree, bad.terms.modulation),
                (Agree::Disagrees, 0.0),
                "{letter}"
            );
            let none = score_row(&row, &q(&[], CENTER, 16_000), None, None).unwrap();
            assert_eq!(
                (none.agree, none.terms.modulation),
                (Agree::Unknown, ORIGIN_MODULATION_UNKNOWN)
            );
        }
        let bare = score_row(
            &tx(None, Some(16_000), CENTER, None),
            &q(&["fm"], CENTER, 16_000),
            None,
            None,
        )
        .unwrap();
        assert_eq!((bare.agree, bare.terms.modulation), (Agree::Unknown, 0.2));
    }

    /// The field host's broadcast band lookups: layer 1 labels a broadcast FM station
    /// `wfm`, and an F8E (or F3E) row must agree with it rather than disagree. The other
    /// letters cover the labels layer 1 can emit for their emissions too: `A` keyed
    /// (A1A, A1D, A2D) reads as `ook`; `carrier` stays `N`'s alone.
    #[test]
    fn a_wfm_band_agrees_with_an_f8e_row_and_keyed_a_rows_with_ook() {
        let fits = |designator: &str, label: &str, bw: u64| {
            let row = tx(Some(designator), None, 97_900_000, Some(5.0));
            score_row(&row, &q(&[label], 97_900_000, bw), None, None)
                .unwrap()
                .agree
        };
        assert_eq!(fits("200KF8E", "wfm", 200_000), Agree::Fits);
        assert_eq!(fits("180KF3E", "wfm", 200_000), Agree::Fits);
        assert_eq!(fits("16K0A1D", "ook", 16_000), Agree::Fits);
        // A bare carrier (or a spur) does not agree with an AM row.
        assert_eq!(fits("12K5A3E", "carrier", 12_500), Agree::Disagrees);
        // Phase modulation and the sidebands still disagree with a broadcast label.
        assert_eq!(fits("16K0G3E", "wfm", 16_000), Agree::Disagrees);
        assert_eq!(fits("200KJ3E", "wfm", 200_000), Agree::Disagrees);
    }

    #[test]
    fn reach_scores_in_unknown_and_out() {
        let band = q(&[], CENTER, 12_500);
        let r = reach(Some(50.0));
        let term = |km: Option<f64>| {
            score_row(&tx(None, Some(12_500), CENTER, km), &band, Some(&r), None)
                .unwrap()
                .terms
                .reach
        };
        assert_eq!(term(Some(0.0)), ORIGIN_REACH_IN);
        // 9 km of 50: 0.2 * (1 - 0.5 * 0.18).
        assert_eq!(term(Some(9.0)), 0.18);
        assert_eq!(term(None), ORIGIN_REACH_UNKNOWN);
        assert_eq!(term(Some(90.0)), 0.0);
        assert_eq!((ORIGIN_REACH_IN, ORIGIN_REACH_UNKNOWN), (0.2, 0.1));
        let full = score_row(
            &tx(Some("12K5F3E"), None, CENTER, Some(0.0)),
            &q(&["fm"], CENTER, 12_500),
            Some(&r),
            None,
        )
        .unwrap();
        assert!((full.score - 1.0).abs() < 1e-6, "{}", full.score);
    }

    #[test]
    fn a_width_less_row_lights_unknown() {
        let o = score_row(
            &tx(None, None, CENTER, None),
            &q(&["fm"], CENTER, 12_500),
            None,
            None,
        )
        .unwrap();
        assert_eq!(o.agree, Agree::Unknown);
        assert_eq!(o.terms.width, ORIGIN_WIDTH_NONE);
        assert!(o.confirmed.is_none());
    }

    #[test]
    fn a_receiver_or_plan_row_is_never_an_origin() {
        let mut rx = tx(None, Some(12_500), CENTER, None);
        rx.role = Some("rx".into());
        assert!(score_row(&rx, &q(&[], CENTER, 12_500), None, None).is_none());
        let mut plan = tx(None, Some(12_500), CENTER, None);
        plan.kind = Some("plan".into());
        assert!(score_row(&plan, &q(&[], CENTER, 12_500), None, None).is_none());
    }

    #[test]
    fn receivers_and_plans_are_counted_not_scored() {
        let mut rx = tx(None, Some(12_500), CENTER, None);
        rx.role = Some("rx".into());
        let mut plan = tx(None, None, CENTER, None);
        plan.kind = Some("plan".into());
        plan.role = None;
        let rows = vec![tx(None, Some(12_500), CENTER, None), rx, plan];
        let a = answer(
            &q(&[], CENTER, 12_500),
            rows,
            None,
            &Vantage::None("no station set"),
            vec!["acma".into()],
            None,
        );
        assert_eq!((a.origins.len(), a.receivers, a.plans), (1, 1, 1));
        assert_eq!(a.from_note.as_deref(), Some("no station set"));
        assert!(a.from.is_none());
    }

    #[test]
    fn dedup_keeps_the_best_and_the_cap_orders_best_then_nearest() {
        let band = q(&[], CENTER, 12_500);
        let rows = vec![
            tx(None, Some(100_000), CENTER, None),
            tx(None, Some(12_500), CENTER, None),
        ];
        let a = answer(
            &band,
            rows,
            None,
            &Vantage::None("no station set"),
            vec![],
            None,
        );
        assert_eq!(a.origins.len(), 1);
        assert_eq!(a.origins[0].terms.width, ORIGIN_WIDTH_FULL);

        let wide = q(&[], CENTER, 1_000_000);
        let rows: Vec<Allocation> = (0..60u64)
            .map(|i| {
                // Three widths (three scores), distances mixed with some unknown.
                let bw = [1_000_000, 300_000, 5_000_000][(i % 3) as usize];
                let km = (i % 4 != 0).then_some(((i * 7) % 23) as f64);
                tx(None, Some(bw), CENTER + i * 1_000, km)
            })
            .collect();
        let a = answer(
            &wide,
            rows,
            Some(reach(Some(500.0))),
            &Vantage::None("remote source without a position"),
            vec![],
            None,
        );
        assert_eq!(a.origins.len(), ORIGIN_SITES);
        assert_eq!(ORIGIN_SITES, 50);
        let mut ties = 0;
        for w in a.origins.windows(2) {
            assert!(w[0].score >= w[1].score);
            if w[0].score == w[1].score {
                ties += 1;
                match (w[0].allocation.distance_km, w[1].allocation.distance_km) {
                    (Some(x), Some(y)) => assert!(x <= y),
                    (None, Some(_)) => panic!("None before a distance"),
                    _ => {}
                }
            }
        }
        assert!(ties > 0);
    }

    #[test]
    fn confirm_sets_score_one_and_keeps_terms() {
        let band = q(&["fm"], CENTER, 12_500);
        let plain = score_row(&tx(None, Some(12_500), CENTER, None), &band, None, None).unwrap();
        let mut row = tx(Some("16K0F3E"), None, CENTER, Some(17.0));
        row.callsign = Some("XX4EX".into());
        let marked = score_row(&row, &band, None, None).unwrap();
        let mut a = empty_answer(vec![plain, marked.clone()], Some(2));
        let before = a.clone();
        assert!(confirm(&mut a, "callsign", "XX4ZZ").is_empty());
        assert_eq!(a, before);
        assert_eq!(confirm(&mut a, "callsign", " xx4ex "), vec![1]);
        assert_eq!(a.origins[1].score, 1.0);
        assert_eq!(a.origins[1].terms, marked.terms);
        assert_eq!(
            a.origins[1].confirmed,
            Some(Confirmed {
                field: "callsign".into(),
                value: " xx4ex ".into()
            })
        );
        assert_eq!(a.origins[0], before.origins[0]);
    }

    #[test]
    fn the_schemas_require_every_field_and_round_trip() {
        let schema = serde_json::to_value(schemars::schema_for!(OriginAnswer)).unwrap();
        let mut required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        required.sort_unstable();
        let mut props: Vec<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        props.sort_unstable();
        assert_eq!(required, props);
        assert_eq!(schema["properties"]["from"]["type"][1], "null");
        let query = serde_json::to_value(schemars::schema_for!(OriginQuery)).unwrap();
        assert_eq!(query["required"].as_array().unwrap().len(), 7);
        let band = q(&["fm"], CENTER, 12_500);
        let o = score_row(&tx(Some("16K0F3E"), None, CENTER, None), &band, None, None).unwrap();
        let a = answer(
            &band,
            vec![o.allocation.clone()],
            None,
            &Vantage::Station(demo_station()),
            vec!["acma".into()],
            None,
        );
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["origins"][0]["agree"], "fits");
        assert_eq!(v["from"]["kind"], "station");
        assert_eq!(serde_json::from_value::<OriginAnswer>(v).unwrap(), a);
        let mut extra = serde_json::to_value(&band).unwrap();
        extra["extra"] = 1.into();
        assert!(serde_json::from_value::<OriginQuery>(extra).is_err());
    }

    #[test]
    fn a_noise_or_missing_label_gives_no_modulation() {
        assert_eq!(measured_modulation(Some("fm")), vec!["fm".to_string()]);
        assert!(measured_modulation(Some("noise")).is_empty());
        assert!(measured_modulation(None).is_empty());
    }

    #[test]
    fn summary_has_four_forms() {
        let band = q(&["fm"], CENTER, 12_500);
        let near = tx(Some("12K5F3E"), None, CENTER, Some(17.2));
        let mut second = tx(Some("12K5F3E"), None, CENTER, None);
        second.licensee = Some("Example Broadcasting".into());
        second.site = Some("Example Ridge".into());
        second.licence_no = Some("0000002".into());
        let mut origins: Vec<Origin> = [near, second.clone(), second.clone(), second]
            .iter()
            .map(|r| score_row(r, &band, Some(&reach(Some(150.0))), None).unwrap())
            .collect();
        origins[0].score = 0.9;
        let a = empty_answer(origins, Some(4));
        let s = summary(&a);
        assert!(
            s.starts_with(
                "4 sites on 403.025 MHz: Example Water Authority, Example Hill 17 km fits 0.9; \
                 Example Broadcasting, Example Ridge fits 0.9; "
            ),
            "{s}"
        );
        assert_eq!(s.matches("; ").count(), ORIGIN_SUMMARY_SITES - 1, "{s}");
        assert_eq!(
            summary(&empty_answer(vec![], Some(3))),
            "no licensed transmitter on 403.025 MHz"
        );
        assert_eq!(
            summary(&empty_answer(vec![], Some(0))),
            "register gave no rows"
        );
        assert_eq!(summary(&empty_answer(vec![], None)), "no register provider");
    }

    #[test]
    fn identity_value_is_stripped_and_compared_case_insensitively() {
        let ssid = Identity {
            field: "src".into(),
            register: "callsign".into(),
            strip: Some("ssid".into()),
        };
        let plain = Identity {
            strip: None,
            ..ssid.clone()
        };
        let v = |s: &str| serde_json::Value::from(s);
        assert_eq!(
            identity_value(&ssid, &v("xx4ex-7")).as_deref(),
            Some("XX4EX")
        );
        assert_eq!(
            identity_value(&ssid, &v(" XX4EX-15 ")).as_deref(),
            Some("XX4EX")
        );
        assert_eq!(identity_value(&ssid, &v("XX4EX")).as_deref(), Some("XX4EX"));
        // Only a numeric `-N` is an SSID.
        assert_eq!(
            identity_value(&ssid, &v("XX4EX-A")).as_deref(),
            Some("XX4EX-A")
        );
        assert_eq!(
            identity_value(&plain, &v("XX4EX-7")).as_deref(),
            Some("XX4EX-7")
        );
        assert_eq!(identity_value(&ssid, &v("  ")), None);
        assert_eq!(identity_value(&ssid, &v("-7")), None);
        assert_eq!(identity_value(&ssid, &serde_json::Value::Null), None);
        assert_eq!(identity_value(&ssid, &serde_json::json!(7)), None);
    }

    #[test]
    fn a_fresh_answer_keeps_the_confirmations_of_the_kept_one() {
        let band = q(&["fm"], CENTER, 12_500);
        let mut best = tx(Some("12K5F3E"), None, CENTER, Some(5.0));
        best.site = Some("Example Ridge".into());
        best.licence_no = Some("0000002".into());
        let mut marked = tx(Some("16K0F1D"), None, CENTER, Some(17.0));
        marked.callsign = Some("XX4EX".into());
        let origins: Vec<Origin> = [best, marked]
            .iter()
            .map(|r| score_row(r, &band, None, None).unwrap())
            .collect();
        let fresh = empty_answer(origins, Some(2));
        // The kept answer was confirmed while the fresh lookup was out.
        let mut kept = fresh.clone();
        confirm_identity(&mut kept, "src", "XX4EX").unwrap();
        let mut a = fresh.clone();
        carry_confirmed(&kept, &mut a);
        assert_eq!(a.origins[0].allocation.callsign.as_deref(), Some("XX4EX"));
        assert_eq!(a.origins[0].score, 1.0);
        assert_eq!(
            a.origins[0].confirmed,
            Some(Confirmed {
                field: "src".into(),
                value: "XX4EX".into()
            })
        );
        assert!(a.origins[1].confirmed.is_none());
        // Nothing confirmed kept: the fresh answer is left as it is.
        let mut b = fresh.clone();
        carry_confirmed(&fresh, &mut b);
        assert_eq!(b, fresh);
    }

    #[test]
    fn a_confirmed_origin_goes_first_and_names_itself() {
        let band = q(&["fm"], CENTER, 12_500);
        let mut best = tx(Some("12K5F3E"), None, CENTER, Some(5.0));
        best.licensee = Some("Example Broadcasting".into());
        best.site = Some("Example Ridge".into());
        best.licence_no = Some("0000002".into());
        let mut marked = tx(Some("16K0F1D"), None, CENTER, Some(17.0));
        marked.callsign = Some("XX4EX".into());
        // The same callsign licensed at a second, nearer site.
        let mut second = marked.clone();
        second.site = Some("Example Ridge".into());
        second.licence_no = Some("0000003".into());
        second.distance_km = Some(9.0);
        let origins = [best, marked, second]
            .iter()
            .map(|r| score_row(r, &band, None, None).unwrap())
            .collect();
        let mut a = empty_answer(origins, Some(3));
        assert_eq!(confirm_identity(&mut a, "src", "XX4ZZ"), None);
        assert!(a.origins.iter().all(|o| o.confirmed.is_none()));
        let s = confirm_identity(&mut a, "src", "XX4EX").unwrap();
        // Every match is confirmed; the summary names the first after the sort.
        assert_eq!(s, "confirmed XX4EX: Example Water Authority, Example Ridge");
        let sites: Vec<_> = a
            .origins
            .iter()
            .map(|o| o.allocation.site.as_deref())
            .collect();
        assert_eq!(
            sites,
            [
                Some("Example Ridge"),
                Some("Example Hill"),
                Some("Example Ridge")
            ]
        );
        for o in &a.origins[..2] {
            assert_eq!(o.allocation.callsign.as_deref(), Some("XX4EX"));
            assert_eq!(o.score, 1.0);
            assert_eq!(
                o.confirmed,
                Some(Confirmed {
                    field: "src".into(),
                    value: "XX4EX".into()
                })
            );
        }
        assert_eq!(a.origins[2].confirmed, None);
        // A repeat changes nothing, so nothing is republished.
        let before = a.clone();
        assert_eq!(confirm_identity(&mut a, "src", "XX4EX"), None);
        assert_eq!(a, before);
    }

    struct TestPort {
        id: SourceId,
        caps: Capabilities,
        position: Option<SourcePosition>,
        tx: tokio::sync::broadcast::Sender<crate::port::IqBlock>,
    }

    impl TestPort {
        fn port(id: &str, remote: bool, position: Option<SourcePosition>) -> Arc<dyn Port> {
            Arc::new(TestPort {
                id: SourceId(id.into()),
                caps: Capabilities {
                    tune_min_hz: Claim::Asserted(Hz(0)),
                    tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                    max_bandwidth_hz: Claim::Asserted(Hz(2_000_000)),
                    formats: vec![SampleFormat::Cf32],
                    channels: 1,
                    rx: true,
                    tx: false,
                    remote,
                    push: false,
                },
                position,
                tx: tokio::sync::broadcast::channel(1).0,
            })
        }
    }

    #[async_trait]
    impl Port for TestPort {
        fn id(&self) -> &SourceId {
            &self.id
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
            Ok(t)
        }
        fn current(&self) -> Option<Tuning> {
            None
        }
        fn subscribe(&self) -> IqReceiver {
            self.tx.subscribe()
        }
        fn position(&self) -> Option<SourcePosition> {
            self.position
        }
    }

    /// A position near the demo station (Brisbane Airport), not any real receiver's.
    const KIWI: SourcePosition = SourcePosition {
        lat: -27.47,
        lon: 153.02,
        alt_m: Some(40.0),
        from: PositionFrom::Status,
    };

    async fn node(dir: &std::path::Path) -> super::super::Node {
        node_with(dir, Default::default()).await
    }

    async fn node_with(
        dir: &std::path::Path,
        source_positions: std::collections::BTreeMap<SourceId, SourcePosition>,
    ) -> super::super::Node {
        let cfg = super::super::NodeConfig {
            source_positions,
            burst_policy: Default::default(),
            state_dir: dir.join("state"),
            ring_seconds: 1.0,
            ring_budget_bytes: u64::MAX,
            scan_dwell_ms: crate::ops::SCAN_DWELL_MS,
            backends: vec![crate::stage::Backend::Cpu],
            resume: false,
            bank_factory: None,
            modes_dir: None,
            sets_dir: None,
        };
        let ports = vec![
            TestPort::port("radio", false, None),
            TestPort::port("kiwi", true, Some(KIWI)),
            TestPort::port("peer", true, None),
        ];
        let n = super::super::Node::open(cfg, crate::stage::StageRegistry::new(), ports)
            .await
            .unwrap();
        n.set_station(demo_station()).await.unwrap();
        n
    }

    #[tokio::test]
    async fn vantage_for_local_remote_with_and_without_position() {
        let dir = tempfile::tempdir().unwrap();
        let n = node(dir.path()).await;
        assert_eq!(
            vantage_for(&n.inner, &SourceId("radio".into())),
            Vantage::Station(demo_station())
        );
        let kiwi = Station {
            lat: KIWI.lat,
            lon: KIWI.lon,
            alt_m: 40.0,
            region: "AU".into(),
            antennas: Station::default_antennas(40.0),
        };
        assert_eq!(
            vantage_for(&n.inner, &SourceId("kiwi".into())),
            Vantage::Source(kiwi, SourceId("kiwi".into()))
        );
        assert_eq!(
            vantage_for(&n.inner, &SourceId("peer".into())),
            Vantage::None("remote source without a position")
        );
        n.clear_station().await.unwrap();
        assert_eq!(
            vantage_for(&n.inner, &SourceId("radio".into())),
            Vantage::None("no station set")
        );
    }

    /// Records the `from` each `allocations_from` call saw, and answers one row.
    struct Recording(Mutex<Vec<Option<Station>>>);

    #[async_trait]
    impl KnowledgeProvider for Recording {
        fn name(&self) -> &str {
            "recording"
        }
        fn gives_assignments(&self) -> bool {
            true
        }
        async fn allocations(&self, _: &str, _: Hz, _: Hz) -> Vec<Allocation> {
            unreachable!("origins asks allocations_from")
        }
        async fn signatures(&self, _: Hz, _: Hz) -> Vec<crate::knowledge::Signature> {
            vec![]
        }
        async fn allocations_from(
            &self,
            _: &str,
            c: Hz,
            _: Hz,
            from: Option<&Station>,
        ) -> Vec<Allocation> {
            self.0.lock().unwrap().push(from.cloned());
            vec![tx(Some("12K5F3E"), None, c.0, Some(12.0))]
        }
        async fn reach_from(&self, _: &str, _: Hz, _: Hz, _: Option<&Station>) -> Option<Reach> {
            Some(reach(Some(150.0)))
        }
    }

    #[tokio::test]
    async fn origins_asks_allocations_from_with_the_vantage() {
        let dir = tempfile::tempdir().unwrap();
        let n = node(dir.path()).await;
        let p = Arc::new(Recording(Mutex::new(vec![])));
        n.add_knowledge(p.clone());
        let mut query = q(&["fm"], CENTER, 12_500);
        query.source = SourceId("kiwi".into());
        let a = origins(&n.inner, &query).await;
        let Vantage::Source(kiwi, _) = vantage_for(&n.inner, &query.source) else {
            panic!("the kiwi has a position");
        };
        assert_eq!(*p.0.lock().unwrap(), vec![Some(kiwi.clone())]);
        let from = a.from.clone().unwrap();
        assert_eq!(
            (from.lat, from.lon, from.kind, from.source),
            (
                kiwi.lat,
                kiwi.lon,
                FromKind::Source,
                Some(SourceId("kiwi".into()))
            )
        );
        assert_eq!(serde_json::to_value(&a).unwrap()["from"]["kind"], "source");
        assert_eq!(
            a.providers,
            vec!["learned".to_string(), "recording".to_string()]
        );
        assert_eq!(a.register_rows, Some(1));
        assert_eq!(a.origins.len(), 1);
        assert_eq!(a.reach, Some(reach(Some(150.0))));
        assert_eq!(a.source, SourceId("kiwi".into()));
        // The local radio asks from the station.
        query.source = SourceId("radio".into());
        let a = origins(&n.inner, &query).await;
        assert_eq!(p.0.lock().unwrap()[1], Some(demo_station()));
        assert_eq!(a.from.unwrap().kind, FromKind::Station);
    }

    /// A register that answers nothing (unreachable, or empty on the band).
    struct SilentRegister;

    #[async_trait]
    impl KnowledgeProvider for SilentRegister {
        fn name(&self) -> &str {
            "acma"
        }
        fn gives_assignments(&self) -> bool {
            true
        }
        async fn allocations(&self, _: &str, _: Hz, _: Hz) -> Vec<Allocation> {
            vec![]
        }
        async fn signatures(&self, _: Hz, _: Hz) -> Vec<crate::knowledge::Signature> {
            vec![]
        }
    }

    #[tokio::test]
    async fn a_plan_only_node_has_no_register_and_a_silent_register_gave_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        let n = node(dir.path()).await;
        let query = q(&["fm"], CENTER, 12_500);
        // Plans only: the built-in signatures and no assignment row.
        n.add_knowledge(Arc::new(crate::knowledge::StaticKnowledge::builtin()));
        let a = origins(&n.inner, &query).await;
        assert_eq!(
            a.providers,
            vec!["learned".to_string(), "static".to_string()]
        );
        assert_eq!(a.register_rows, None);
        assert_eq!(summary(&a), "no register provider");
        n.add_knowledge(Arc::new(SilentRegister));
        let a = origins(&n.inner, &query).await;
        assert_eq!(
            a.providers,
            vec![
                "learned".to_string(),
                "static".to_string(),
                "acma".to_string()
            ]
        );
        assert_eq!(a.register_rows, Some(0));
        assert_eq!(summary(&a), "register gave no rows");
    }

    #[test]
    fn a_static_provider_gives_assignments_only_with_an_assignment_row() {
        let dir = tempfile::tempdir().unwrap();
        let sigs = dir.path().join("signatures.json");
        std::fs::write(&sigs, "[]").unwrap();
        let allocs = dir.path().join("allocations.json");
        let mut plan = tx(None, None, CENTER, None);
        plan.kind = Some("plan".into());
        std::fs::write(&allocs, serde_json::to_string(&vec![plan]).unwrap()).unwrap();
        let plans = crate::knowledge::StaticKnowledge::from_files(&sigs, Some(&allocs)).unwrap();
        assert!(!plans.gives_assignments());
        let rows = vec![tx(None, Some(12_500), CENTER, None)];
        std::fs::write(&allocs, serde_json::to_string(&rows).unwrap()).unwrap();
        let register = crate::knowledge::StaticKnowledge::from_files(&sigs, Some(&allocs)).unwrap();
        assert!(register.gives_assignments());
    }

    #[test]
    fn the_reach_widens_inside_a_directional_antennas_sector() {
        let band = q(&[], CENTER, 12_500);
        let beam = Antenna {
            id: "yagi".into(),
            kind: crate::station::AntennaKind::Yagi,
            min_hz: Hz(400_000_000),
            max_hz: Hz(410_000_000),
            height_m: 10.0,
            azimuth_deg: Some(315.0),
            elevation_deg: None,
        };
        let mut r = reach(Some(50.0));
        r.antenna = "yagi".into();
        // 90 km on bearing 315: inside the sector the reach doubles to 100 km (0.11 of
        // the graded term, 0.2 * (1 - 0.5 * 0.9)).
        let row = tx(None, Some(12_500), CENTER, Some(90.0));
        let term = |row: &Allocation, ant: Option<&Antenna>| {
            score_row(row, &band, Some(&r), ant).unwrap().terms.reach
        };
        assert_eq!(term(&row, Some(&beam)), 0.11);
        // Without the antenna, or without a bearing, `reach.km` (50) decides.
        assert_eq!(term(&row, None), 0.0);
        let mut no_bearing = row.clone();
        no_bearing.bearing_deg = None;
        assert_eq!(term(&no_bearing, Some(&beam)), 0.0);
        // Behind the beam the reach halves to 25 km: 30 km is out.
        let mut behind = tx(None, Some(12_500), CENTER, Some(30.0));
        behind.bearing_deg = Some(135.0);
        assert_eq!(term(&behind, Some(&beam)), 0.0);
        // 30 km of 50: 0.2 * (1 - 0.5 * 0.6).
        assert_eq!(term(&behind, None), 0.14);

        // `answer` finds the antenna on the vantage's station by `reach.antenna`.
        let mut station = demo_station();
        station.antennas = vec![beam];
        let a = answer(
            &band,
            vec![row],
            Some(r.clone()),
            &Vantage::Station(station),
            vec!["acma".into()],
            Some(1),
        );
        assert_eq!(a.origins[0].terms.reach, 0.11);
    }

    #[tokio::test]
    async fn a_configured_position_gives_a_remote_source_its_vantage() {
        let dir = tempfile::tempdir().unwrap();
        let configured = SourcePosition {
            lat: -27.40,
            lon: 153.10,
            alt_m: None,
            from: PositionFrom::Configured,
        };
        let n = node_with(
            dir.path(),
            [(SourceId("peer".into()), configured)]
                .into_iter()
                .collect(),
        )
        .await;
        let want = Station {
            lat: -27.40,
            lon: 153.10,
            alt_m: 0.0,
            region: "AU".into(),
            antennas: Station::default_antennas(0.0),
        };
        assert_eq!(
            vantage_for(&n.inner, &SourceId("peer".into())),
            Vantage::Source(want, SourceId("peer".into()))
        );
    }

    /// Answers one `tx` row and does not say it gives assignments.
    struct Undeclared;

    #[async_trait]
    impl KnowledgeProvider for Undeclared {
        fn name(&self) -> &str {
            "undeclared"
        }
        async fn allocations(&self, _: &str, c: Hz, _: Hz) -> Vec<Allocation> {
            vec![tx(Some("12K5F3E"), None, c.0, Some(12.0))]
        }
        async fn signatures(&self, _: Hz, _: Hz) -> Vec<crate::knowledge::Signature> {
            vec![]
        }
    }

    #[tokio::test]
    async fn sites_from_an_undeclared_provider_are_listed_in_the_summary() {
        let dir = tempfile::tempdir().unwrap();
        let n = node(dir.path()).await;
        n.add_knowledge(Arc::new(Undeclared));
        let a = origins(&n.inner, &q(&["fm"], CENTER, 12_500)).await;
        assert_eq!((a.register_rows, a.origins.len()), (None, 1));
        let s = summary(&a);
        assert!(
            s.starts_with("1 site on 403.025 MHz: Example Water Authority, Example Hill 12 km"),
            "{s}"
        );
    }

    #[test]
    fn equal_sums_tie_and_order_by_distance() {
        let sum = |t: (f32, f32, f32)| round_score(t.0 + t.1 + t.2);
        assert_eq!(sum((0.1, 0.4, 0.2)), sum((0.4, 0.2, 0.1)));
        assert_eq!(sum((0.1, 0.4, 0.2)), 0.7);
        // With no label every modulation term is 0.2: a width-less row 5 km away
        // (0.1 + 0.2 + 0.2) ties a half-width row with no distance (0.2 + 0.2 + 0.1).
        let band = q(&[], CENTER, 12_500);
        let mut far = tx(None, Some(37_500), CENTER, None);
        far.licence_no = Some("0000002".into());
        let near = tx(None, None, CENTER, Some(5.0));
        let r = reach(Some(150.0));
        let (x, y) = (
            score_row(&near, &band, Some(&r), None).unwrap(),
            score_row(&far, &band, Some(&r), None).unwrap(),
        );
        assert_eq!((x.score, y.score), (0.5, 0.5));
        for rows in [
            vec![far.clone(), near.clone()],
            vec![near.clone(), far.clone()],
        ] {
            let a = answer(
                &band,
                rows,
                Some(r.clone()),
                &Vantage::None("x"),
                vec![],
                None,
            );
            assert_eq!(a.origins[0].allocation.distance_km, Some(5.0));
            assert_eq!(a.origins[1].allocation.distance_km, None);
        }
    }

    #[test]
    fn rows_without_licence_or_site_are_not_folded_and_centres_resolve() {
        let band = q(&[], CENTER, 12_500);
        let mut a = tx(None, Some(12_500), CENTER, None);
        a.licence_no = None;
        a.site = None;
        let mut b = a.clone();
        b.licensee = Some("Example Broadcasting".into());
        let ans = answer(&band, vec![a, b], None, &Vantage::None("x"), vec![], None);
        assert_eq!(ans.origins.len(), 2);
        // No centre_hz: the resolved centres differ, so the same licence and site stay two.
        let mut c = tx(None, Some(12_500), CENTER, None);
        c.center_hz = None;
        let mut d = c.clone();
        (d.min_hz, d.max_hz) = (Hz(CENTER + 2_000), Hz(CENTER + 2_000));
        let ans = answer(
            &band,
            vec![c.clone(), d],
            None,
            &Vantage::None("x"),
            vec![],
            None,
        );
        assert_eq!(ans.origins.len(), 2);
        // The same resolved centre folds.
        let ans = answer(
            &band,
            vec![c.clone(), c],
            None,
            &Vantage::None("x"),
            vec![],
            None,
        );
        assert_eq!(ans.origins.len(), 1);
    }
}
