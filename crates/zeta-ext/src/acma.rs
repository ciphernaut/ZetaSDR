//! The `acma` knowledge provider: one register answer per signal from the operator's ACMA
//! register service (the acma-rrl-server fork) over MCP. It runs the register queries of
//! `registers/acma/` (data rows, see [`zeta_core::register`]) through the service's
//! `execute_sql` tool and reads the data date from its `GET /health`:
//! `candidates`, `area_devices` and `plan_span` together, then `licence_related`,
//! `site_channels` and `link_far_end` for the top candidates, all inside one deadline.
//! The candidates are ranked by the propagation estimate from the observer
//! ([`zeta_core::propagation`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use zeta_core::emission::parse_emission;
use zeta_core::knowledge::{Allocation, KnowledgeProvider, Signature};
use zeta_core::propagation::{
    Emitter, InRange, Observer, Propagation, estimate, haversine_km, search_km,
};
use zeta_core::reach::{Reach, reach_for};
use zeta_core::register::{
    AreaCandidate, Candidate, Channel, Device, FailedQuery, FarEnd, Holder, Licence, ParamValue,
    PlanFootnote, PlanSegment, PlanService, REGISTER_MAX_CANDIDATES, RELATED_CHANNELS, RELATED_TOP,
    RegisterAnswer, RegisterQueries, RegisterState, RegisterStatus, Related, Signal, Site,
};
use zeta_core::station::Station;
use zeta_core::types::Hz;

use crate::mcp_client::McpClient;

/// The provider's name in `knowledge_lookup` results, and its queries' `provider`.
pub const ACMA_PROVIDER: &str = "acma";
/// The only region the register answers for.
pub const ACMA_REGION: &str = "AU";
/// A whole lookup (every call it makes) gives up after this long.
pub const ACMA_TIMEOUT: Duration = Duration::from_secs(5);
/// A failing service is logged at most once per this period.
pub const ACMA_LOG_EVERY: Duration = Duration::from_secs(60);
/// The register's data date (its `GET /health`) is reused for this long.
pub const ACMA_PLAN_TTL: Duration = Duration::from_secs(60);

/// Kilometres per degree of latitude, for the query's bounding box.
const KM_PER_DEG_LAT: f64 = 111.0;
/// Floor of `cos(lat)` in the bounding box, so a station near a pole stays finite.
const MIN_COS_LAT: f64 = 0.01;
/// The whole globe, `[lat_min, lat_max, lon_min, lon_max]`: the box with no bound.
const GLOBE: [f64; 4] = [-90.0, 90.0, -180.0, 180.0];
/// A plan service whose name holds one of these makes the band a space band.
const SPACE_SERVICES: [&str; 3] = ["SATELLITE", "SPACE", "EARTH EXPLORATION"];

/// Where the station is now; `None` when none is set.
pub type StationFn = Arc<dyn Fn() -> Option<Station> + Send + Sync>;

/// One result row of a register query, keyed by column alias.
pub type Row = Map<String, Value>;

/// The register service as a knowledge provider.
pub struct AcmaKnowledge {
    client: McpClient,
    override_km: Option<f64>,
    station: StationFn,
    queries: Arc<RegisterQueries>,
    propagation: Propagation,
    timeout: Duration,
    errors: AtomicU64,
    logged: AtomicU64,
    last_log: Mutex<Option<Instant>>,
    /// The last data date the health read gave, and when.
    as_of: Mutex<Option<(Instant, String)>>,
}

impl AcmaKnowledge {
    /// `override_km` replaces the propagation search bound; `station` is asked once per
    /// `allocations` or `reach` (the `_from` methods and `describe` take their vantage
    /// instead); `queries` holds the `acma` register queries.
    pub fn new(
        client: McpClient,
        override_km: Option<f64>,
        station: StationFn,
        queries: Arc<RegisterQueries>,
    ) -> Self {
        Self {
            client,
            override_km,
            station,
            queries,
            propagation: Propagation::default(),
            timeout: ACMA_TIMEOUT,
            errors: AtomicU64::new(0),
            logged: AtomicU64::new(0),
            last_log: Mutex::new(None),
            as_of: Mutex::new(None),
        }
    }

    /// Replace [`ACMA_TIMEOUT`] (tests).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Replace the propagation assumptions (the defaults otherwise).
    pub fn with_propagation(mut self, p: Propagation) -> Self {
        self.propagation = p;
        self
    }

    /// Lookups that failed, wholly or in part, since the provider was made.
    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::SeqCst)
    }

    /// Failures that were logged (at most one per [`ACMA_LOG_EVERY`]).
    pub fn logged(&self) -> u64 {
        self.logged.load(Ordering::SeqCst)
    }

    fn fail(&self, error: &str) {
        self.errors.fetch_add(1, Ordering::SeqCst);
        let mut last = self.last_log.lock().unwrap_or_else(|p| p.into_inner());
        if last.is_none_or(|t| t.elapsed() >= ACMA_LOG_EVERY) {
            *last = Some(Instant::now());
            self.logged.fetch_add(1, Ordering::SeqCst);
            eprintln!("zeta: knowledge acma: {error}");
        }
    }

    /// Run the register query `id` with `values`: its rows keyed by column alias. A
    /// declared column missing from the answer, a tool error the client hands back as
    /// text, or an answer of another shape is an error.
    pub async fn run_query(
        &self,
        id: &str,
        values: &BTreeMap<&str, ParamValue>,
    ) -> Result<Vec<Row>, String> {
        let q = self
            .queries
            .get(ACMA_PROVIDER, id)
            .ok_or_else(|| format!("no register query `{ACMA_PROVIDER}/{id}`"))?;
        let sql = q.render(values)?;
        let answer = self
            .client
            .call(&q.call, json!({"sql": sql, "limit": q.limit}))
            .await
            .map_err(|e| format!("{}: {e}", q.call))?;
        if let Value::String(text) = &answer {
            return Err(format!("{}: {text}", q.call));
        }
        rows_of(&answer, &q.columns).map_err(|e| format!("{}: {e}", q.call))
    }

    /// [`run_query`](Self::run_query) cut off at `deadline`.
    async fn query_by(
        &self,
        deadline: tokio::time::Instant,
        id: &str,
        values: &BTreeMap<&str, ParamValue>,
    ) -> Result<Vec<Row>, String> {
        match tokio::time::timeout_at(deadline, self.run_query(id, values)).await {
            Ok(r) => r,
            Err(_) => Err(format!("timed out after {} ms", self.timeout.as_millis())),
        }
    }

    /// The register's data date, from `GET /health` (`dataAsOf`), reused for
    /// [`ACMA_PLAN_TTL`]; `None` when the read fails or names none.
    async fn as_of(&self) -> Option<String> {
        let memo = self.as_of.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some((at, v)) = memo
            && at.elapsed() < ACMA_PLAN_TTL
        {
            return Some(v);
        }
        let health = self.client.health().await.ok()?;
        let v = health.get("dataAsOf")?.as_str()?.to_string();
        *self.as_of.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), v.clone()));
        Some(v)
    }

    /// How far the search reaches from `observer`: the operator's override, else the
    /// furthest any propagation mode allows; `None` without an observer or unbounded.
    fn bound(&self, observer: Option<&Observer>, center_hz: Hz) -> Option<f64> {
        let o = observer?;
        self.override_km
            .or_else(|| search_km(o, center_hz.0, &self.propagation))
    }

    /// The whole register answer for `signal` as heard from `from`.
    async fn lookup(&self, signal: &Signal, from: Option<&Station>) -> RegisterAnswer {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let center = signal.center_hz;
        let half = signal.bandwidth_hz.0 / 2;
        let (lo, hi) = (center.0.saturating_sub(half), center.0.saturating_add(half));
        let observer = from.map(|s| observer_of(s, center));
        let bound = self.bound(observer.as_ref(), center);
        let b = search_box(observer.as_ref(), bound);

        let int = |hz: u64| ParamValue::Int(i64::try_from(hz).unwrap_or(i64::MAX));
        let band: BTreeMap<&str, ParamValue> = [("lo_hz", int(lo)), ("hi_hz", int(hi))].into();
        let mut boxed = band.clone();
        boxed.extend([
            ("lat_min", ParamValue::Number(b[0])),
            ("lat_max", ParamValue::Number(b[1])),
            ("lon_min", ParamValue::Number(b[2])),
            ("lon_max", ParamValue::Number(b[3])),
            ("center_hz", int(center.0)),
        ]);

        let (candidates, area, plan, as_of) = tokio::join!(
            self.query_by(deadline, "candidates", &boxed),
            self.query_by(deadline, "area_devices", &band),
            self.query_by(deadline, "plan_span", &band),
            tokio::time::timeout_at(deadline, self.as_of()),
        );
        let plan = plan.and_then(|rows| rows.iter().map(plan_of).collect::<Result<Vec<_>, _>>());
        let mut failed: Vec<FailedQuery> = Vec::new();
        for (query, e) in [
            ("candidates", candidates.as_ref().err()),
            ("area_devices", area.as_ref().err()),
            ("plan_span", plan.as_ref().err()),
        ] {
            if let Some(e) = e {
                failed.push(FailedQuery {
                    query: query.into(),
                    reason: e.clone(),
                });
            }
        }
        if failed.len() == 3 {
            let first = &failed[0];
            let reason = format!("{}: {}", first.query, first.reason);
            self.fail(&reason);
            let mut a = RegisterAnswer::unreachable(ACMA_PROVIDER, ACMA_REGION, reason);
            a.assumptions = self.propagation.clone();
            return a;
        }

        let limit = self
            .queries
            .get(ACMA_PROVIDER, "candidates")
            .map_or(u64::MAX, |q| q.limit);
        let rows = candidates.unwrap_or_default();
        let matched = rows.len();
        let mut ranked = merged(rows.iter().filter_map(candidate_of));
        if let Some(o) = &observer {
            for c in &mut ranked {
                let (g, e) = estimate(o, &emitter_of(c), &self.propagation);
                c.geometry = Some(g);
                c.expected = Some(e);
            }
            ranked.retain(|c| {
                let out_of_range = c
                    .expected
                    .as_ref()
                    .is_some_and(|e| e.in_range == InRange::No);
                let outside = matches!(
                    (bound, c.geometry.as_ref()),
                    (Some(km), Some(g)) if g.distance_km > km
                );
                !(out_of_range && outside)
            });
        }
        ranked.sort_by(|a, b| rank(a, b, center));
        let truncated = matched as u64 >= limit || ranked.len() > REGISTER_MAX_CANDIDATES;
        ranked.truncate(REGISTER_MAX_CANDIDATES);

        let related = futures_util::future::join_all(
            ranked
                .iter()
                .take(RELATED_TOP)
                .map(|c| self.related(c, deadline)),
        )
        .await;
        for (c, r) in ranked.iter_mut().zip(related) {
            match r {
                Ok(r) => c.related = Some(r),
                Err(fails) => {
                    for f in fails {
                        if !failed.iter().any(|x| x.query == f.query) {
                            failed.push(f);
                        }
                    }
                }
            }
        }

        let state = if failed.is_empty() {
            RegisterState::Ok
        } else {
            let names: Vec<String> = failed
                .iter()
                .map(|f| format!("{}: {}", f.query, f.reason))
                .collect();
            self.fail(&format!("partial answer: {}", names.join("; ")));
            RegisterState::Partial
        };
        RegisterAnswer {
            provider: ACMA_PROVIDER.into(),
            region: ACMA_REGION.into(),
            status: RegisterStatus {
                state,
                reason: None,
                failed,
            },
            as_of: as_of.ok().flatten(),
            plan: plan.unwrap_or_default(),
            candidates: ranked,
            area_candidates: area
                .unwrap_or_default()
                .iter()
                .filter_map(area_candidate_of)
                .collect(),
            matched: u32::try_from(matched).unwrap_or(u32::MAX),
            truncated,
            assumptions: self.propagation.clone(),
            verdict: None,
        }
    }

    /// What else the register holds about `c`: the other frequencies on its licence,
    /// its holder's other channels at its site, and the far end of a link from the site
    /// on its frequency. A query that fails is named, and nothing is filled.
    async fn related(
        &self,
        c: &Candidate,
        deadline: tokio::time::Instant,
    ) -> Result<Related, Vec<FailedQuery>> {
        let licence: BTreeMap<&str, ParamValue> =
            [("licence_no", ParamValue::Id(c.licence.number.clone()))].into();
        let site: BTreeMap<&str, ParamValue> =
            [("site_id", ParamValue::Id(c.site.id.clone()))].into();
        let mut link = site.clone();
        link.insert(
            "center_hz",
            ParamValue::Int(i64::try_from(c.device.center_hz.0).unwrap_or(i64::MAX)),
        );
        let (paired, channels, far) = tokio::join!(
            self.query_by(deadline, "licence_related", &licence),
            self.query_by(deadline, "site_channels", &site),
            self.query_by(deadline, "link_far_end", &link),
        );
        let failed: Vec<FailedQuery> = [
            ("licence_related", paired.as_ref().err()),
            ("site_channels", channels.as_ref().err()),
            ("link_far_end", far.as_ref().err()),
        ]
        .into_iter()
        .filter_map(|(q, e)| {
            e.map(|e| FailedQuery {
                query: q.into(),
                reason: e.clone(),
            })
        })
        .collect();
        let (Ok(paired), Ok(channels), Ok(far)) = (paired, channels, far) else {
            return Err(failed);
        };
        let own_role = c.device.role.as_deref();
        let own_centre = c.device.center_hz;

        // The licence's other frequencies, one per centre and role.
        let mut seen: Vec<(Hz, Option<String>)> = Vec::new();
        let paired: Vec<Channel> = paired
            .iter()
            .filter_map(channel_of)
            .filter(|ch| !(ch.center_hz == own_centre && ch.role.as_deref() == own_role))
            .filter(|ch| {
                let key = (ch.center_hz, ch.role.clone());
                let new = !seen.contains(&key);
                if new {
                    seen.push(key);
                }
                new
            })
            .collect();

        // The holder's channels at the site, the candidate's own left out.
        let mut seen: Vec<(Option<String>, Hz, Option<String>)> = Vec::new();
        let holder: Vec<Channel> = channels
            .iter()
            .filter(|r| {
                let licence_no = text(r, "licence_no");
                let same_licence = licence_no.as_deref() == Some(c.licence.number.as_str());
                let same_holder = c
                    .holder
                    .name
                    .as_deref()
                    .is_some_and(|n| text(r, "licensee").as_deref() == Some(n));
                same_licence || same_holder
            })
            .filter_map(|r| Some((text(r, "licence_no"), channel_of(r)?)))
            .filter(|(l, ch)| {
                !(l.as_deref() == Some(c.licence.number.as_str())
                    && ch.center_hz == own_centre
                    && ch.role.as_deref() == own_role)
            })
            .filter(|(l, ch)| {
                let key = (l.clone(), ch.center_hz, ch.role.clone());
                let new = !seen.contains(&key);
                if new {
                    seen.push(key);
                }
                new
            })
            .map(|(_, ch)| ch)
            .collect();

        let far_end = far.iter().find_map(|r| {
            let (lat, lon) = (num(r, "far_lat")?, num(r, "far_lon")?);
            Some(FarEnd {
                site: Site {
                    id: text(r, "far_site_id")?,
                    name: text(r, "far_name"),
                    lat,
                    lon,
                    precision: None,
                    elevation_m: None,
                },
                distance_km: Some(haversine_km(c.site.lat, c.site.lon, lat, lon)),
            })
        });
        Ok(Related {
            paired,
            holder_channels: u32::try_from(holder.len()).unwrap_or(u32::MAX),
            holder_channel_list: holder.into_iter().take(RELATED_CHANNELS).collect(),
            far_end,
        })
    }
}

#[async_trait]
impl KnowledgeProvider for AcmaKnowledge {
    fn name(&self) -> &str {
        ACMA_PROVIDER
    }

    fn gives_assignments(&self) -> bool {
        true
    }

    fn regions(&self) -> Vec<String> {
        vec![ACMA_REGION.into()]
    }

    /// The register answer for `signal` from `from`; `None` for another region. An
    /// unreachable register is an answer saying so, never `None`.
    async fn describe(
        &self,
        region: &str,
        signal: &Signal,
        from: Option<&Station>,
    ) -> Option<RegisterAnswer> {
        if !answers(region) {
            return None;
        }
        Some(self.lookup(signal, from).await)
    }

    /// From the node's station, asked now.
    async fn allocations(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Allocation> {
        let station = (self.station)();
        self.allocations_from(region, center_hz, bandwidth_hz, station.as_ref())
            .await
    }

    /// The rows of [`describe`](KnowledgeProvider::describe)'s answer: the candidates in
    /// rank order, then the plan.
    async fn allocations_from(
        &self,
        region: &str,
        center_hz: Hz,
        bandwidth_hz: Hz,
        from: Option<&Station>,
    ) -> Vec<Allocation> {
        let signal = Signal {
            center_hz,
            bandwidth_hz,
        };
        self.describe(region, &signal, from)
            .await
            .map(|a| a.allocations())
            .unwrap_or_default()
    }

    async fn signatures(&self, _center_hz: Hz, _bandwidth_hz: Hz) -> Vec<Signature> {
        Vec::new()
    }

    /// The band's reach from the station; `None` for another region or without a station.
    async fn reach(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz) -> Option<Reach> {
        let station = (self.station)();
        self.reach_from(region, center_hz, bandwidth_hz, station.as_ref())
            .await
    }

    /// The search bound from `from` as a [`Reach`]: `km` is the bound, the class the
    /// band's, a space band when the plan's services are space services (from
    /// [`describe`](KnowledgeProvider::describe); a plan that failed reads as no space
    /// band) or the antenna points up. `None` without `from`.
    async fn reach_from(
        &self,
        region: &str,
        center_hz: Hz,
        bandwidth_hz: Hz,
        from: Option<&Station>,
    ) -> Option<Reach> {
        let from = from?;
        let signal = Signal {
            center_hz,
            bandwidth_hz,
        };
        let answer = self.describe(region, &signal, Some(from)).await?;
        let space = answer.plan.iter().any(is_space);
        let mut reach = reach_for(
            center_hz,
            &from.antenna_for(center_hz),
            space,
            self.override_km,
        );
        reach.km = self.bound(Some(&observer_of(from, center_hz)), center_hz);
        Some(reach)
    }
}

/// Whether the register answers for `region` (Australia only).
fn answers(region: &str) -> bool {
    region.eq_ignore_ascii_case(ACMA_REGION)
}

/// The observer at `s`, with the height of the antenna serving `center_hz`.
fn observer_of(s: &Station, center_hz: Hz) -> Observer {
    Observer {
        lat: s.lat,
        lon: s.lon,
        alt_m: s.alt_m,
        antenna_height_m: s.antenna_for(center_hz).height_m,
    }
}

/// The query's box, `[lat_min, lat_max, lon_min, lon_max]`: `km` around the observer
/// (longitude widened by `1 / cos(lat)`, floored at [`MIN_COS_LAT`]), clamped to the
/// globe; the whole globe with no observer or no bound.
fn search_box(observer: Option<&Observer>, km: Option<f64>) -> [f64; 4] {
    let (Some(o), Some(km)) = (observer, km.filter(|k| k.is_finite() && *k >= 0.0)) else {
        return GLOBE;
    };
    let dlat = km / KM_PER_DEG_LAT;
    let dlon = km / (KM_PER_DEG_LAT * o.lat.to_radians().cos().abs().max(MIN_COS_LAT));
    let b = [o.lat - dlat, o.lat + dlat, o.lon - dlon, o.lon + dlon];
    if b.iter().any(|x| !x.is_finite()) {
        return GLOBE;
    }
    [
        b[0].clamp(GLOBE[0], GLOBE[1]),
        b[1].clamp(GLOBE[0], GLOBE[1]),
        b[2].clamp(GLOBE[2], GLOBE[3]),
        b[3].clamp(GLOBE[2], GLOBE[3]),
    ]
}

/// Whether a plan segment allocates a space service.
fn is_space(p: &PlanSegment) -> bool {
    p.services.iter().any(|s| {
        let n = s.name.to_uppercase();
        SPACE_SERVICES.iter().any(|k| n.contains(k))
    })
}

/// Candidates merged on (licence, site, centre), in first-seen order; a transmitter
/// replaces a receiver of the same key.
fn merged(rows: impl Iterator<Item = Candidate>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut at: HashMap<(String, String, u64), usize> = HashMap::new();
    for c in rows {
        let key = (
            c.licence.number.clone(),
            c.site.id.clone(),
            c.device.center_hz.0,
        );
        match at.get(&key) {
            Some(&i) => {
                if out[i].device.role.as_deref() != Some("tx")
                    && c.device.role.as_deref() == Some("tx")
                {
                    out[i] = c;
                }
            }
            None => {
                at.insert(key, out.len());
                out.push(c);
            }
        }
    }
    out
}

/// Rank order: the strongest expected level first; then those with no level by
/// distance; then by distance in frequency from `center`; transmitters before receivers.
fn rank(a: &Candidate, b: &Candidate, center: Hz) -> std::cmp::Ordering {
    use std::cmp::Ordering::{Greater, Less};
    let level = |c: &Candidate| c.expected.as_ref().and_then(|e| e.level_dbm);
    let distance = |c: &Candidate| c.geometry.as_ref().map_or(f64::INFINITY, |g| g.distance_km);
    let off = |c: &Candidate| c.device.center_hz.0.abs_diff(center.0);
    let role = |c: &Candidate| u8::from(c.device.role.as_deref() != Some("tx"));
    match (level(a), level(b)) {
        (Some(x), Some(y)) => y.total_cmp(&x),
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
        (None, None) => distance(a).total_cmp(&distance(b)),
    }
    .then_with(|| off(a).cmp(&off(b)))
    .then_with(|| role(a).cmp(&role(b)))
}

/// What the estimate needs of a candidate.
fn emitter_of(c: &Candidate) -> Emitter {
    let d = &c.device;
    Emitter {
        lat: c.site.lat,
        lon: c.site.lon,
        freq_hz: d.center_hz.0,
        elevation_m: c.site.elevation_m,
        height_m: d.height_m,
        eirp_w: d.eirp_w,
        power_w: d.power_w,
        gain_dbi: d.gain_dbi,
        azimuth_deg: d.azimuth_deg,
        beamwidth_deg: d.beamwidth_deg,
        front_to_back_db: d.front_to_back_db,
    }
}

/// The rows of an `execute_sql` answer keyed by column name; every one of `columns`
/// must be among them.
fn rows_of(v: &Value, columns: &[String]) -> Result<Vec<Row>, String> {
    let bad = || format!("unexpected answer: {}", head(v));
    let names: Vec<String> = v
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(bad)?
        .iter()
        .map(|c| c.as_str().unwrap_or_default().to_string())
        .collect();
    if let Some(missing) = columns.iter().find(|c| !names.contains(c)) {
        return Err(format!("the answer has no column `{missing}`"));
    }
    Ok(v.get("rows")
        .and_then(Value::as_array)
        .ok_or_else(bad)?
        .iter()
        .filter_map(Value::as_array)
        .map(|r| names.iter().cloned().zip(r.iter().cloned()).collect())
        .collect())
}

/// The start of `v` for an error message.
fn head(v: &Value) -> String {
    v.to_string().chars().take(80).collect()
}

/// A cell that is not NULL.
fn cell<'a>(r: &'a Row, k: &str) -> Option<&'a Value> {
    r.get(k).filter(|v| !v.is_null())
}

/// A cell as trimmed, non-empty text; a number as its digits (a register id may be
/// stored as one).
fn text(r: &Row, k: &str) -> Option<String> {
    match cell(r, k)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A cell as a finite number (numeric text too).
fn num(r: &Row, k: &str) -> Option<f64> {
    match cell(r, k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

/// A cell as a frequency in Hz.
fn hz(r: &Row, k: &str) -> Option<Hz> {
    num(r, k)
        .filter(|x| *x >= 0.0)
        .map(|x| Hz(x.round() as u64))
}

/// `T` / `R` as `tx` / `rx`.
fn role_of(r: &Row) -> Option<String> {
    match text(r, "device_type")?.as_str() {
        t if t.eq_ignore_ascii_case("T") => Some("tx".into()),
        t if t.eq_ignore_ascii_case("R") => Some("rx".into()),
        _ => None,
    }
}

/// A value and its unit in watts.
fn watts(r: &Row, value: &str, unit: &str) -> Option<f64> {
    to_watts(num(r, value)?, &text(r, unit)?)
}

fn device_of(r: &Row) -> Option<Device> {
    let emission = text(r, "emission");
    Some(Device {
        center_hz: hz(r, "center_hz")?,
        bandwidth_hz: hz(r, "bandwidth_hz"),
        emission_decoded: emission.as_deref().and_then(parse_emission),
        emission,
        role: role_of(r),
        power_w: watts(r, "power", "power_unit"),
        eirp_w: watts(r, "eirp", "eirp_unit"),
        gain_dbi: num(r, "gain_dbi"),
        azimuth_deg: num(r, "azimuth_deg"),
        beamwidth_deg: num(r, "beamwidth_deg"),
        front_to_back_db: num(r, "front_to_back_db"),
        height_m: num(r, "height_m"),
        tilt_deg: num(r, "tilt_deg"),
        polarisation: text(r, "polarisation"),
        callsign: text(r, "callsign"),
        station_class: text(r, "station_class"),
        nature_of_service: text(r, "nature_of_service"),
        hours: text(r, "hours"),
    })
}

fn licence_of(r: &Row) -> Licence {
    Licence {
        number: text(r, "licence_no").unwrap_or_default(),
        service: text(r, "service"),
        subservice: text(r, "subservice"),
        status: text(r, "licence_status"),
        issued: text(r, "issued"),
        expires: text(r, "expires"),
    }
}

fn holder_of(r: &Row) -> Holder {
    Holder {
        name: text(r, "licensee"),
        trading_name: text(r, "trading_name"),
        industry: text(r, "industry"),
        client_type: text(r, "client_type"),
    }
}

/// A `candidates` row; `None` without a centre, a site id or a position.
fn candidate_of(r: &Row) -> Option<Candidate> {
    Some(Candidate {
        device: device_of(r)?,
        site: Site {
            id: text(r, "site_id")?,
            name: text(r, "site_name"),
            lat: num(r, "lat")?,
            lon: num(r, "lon")?,
            precision: text(r, "site_precision"),
            elevation_m: num(r, "elevation_m"),
        },
        licence: licence_of(r),
        holder: holder_of(r),
        geometry: None,
        expected: None,
        related: None,
    })
}

/// An `area_devices` row; `None` without a centre.
fn area_candidate_of(r: &Row) -> Option<AreaCandidate> {
    Some(AreaCandidate {
        device: device_of(r)?,
        licence: licence_of(r),
        holder: holder_of(r),
        area: text(r, "area"),
    })
}

/// A `licence_related` or `site_channels` row; `None` without a centre.
fn channel_of(r: &Row) -> Option<Channel> {
    Some(Channel {
        center_hz: hz(r, "center_hz")?,
        bandwidth_hz: hz(r, "bandwidth_hz"),
        role: role_of(r),
        emission: text(r, "emission"),
    })
}

/// A JSON cell that may arrive as JSON text: the array it holds; NULL is empty.
fn json_array(r: &Row, k: &str) -> Result<Vec<Value>, String> {
    let v = match cell(r, k) {
        None => return Ok(Vec::new()),
        Some(Value::String(s)) => {
            serde_json::from_str(s).map_err(|e| format!("{k} is not JSON: {e}"))?
        }
        Some(v) => v.clone(),
    };
    match v {
        Value::Array(a) => Ok(a),
        Value::Null => Ok(Vec::new()),
        other => Err(format!("{k} is not an array: {}", head(&other))),
    }
}

/// A `plan_span` row: the segment, its services and its footnotes.
fn plan_of(r: &Row) -> Result<PlanSegment, String> {
    let bound = |k: &str| hz(r, k).ok_or_else(|| format!("a plan segment without {k}"));
    let services = json_array(r, "services_json")?
        .iter()
        .filter_map(|s| {
            Some(PlanService {
                name: s.get("name")?.as_str()?.to_string(),
                primary: s.get("primary").and_then(Value::as_bool).unwrap_or(false),
                qualifier: s
                    .get("qualifier")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect();
    let footnotes = json_array(r, "footnotes_json")?
        .iter()
        .filter_map(|f| {
            let reference = match f.get("ref")? {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            Some(PlanFootnote {
                reference,
                text: f.get("text").and_then(Value::as_str).map(str::to_string),
            })
        })
        .collect();
    Ok(PlanSegment {
        min_hz: bound("min_hz")?,
        max_hz: bound("max_hz")?,
        services,
        footnotes,
    })
}

/// The register's power in watts; `None` for a unit it does not know.
fn to_watts(p: f64, unit: &str) -> Option<f64> {
    match unit.trim() {
        "W" | "w" => Some(p),
        "mW" => Some(p / 1e3),
        "kW" => Some(p * 1e3),
        "dBW" => Some(10f64.powf(p / 10.0)),
        "dBm" => Some(10f64.powf((p - 30.0) / 10.0)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeta_core::register::emission_words;

    #[test]
    fn power_units_convert_to_watts() {
        assert_eq!(to_watts(1.0, "W"), Some(1.0));
        assert_eq!(to_watts(500.0, "mW"), Some(0.5));
        assert_eq!(to_watts(2.0, "kW"), Some(2000.0));
        assert!((to_watts(10.0, "dBW").unwrap() - 10.0).abs() < 1e-9);
        assert!((to_watts(30.0, "dBm").unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(to_watts(1.0, "furlongs"), None);
    }

    #[test]
    fn emissions_read_as_words() {
        let w = |c| emission_words(&parse_emission(c).unwrap());
        assert_eq!(w("16K0F1D"), "16 kHz FM data");
        assert_eq!(w("12K5F3E"), "12.5 kHz FM voice");
        assert_eq!(w("500HA1A"), "500 Hz AM telegraphy");
        assert_eq!(w("1M25G7W"), "1.25 MHz PM mixed");
    }

    #[test]
    fn a_box_is_clamped_to_the_globe_and_whole_without_a_bound() {
        let o = |lat, lon| Observer {
            lat,
            lon,
            alt_m: 0.0,
            antenna_height_m: 5.0,
        };
        assert_eq!(search_box(None, Some(100.0)), GLOBE);
        assert_eq!(search_box(Some(&o(0.0, 0.0)), None), GLOBE);
        let b = search_box(Some(&o(89.99, 179.99)), Some(500.0));
        assert_eq!((b[1], b[3]), (90.0, 180.0));
        assert!(b.iter().all(|x| x.is_finite()));
    }
}
