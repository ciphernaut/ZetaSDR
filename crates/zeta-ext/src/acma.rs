//! The `acma` knowledge provider: the Australian Spectrum Plan's services at a
//! frequency and the register's licensed assignments overlapping a band, from the
//! operator's ACMA register service (the acma-rrl-server fork) over MCP. Two calls
//! per lookup: `get_frequency_allocation` for the plan and one fixed `execute_sql`
//! query for the assignments, cut to the band's reach around the station.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use zeta_core::emission::{Emission, parse_emission};
use zeta_core::knowledge::{Allocation, KnowledgeProvider, Signature};
use zeta_core::reach::{Reach, reach_for};
use zeta_core::station::Station;
use zeta_core::types::Hz;

use crate::mcp_client::McpClient;

/// The provider's name in `knowledge_lookup` results.
pub const ACMA_PROVIDER: &str = "acma";
/// The only region the register answers for.
pub const ACMA_REGION: &str = "AU";
/// At most this many assignments in one lookup, nearest first.
pub const ACMA_MAX_ROWS: usize = 50;
/// The `limit` passed to `execute_sql`: rows fetched before the reach cut.
pub const ACMA_SQL_LIMIT: u64 = 200;
/// A whole lookup (every call it makes) gives up after this long.
pub const ACMA_TIMEOUT: Duration = Duration::from_secs(5);
/// A failing service is logged at most once per this period.
pub const ACMA_LOG_EVERY: Duration = Duration::from_secs(60);
/// `reach()` reuses the plan answer of a lookup at the same centre this recent
/// instead of asking again, so one `knowledge_lookup` is one deadline.
pub const ACMA_PLAN_TTL: Duration = Duration::from_secs(60);

/// Whether the register's `device_details` has a `CALL_SIGN` column: not known yet,
/// it has, or it has not (an older build of the register service).
const CALL_SIGN_UNKNOWN: u8 = 0;
const CALL_SIGN_PRESENT: u8 = 1;
const CALL_SIGN_ABSENT: u8 = 2;

/// Mean earth radius for the haversine distance.
const EARTH_RADIUS_KM: f64 = 6371.0088;
/// Kilometres per degree of latitude, for the query's bounding box.
const KM_PER_DEG_LAT: f64 = 111.0;
/// Floor of `cos(lat)` in the bounding box, so a station near a pole stays finite.
const MIN_COS_LAT: f64 = 0.01;
/// A plan service whose name holds one of these makes the band a space band.
const SPACE_SERVICES: [&str; 3] = ["SATELLITE", "SPACE", "EARTH EXPLORATION"];

/// Where the station is now; `None` when none is set.
pub type StationFn = Arc<dyn Fn() -> Option<Station> + Send + Sync>;

/// The register service as a knowledge provider.
pub struct AcmaKnowledge {
    client: McpClient,
    override_km: Option<f64>,
    station: StationFn,
    timeout: Duration,
    errors: AtomicU64,
    logged: AtomicU64,
    last_log: Mutex<Option<Instant>>,
    plan_memo: Mutex<Option<PlanMemo>>,
    /// What the first assignments query learned about `CALL_SIGN`
    /// (`CALL_SIGN_UNKNOWN` until then).
    call_sign: AtomicU8,
}

/// What the last plan lookup at a centre said about space services: `space` is
/// `None` when it failed or did not answer in time.
#[derive(Clone, Copy)]
struct PlanMemo {
    center_hz: Hz,
    at: Instant,
    space: Option<bool>,
}

/// One service of the plan entry at a frequency.
struct PlanService {
    name: String,
    primary: bool,
    qualifier: Option<String>,
}

/// The plan entry at a frequency.
struct Plan {
    min_hz: Hz,
    max_hz: Hz,
    services: Vec<PlanService>,
}

impl Plan {
    fn is_space(&self) -> bool {
        self.services.iter().any(|s| {
            let n = s.name.to_uppercase();
            SPACE_SERVICES.iter().any(|k| n.contains(k))
        })
    }
}

/// One `execute_sql` row.
#[derive(Default)]
struct Row {
    frequency: u64,
    bandwidth: u64,
    emission: Option<String>,
    device_type: Option<String>,
    power: Option<f64>,
    power_unit: Option<String>,
    licence_no: Option<String>,
    site: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    licensee: Option<String>,
    callsign: Option<String>,
}

/// The station, the antenna serving the band and the band's reach from it.
struct Here {
    station: Station,
    antenna: zeta_core::station::Antenna,
    reach: Reach,
}

impl AcmaKnowledge {
    /// `override_km` replaces the reach rule's kilometres (see [`reach_for`]);
    /// `station` is asked once per `allocations` or `reach`; the `_from` methods take
    /// their vantage instead.
    pub fn new(client: McpClient, override_km: Option<f64>, station: StationFn) -> Self {
        Self {
            client,
            override_km,
            station,
            timeout: ACMA_TIMEOUT,
            errors: AtomicU64::new(0),
            logged: AtomicU64::new(0),
            last_log: Mutex::new(None),
            plan_memo: Mutex::new(None),
            call_sign: AtomicU8::new(CALL_SIGN_UNKNOWN),
        }
    }

    /// Replace [`ACMA_TIMEOUT`] (tests).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Lookups that failed since the provider was made.
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

    /// Run `f` under the lookup timeout; a failure is counted, logged and `None`.
    async fn guarded<T>(
        &self,
        f: impl std::future::Future<Output = Result<T, String>>,
    ) -> Option<T> {
        match tokio::time::timeout(self.timeout, f).await {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                self.fail(&e);
                None
            }
            Err(_) => {
                self.fail(&format!("timed out after {} ms", self.timeout.as_millis()));
                None
            }
        }
    }

    fn memo(&self, center_hz: Hz, space: Option<bool>) {
        *self.plan_memo.lock().unwrap_or_else(|p| p.into_inner()) = Some(PlanMemo {
            center_hz,
            at: Instant::now(),
            space,
        });
    }

    /// The memo for `center_hz` when fresh: `Some(space)`, `space` itself `None`
    /// when that lookup failed.
    fn fresh_memo(&self, center_hz: Hz) -> Option<Option<bool>> {
        self.plan_memo
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .filter(|m| m.center_hz == center_hz && m.at.elapsed() < ACMA_PLAN_TTL)
            .map(|m| m.space)
    }

    /// The plan at `center_hz`, memoised for `reach()`.
    async fn plan(&self, center_hz: Hz) -> Result<Option<Plan>, String> {
        // Marked failed first, so a lookup cut off by the deadline leaves that.
        self.memo(center_hz, None);
        let plan = self.fetch_plan(center_hz).await?;
        self.memo(center_hz, Some(plan.as_ref().is_some_and(Plan::is_space)));
        Ok(plan)
    }

    async fn fetch_plan(&self, center_hz: Hz) -> Result<Option<Plan>, String> {
        let v = self
            .client
            .call(
                "get_frequency_allocation",
                json!({"freq_hz": center_hz.0, "include_footnotes": false}),
            )
            .await
            .map_err(|e| format!("get_frequency_allocation: {e}"))?;
        parse_plan(&v)
    }

    fn here(&self, station: Option<Station>, center_hz: Hz, space: bool) -> Option<Here> {
        let station = station?;
        let antenna = station.antenna_for(center_hz);
        let reach = reach_for(center_hz, &antenna, space, self.override_km);
        Some(Here {
            station,
            antenna,
            reach,
        })
    }

    /// The assignments query's rows. The first query that answers settles whether
    /// the register has `CALL_SIGN`: while unsettled, a query naming it that fails
    /// (with any error) is asked again with `NULL AS CALL_SIGN`; when that answers the
    /// column is absent and every later query selects NULL, and when it fails too
    /// nothing is settled and the first error stands.
    async fn assignment_rows(
        &self,
        center_hz: Hz,
        bandwidth_hz: Hz,
        bbox: Option<(f64, f64, f64)>,
    ) -> Result<Vec<Row>, String> {
        let state = self.call_sign.load(Ordering::SeqCst);
        if state == CALL_SIGN_ABSENT {
            return self
                .query_rows(&assignments_sql(center_hz, bandwidth_hz, bbox, false))
                .await;
        }
        match self
            .query_rows(&assignments_sql(center_hz, bandwidth_hz, bbox, true))
            .await
        {
            Ok(rows) => {
                self.call_sign.store(CALL_SIGN_PRESENT, Ordering::SeqCst);
                Ok(rows)
            }
            // Any error while unsettled is asked again without the column: a register
            // may wrap SQLite's `no such column` in text that names nothing.
            Err(first) if state == CALL_SIGN_UNKNOWN => match self
                .query_rows(&assignments_sql(center_hz, bandwidth_hz, bbox, false))
                .await
            {
                Ok(rows) => {
                    self.call_sign.store(CALL_SIGN_ABSENT, Ordering::SeqCst);
                    Ok(rows)
                }
                Err(_) => Err(first),
            },
            Err(e) => Err(e),
        }
    }

    /// One `execute_sql` call, read as rows. A tool error the client hands back as
    /// text is an error carrying that whole text.
    async fn query_rows(&self, sql: &str) -> Result<Vec<Row>, String> {
        let answer = self
            .client
            .call("execute_sql", json!({"sql": sql, "limit": ACMA_SQL_LIMIT}))
            .await
            .map_err(|e| format!("execute_sql: {e}"))?;
        if let Value::String(text) = &answer {
            return Err(format!("execute_sql: {text}"));
        }
        parse_rows(&answer)
    }

    async fn lookup(
        &self,
        station: Option<Station>,
        center_hz: Hz,
        bandwidth_hz: Hz,
    ) -> Result<Vec<Allocation>, String> {
        let plan = self.plan(center_hz).await?;
        let here = self.here(
            station,
            center_hz,
            plan.as_ref().is_some_and(Plan::is_space),
        );
        // The widest reach in any direction bounds the query.
        let bbox = here.as_ref().and_then(|h| {
            let widest = h
                .reach
                .km_toward(h.antenna.azimuth_deg.unwrap_or(0.0), &h.antenna)?;
            Some((h.station.lat, h.station.lon, widest))
        });
        let rows = self.assignment_rows(center_hz, bandwidth_hz, bbox).await?;

        let mut out: Vec<Allocation> = plan
            .iter()
            .flat_map(|p| {
                p.services.iter().map(|s| Allocation {
                    provider: ACMA_PROVIDER.into(),
                    region: ACMA_REGION.into(),
                    min_hz: p.min_hz,
                    max_hz: p.max_hz,
                    service: format!(
                        "{} ({})",
                        s.name,
                        if s.primary { "primary" } else { "secondary" }
                    ),
                    note: s.qualifier.clone(),
                    kind: Some("plan".into()),
                    ..Allocation::default()
                })
            })
            .collect();
        let mut assignments: Vec<Allocation> = rows
            .into_iter()
            .map(|r| assignment(r, here.as_ref()))
            .filter(|a| within_reach(a, here.as_ref()))
            .collect();
        let role_rank = |a: &Allocation| u8::from(a.role.as_deref() != Some("tx"));
        if here.is_some() {
            assignments.sort_by(|a, b| {
                let (da, db) = (
                    a.distance_km.unwrap_or(f64::INFINITY),
                    b.distance_km.unwrap_or(f64::INFINITY),
                );
                da.total_cmp(&db)
                    .then_with(|| role_rank(a).cmp(&role_rank(b)))
            });
        } else {
            let off = |a: &Allocation| a.center_hz.map_or(u64::MAX, |c| c.0.abs_diff(center_hz.0));
            assignments.sort_by(|a, b| {
                off(a)
                    .cmp(&off(b))
                    .then_with(|| role_rank(a).cmp(&role_rank(b)))
            });
        }
        assignments.truncate(ACMA_MAX_ROWS);
        out.extend(assignments);
        Ok(out)
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

    /// From the node's station, asked now.
    async fn allocations(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz) -> Vec<Allocation> {
        let station = (self.station)();
        self.allocations_from(region, center_hz, bandwidth_hz, station.as_ref())
            .await
    }

    /// Distances, bearings, the reach cut and the query's box all from `from`;
    /// without it the list is by frequency distance, as with no station.
    async fn allocations_from(
        &self,
        region: &str,
        center_hz: Hz,
        bandwidth_hz: Hz,
        from: Option<&Station>,
    ) -> Vec<Allocation> {
        if !answers(region) {
            return Vec::new();
        }
        self.guarded(self.lookup(from.cloned(), center_hz, bandwidth_hz))
            .await
            .unwrap_or_default()
    }

    async fn signatures(&self, _center_hz: Hz, _bandwidth_hz: Hz) -> Vec<Signature> {
        Vec::new()
    }

    /// The band's reach from the station; `None` for another region or without a
    /// station (the reach is not applied then). It reuses the plan answer of the
    /// `allocations()` just made at this centre (see [`ACMA_PLAN_TTL`]) and asks the
    /// service only when there is none; a plan that failed reads as no space band.
    async fn reach(&self, region: &str, center_hz: Hz, bandwidth_hz: Hz) -> Option<Reach> {
        let station = (self.station)();
        self.reach_from(region, center_hz, bandwidth_hz, station.as_ref())
            .await
    }

    /// The band's reach from `from`, as [`reach`](KnowledgeProvider::reach) gives it
    /// from the station; `None` without `from`.
    async fn reach_from(
        &self,
        region: &str,
        center_hz: Hz,
        _bandwidth_hz: Hz,
        from: Option<&Station>,
    ) -> Option<Reach> {
        if !answers(region) {
            return None;
        }
        let station = from?.clone();
        let space = match self.fresh_memo(center_hz) {
            Some(space) => space,
            None => self
                .guarded(self.plan(center_hz))
                .await
                .map(|p| p.as_ref().is_some_and(Plan::is_space)),
        };
        self.here(Some(station), center_hz, space.unwrap_or(false))
            .map(|h| h.reach)
    }
}

/// Whether the register answers for `region` (Australia only).
fn answers(region: &str) -> bool {
    region.eq_ignore_ascii_case(ACMA_REGION)
}

/// The plan entry in a `get_frequency_allocation` answer; `None` when the plan has
/// no entry at the frequency.
fn parse_plan(v: &Value) -> Result<Option<Plan>, String> {
    let bad = || format!("get_frequency_allocation: unexpected answer: {}", head(v));
    let obj = v.as_object().ok_or_else(bad)?;
    let Some(a) = obj.get("allocation").filter(|a| !a.is_null()) else {
        return Ok(None);
    };
    let hz = |k: &str| a.get(k).and_then(as_u64).map(Hz).ok_or_else(bad);
    let services = a
        .get("services")
        .and_then(Value::as_array)
        .ok_or_else(bad)?
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
    Ok(Some(Plan {
        min_hz: hz("freq_start_hz")?,
        max_hz: hz("freq_end_hz")?,
        services,
    }))
}

/// The rows of an `execute_sql` answer, read by column name.
fn parse_rows(v: &Value) -> Result<Vec<Row>, String> {
    let bad = || format!("execute_sql: unexpected answer: {}", head(v));
    let columns: Vec<&str> = v
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(bad)?
        .iter()
        .map(|c| c.as_str().unwrap_or(""))
        .collect();
    let rows = v.get("rows").and_then(Value::as_array).ok_or_else(bad)?;
    let col = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let idx = [
        "FREQUENCY",
        "BANDWIDTH",
        "EMISSION",
        "DEVICE_TYPE",
        "TRANSMITTER_POWER",
        "TRANSMITTER_POWER_UNIT",
        "LICENCE_NO",
        "SITE_NAME",
        "LATITUDE",
        "LONGITUDE",
        "LICENCEE",
        "CALL_SIGN",
    ]
    .map(col);
    Ok(rows
        .iter()
        .filter_map(Value::as_array)
        .filter_map(|r| {
            let get = |i: usize| idx[i].and_then(|j| r.get(j)).filter(|x| !x.is_null());
            let text = |i: usize| {
                get(i)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            Some(Row {
                frequency: get(0).and_then(as_u64)?,
                bandwidth: get(1).and_then(as_u64).unwrap_or(0),
                emission: text(2),
                device_type: text(3),
                power: get(4).and_then(Value::as_f64),
                power_unit: text(5),
                licence_no: text(6),
                site: text(7),
                lat: get(8).and_then(Value::as_f64),
                lon: get(9).and_then(Value::as_f64),
                licensee: text(10),
                // `None` too from a service whose answer lacks the column.
                callsign: text(11),
            })
        })
        .collect())
}

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f.round() as u64)
    })
}

/// The start of `v` for an error message.
fn head(v: &Value) -> String {
    v.to_string().chars().take(80).collect()
}

/// The fixed assignments query: devices whose `FREQUENCY ± BANDWIDTH / 2` overlaps
/// the band (a device with no `BANDWIDTH` counts as 0 Hz wide, so it is matched on its
/// frequency rather than dropped by a NULL comparison), joined to their site, licence and licensee, inside a box of `km`
/// around `(lat, lon)` when one is given, nearest in frequency first. Without
/// `call_sign` (a register lacking the column) `CALL_SIGN` is selected as `NULL`.
fn assignments_sql(
    center_hz: Hz,
    bandwidth_hz: Hz,
    bbox: Option<(f64, f64, f64)>,
    call_sign: bool,
) -> String {
    let half = bandwidth_hz.0 / 2;
    let (lo, hi) = (
        center_hz.0.saturating_sub(half),
        center_hz.0.saturating_add(half),
    );
    let call_sign = if call_sign { "d.CALL_SIGN" } else { "NULL" };
    let mut sql = format!(
        "SELECT d.FREQUENCY AS FREQUENCY, d.BANDWIDTH AS BANDWIDTH, d.EMISSION AS EMISSION, \
         d.DEVICE_TYPE AS DEVICE_TYPE, d.TRANSMITTER_POWER AS TRANSMITTER_POWER, \
         d.TRANSMITTER_POWER_UNIT AS TRANSMITTER_POWER_UNIT, d.LICENCE_NO AS LICENCE_NO, \
         s.NAME AS SITE_NAME, s.LATITUDE AS LATITUDE, s.LONGITUDE AS LONGITUDE, \
         c.LICENCEE AS LICENCEE, {call_sign} AS CALL_SIGN \
         FROM device_details d \
         JOIN site s ON s.SITE_ID = d.SITE_ID \
         LEFT JOIN licence l ON l.LICENCE_NO = d.LICENCE_NO \
         LEFT JOIN client c ON c.CLIENT_NO = l.CLIENT_NO \
         WHERE d.FREQUENCY - COALESCE(d.BANDWIDTH, 0) / 2 <= {hi} \
         AND d.FREQUENCY + COALESCE(d.BANDWIDTH, 0) / 2 >= {lo}"
    );
    if let Some((lat, lon, km)) = bbox {
        let dlat = km / KM_PER_DEG_LAT;
        let dlon = km / (KM_PER_DEG_LAT * lat.to_radians().cos().max(MIN_COS_LAT));
        sql.push_str(&format!(
            " AND s.LATITUDE BETWEEN {:.6} AND {:.6} AND s.LONGITUDE BETWEEN {:.6} AND {:.6}",
            lat - dlat,
            lat + dlat,
            lon - dlon,
            lon + dlon
        ));
    }
    sql.push_str(&format!(" ORDER BY ABS(d.FREQUENCY - {})", center_hz.0));
    sql
}

/// One register row as an `assignment` allocation.
fn assignment(r: Row, here: Option<&Here>) -> Allocation {
    let half = r.bandwidth / 2;
    let (distance_km, bearing_deg) = match (here, r.lat, r.lon) {
        (Some(h), Some(lat), Some(lon)) => (
            Some(haversine_km(h.station.lat, h.station.lon, lat, lon)),
            Some(bearing_deg(h.station.lat, h.station.lon, lat, lon)),
        ),
        _ => (None, None),
    };
    let power_w = match (r.power, r.power_unit.as_deref()) {
        (Some(p), Some(u)) => to_watts(p, u),
        _ => None,
    };
    let role = match r.device_type.as_deref() {
        Some(t) if t.eq_ignore_ascii_case("T") => Some("tx".to_string()),
        Some(t) if t.eq_ignore_ascii_case("R") => Some("rx".to_string()),
        _ => None,
    };
    let emission_decoded = r.emission.as_deref().and_then(parse_emission);
    let note = note_line(
        r.licensee.as_deref(),
        r.site.as_deref(),
        distance_km,
        power_w,
        r.emission.as_deref(),
        emission_decoded.as_ref(),
    );
    Allocation {
        provider: ACMA_PROVIDER.into(),
        region: ACMA_REGION.into(),
        min_hz: Hz(r.frequency.saturating_sub(half)),
        max_hz: Hz(r.frequency.saturating_add(half)),
        service: "assignment".into(),
        note,
        kind: Some("assignment".into()),
        licensee: r.licensee,
        site: r.site,
        lat: r.lat,
        lon: r.lon,
        distance_km,
        bearing_deg,
        power_w,
        center_hz: Some(Hz(r.frequency)),
        bandwidth_hz: Some(Hz(r.bandwidth)),
        licence_no: r.licence_no,
        callsign: r.callsign,
        role,
        emission: r.emission,
        emission_decoded,
        // The service carries no sync time yet.
        as_of: None,
    }
}

/// Whether the assignment lies inside the reach toward it. Rows without a distance
/// (no station, no reach in km, a site without coordinates) are kept.
fn within_reach(a: &Allocation, here: Option<&Here>) -> bool {
    let (Some(h), Some(d), Some(b)) = (here, a.distance_km, a.bearing_deg) else {
        return true;
    };
    h.reach.km_toward(b, &h.antenna).is_none_or(|km| d <= km)
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

/// `licensee, site, 10 km, 1 W, 16 kHz FM data`, the parts the row lacks dropped.
fn note_line(
    licensee: Option<&str>,
    site: Option<&str>,
    distance_km: Option<f64>,
    power_w: Option<f64>,
    emission: Option<&str>,
    decoded: Option<&Emission>,
) -> Option<String> {
    let parts: Vec<String> = [
        licensee.map(str::to_string),
        site.map(str::to_string),
        distance_km.map(|d| format!("{d:.0} km")),
        power_w.map(|p| format!("{} W", trim_num(p))),
        decoded.map(emission_words).or(emission.map(str::to_string)),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// A number with at most three decimals and no trailing zeros.
fn trim_num(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// `16K0F1D` as `16 kHz FM data`.
fn emission_words(e: &Emission) -> String {
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

/// Great-circle distance in km.
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = (p2 - p1, (lon2 - lon1).to_radians());
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().asin()
}

/// Initial bearing from the first point to the second, 0-360 clockwise from north.
fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn distance_and_bearing() {
        // The demo station (Brisbane Airport) to a synthetic site: about 9.5 km, a little
        // west of north.
        let d = haversine_km(-27.3842, 153.1175, -27.300, 153.100);
        assert!((d - 9.52).abs() < 0.05, "{d}");
        let b = bearing_deg(-27.3842, 153.1175, -27.300, 153.100);
        assert!((345.0..355.0).contains(&b), "{b}");
        assert!((bearing_deg(0.0, 0.0, 1.0, 0.0) - 0.0).abs() < 1e-9);
        assert!((bearing_deg(0.0, 0.0, 0.0, 1.0) - 90.0).abs() < 1e-9);
    }
}
