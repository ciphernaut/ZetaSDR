//! `sink_sondehub`: radiosonde telemetry uploaded to SondeHub in batches (spec 9).
//!
//! The stage reads the events of a sonde decoder (`protocol` `rs41`, `dfm` or `m20`),
//! keeps those that carry a `position` and an `id` (the sonde's serial), and every
//! `batch_ms` sends what it collected as one `PUT` of a JSON array to `url`, each
//! element in SondeHub's v2 telemetry shape with the station's callsign and position.
//! It has no outputs. After every batch the job's context holds
//! `sondehub = {sent, failed, last_error}`: a non-2xx answer or a transport error counts
//! `failed` once (the batch is dropped, never retried) and sets `last_error`; the next
//! batch still goes. Events of any other protocol, or without a position, a `datetime`, a
//! `frame` or a numeric altitude (SondeHub rejects those), are skipped, so `failed` counts
//! transport trouble only.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{StreamId, StreamKind};

/// The stage kind.
pub const SONDEHUB_KIND: &str = "sink_sondehub";
/// The default batch interval.
pub const BATCH_MS: u64 = 5000;
/// SondeHub's v2 telemetry endpoint.
pub const SONDEHUB_URL: &str = "https://api.v2.sondehub.org/sondes/telemetry";
/// The shortest batch interval accepted.
const BATCH_MS_MIN: u64 = 100;
/// How long one upload may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The failure when a position is needed and neither the params nor a station give one.
const NO_STATION: &str = "sink.params.lat: no station set (station set --lat --lon --alt-m)";

fn default_batch_ms() -> u64 {
    BATCH_MS
}

fn default_url() -> String {
    SONDEHUB_URL.to_string()
}

/// `sink_sondehub`'s params: the uploading station and where to send.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SondehubParams {
    /// The station's callsign, shown on SondeHub.
    pub callsign: String,
    /// The uploader's latitude in degrees; the node's station when omitted.
    #[serde(default)]
    pub lat: Option<f64>,
    /// The uploader's longitude in degrees; the node's station when omitted.
    #[serde(default)]
    pub lon: Option<f64>,
    /// The uploader's altitude in metres; the node's station when omitted.
    #[serde(default)]
    pub alt_m: Option<f64>,
    /// How often a batch is sent, in milliseconds (at least 100).
    #[serde(default = "default_batch_ms")]
    pub batch_ms: u64,
    /// The telemetry endpoint.
    #[serde(default = "default_url")]
    pub url: String,
}

/// Event stream in, nothing out: sonde telemetry to SondeHub.
pub struct SondehubStage {
    spec: StageSpec,
}

impl SondehubStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: SONDEHUB_KIND,
                inputs: vec![StreamKind::Event],
                outputs: vec![],
                params_schema: schemars::schema_for!(SondehubParams),
            },
        }
    }
}

impl Default for SondehubStage {
    fn default() -> Self {
        Self::new()
    }
}

/// The manufacturer and type SondeHub names a decoder `protocol` by.
fn sonde_type(protocol: &str) -> Option<(&'static str, &'static str)> {
    match protocol {
        "rs41" => Some(("Vaisala", "RS41")),
        "dfm" => Some(("Graw", "DFM")),
        "m20" => Some(("Meteomodem", "M20")),
        _ => None,
    }
}

/// `now_ns` (UNIX nanoseconds) as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn iso8601(now_ns: u64) -> String {
    let ms = now_ns / 1_000_000;
    let (secs, millis) = (ms / 1000, ms % 1000);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// The telemetry object for sonde event `ev` as `p`'s station received it at
/// `received`, or `None` for an event that is not sonde telemetry SondeHub accepts: one
/// with a position (a numeric altitude included), a `datetime` and a `frame`.
fn telemetry(p: &SondehubParams, pos: [f64; 3], ev: &Value, received: &str) -> Option<Value> {
    let (manufacturer, kind) = sonde_type(ev["protocol"].as_str()?)?;
    let fix = ev.get("position")?;
    let serial = ev["id"].as_str()?;
    fix["alt_m"].as_f64()?;
    ev["datetime"].as_str()?;
    ev["frame"].as_u64()?;
    let mut t = json!({
        "software_name": "zetasdr",
        "software_version": env!("CARGO_PKG_VERSION"),
        "uploader_callsign": p.callsign,
        "uploader_position": pos,
        "time_received": received,
        "manufacturer": manufacturer,
        "type": kind,
        "serial": serial,
        "lat": fix["lat"],
        "lon": fix["lon"],
        "alt": fix["alt_m"],
    });
    let o = t.as_object_mut()?;
    for k in [
        "datetime", "frame", "vel_h", "vel_v", "heading", "sats", "batt",
    ] {
        if let Some(v) = ev.get(k).filter(|v| !v.is_null()) {
            o.insert(k.into(), v.clone());
        }
    }
    Some(t)
}

/// What the job's context says of the uploads so far.
#[derive(Default)]
struct Tally {
    sent: u64,
    failed: u64,
    last_error: Option<String>,
}

impl Tally {
    fn note(&self, ctx: &StageContext) {
        ctx.note(json!({
            "sondehub": {
                "sent": self.sent,
                "failed": self.failed,
                "last_error": self.last_error,
            }
        }));
    }
}

/// Send `batch` once; the error text on failure.
async fn upload(client: &reqwest::Client, url: &str, batch: &[Value]) -> Result<(), String> {
    let r = client
        .put(url)
        .json(batch)
        .send()
        .await
        .map_err(|e| format!("sondehub: {e}"))?;
    match r.status() {
        s if s.is_success() => Ok(()),
        s => Err(format!("sondehub: HTTP {}", s.as_u16())),
    }
}

#[async_trait]
impl Stage for SondehubStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &Value) -> Result<(), String> {
        let p: SondehubParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        if p.callsign.trim().is_empty() {
            return Err("callsign is empty".into());
        }
        if p.lat.is_some_and(|v| !(-90.0..=90.0).contains(&v))
            || p.lon.is_some_and(|v| !(-180.0..=180.0).contains(&v))
        {
            return Err(format!("lat {:?} / lon {:?} out of range", p.lat, p.lon));
        }
        if p.batch_ms < BATCH_MS_MIN {
            return Err(format!(
                "batch_ms {} under the minimum {BATCH_MS_MIN}",
                p.batch_ms
            ));
        }
        if !p.url.starts_with("http://") && !p.url.starts_with("https://") {
            return Err(format!("url {} is not http(s)", p.url));
        }
        Ok(())
    }

    async fn run(
        &self,
        params: Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        _outputs: Vec<StreamId>,
        _bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let fail = |m: String| StageError::Failed(format!("{SONDEHUB_KIND}: {m}"));
        self.check_params(&params).map_err(&fail)?;
        let p: SondehubParams =
            serde_json::from_value(params).map_err(|e| fail(format!("params: {e}")))?;
        let pos = match (p.lat, p.lon, p.alt_m, ctx.station.as_ref()) {
            (Some(lat), Some(lon), Some(alt), _) => [lat, lon, alt],
            (lat, lon, alt, Some(st)) => [
                lat.unwrap_or(st.lat),
                lon.unwrap_or(st.lon),
                alt.unwrap_or(st.alt_m),
            ],
            _ => {
                // No position to report from: say so in the context and idle (the events are
                // consumed, nothing is uploaded), never failing the decoder it rides on.
                if inputs.len() != 1 {
                    return Err(fail("expected 1 input".into()));
                }
                return idle(inputs.remove(0), &ctx, &cancel).await;
            }
        };
        if inputs.len() != 1 {
            return Err(fail("expected 1 input".into()));
        }
        let mut rx = inputs.remove(0);
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("zetasdr/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| fail(format!("http client: {e}")))?;
        let every = Duration::from_millis(p.batch_ms);
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut batch: Vec<Value> = Vec::new();
        let mut tally = Tally::default();
        let mut open = true;
        while open {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                f = rx.recv() => match f {
                    Ok(Frame { payload: Payload::Event(ev), .. }) => {
                        if let Some(t) = telemetry(&p, pos, &ev, &iso8601(now_ns())) {
                            batch.push(t);
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => open = false,
                },
                _ = tick.tick() => {
                    if !flush(&client, &p.url, &mut batch, &mut tally, &ctx, &cancel).await {
                        return Err(StageError::Cancelled);
                    }
                }
            }
        }
        // The input closed: what is collected still goes.
        flush(&client, &p.url, &mut batch, &mut tally, &ctx, &cancel).await;
        Ok(())
    }
}

/// A sink with no position (none in its params, no station): `job_context.sondehub`
/// carries [`NO_STATION`] as `last_error` and `idle: true`, and the input is drained until it
/// closes or the stage is cancelled.
async fn idle(
    mut rx: broadcast::Receiver<Frame>,
    ctx: &StageContext,
    cancel: &CancellationToken,
) -> Result<(), StageError> {
    ctx.note(json!({
        "sondehub": {"sent": 0, "failed": 0, "last_error": NO_STATION, "idle": true}
    }));
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(StageError::Cancelled),
            f = rx.recv() => if let Err(broadcast::error::RecvError::Closed) = f {
                return Ok(());
            },
        }
    }
}

/// Send and clear `batch` (nothing when empty), counting the outcome in `tally` and the
/// job's context. `false` when `cancel` fired meanwhile.
async fn flush(
    client: &reqwest::Client,
    url: &str,
    batch: &mut Vec<Value>,
    tally: &mut Tally,
    ctx: &StageContext,
    cancel: &CancellationToken,
) -> bool {
    if batch.is_empty() {
        return true;
    }
    let sending = std::mem::take(batch);
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return false,
        r = upload(client, url, &sending) => r,
    };
    match result {
        Ok(()) => tally.sent += sending.len() as u64,
        Err(e) => {
            tally.failed += 1;
            tally.last_error = Some(e);
        }
    }
    tally.note(ctx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_iso_8601_utc() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso8601(1_782_690_123_456_000_000),
            "2026-06-28T23:42:03.456Z"
        );
        assert_eq!(iso8601(951_782_400_000_000_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn protocols_map_to_manufacturers() {
        assert_eq!(sonde_type("rs41"), Some(("Vaisala", "RS41")));
        assert_eq!(sonde_type("dfm"), Some(("Graw", "DFM")));
        assert_eq!(sonde_type("m20"), Some(("Meteomodem", "M20")));
        assert_eq!(sonde_type("pocsag"), None);
    }
}
