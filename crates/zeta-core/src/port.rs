//! The port: the boundary every signal source sits behind.

use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{Capabilities, Hz, SourceId, Timestamp};

/// A centre frequency and the bandwidth captured around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Tuning {
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
}

/// A source's overall receive gain: `db` in decibels (`null` while automatic
/// gain is on), the range a value may take, and whether automatic gain is on and
/// available at all.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GainInfo {
    /// The gain in dB; `null` while automatic gain is on.
    #[serde(deserialize_with = "nullable_f64")]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub db: Option<f64>,
    pub min_db: f64,
    pub max_db: f64,
    /// Whether the driver's automatic gain is on.
    pub automatic: bool,
    /// Whether the device has automatic gain (`gain_db: null` is accepted).
    pub automatic_available: bool,
}

/// Deserialize a required `Option<f64>`: present, a number or `null`.
pub(crate) fn nullable_f64<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<f64>, D::Error> {
    <Option<f64> as Deserialize>::deserialize(d)
}

/// A streaming source's health counters since it opened: the sample rate
/// actually delivered over the last 10 s, to the hertz (`null` before two
/// blocks arrived), reads that overflowed (samples lost in the driver), reads
/// that timed out, and stream restarts (a stream whose stamps ran ahead of the
/// wall clock).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PortStats {
    #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
    pub measured_rate_hz: Option<u64>,
    pub overflows: u64,
    pub timeouts: u64,
    pub restarts: u64,
}

/// A block of complex baseband samples.
#[derive(Clone, Debug)]
pub struct IqBlock {
    pub t0: Timestamp,
    pub sample_rate_hz: Hz,
    pub center_hz: Hz,
    pub samples: Arc<[Complex<f32>]>,
}

pub type IqReceiver = tokio::sync::broadcast::Receiver<IqBlock>;

/// A port's refusal. It serialises through [`PortErrorWire`] (`kind` names the
/// variant) so a worker process can hand it to the node unchanged; `Display`
/// is the same text after the trip.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PortError {
    /// A `suggested` above `max` means the source itself ships more than the
    /// configured cap allows (a remote node's fixed rate); the message says so.
    #[error("{field} {value} out of range {min}..={max}{}", over_cap(*suggested, *max))]
    OutOfRange {
        field: &'static str,
        value: u64,
        min: u64,
        max: u64,
        suggested: Option<u64>,
    },
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("io: {0}")]
    Io(String),
}

/// The JSON shape of a [`PortError`]: `{"kind": "out_of_range", "field", "value",
/// "min", "max", "suggested"}`, `{"kind": "unsupported", "message"}` or
/// `{"kind": "io", "message"}`. It exists because `OutOfRange::field` is a
/// `&'static str`, which serde cannot fill from data without help.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PortErrorWire {
    OutOfRange {
        field: String,
        value: u64,
        min: u64,
        max: u64,
        suggested: Option<u64>,
    },
    Unsupported {
        message: String,
    },
    Io {
        message: String,
    },
}

// Written by hand: a derive would see `&'static str` in `OutOfRange` and
// demand input that lives for `'static`.
impl Serialize for PortError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        PortErrorWire::from(self.clone()).serialize(s)
    }
}

impl<'de> Deserialize<'de> for PortError {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        PortErrorWire::deserialize(d).map(PortError::from)
    }
}

impl From<PortError> for PortErrorWire {
    fn from(e: PortError) -> Self {
        match e {
            PortError::OutOfRange {
                field,
                value,
                min,
                max,
                suggested,
            } => PortErrorWire::OutOfRange {
                field: field.to_string(),
                value,
                min,
                max,
                suggested,
            },
            PortError::Unsupported(message) => PortErrorWire::Unsupported { message },
            PortError::Io(message) => PortErrorWire::Io { message },
        }
    }
}

impl From<PortErrorWire> for PortError {
    fn from(w: PortErrorWire) -> Self {
        match w {
            PortErrorWire::OutOfRange {
                field,
                value,
                min,
                max,
                suggested,
            } => PortError::OutOfRange {
                // The names ports use map back to their literals; any other
                // is leaked (rare, and a few bytes once per distinct error
                // received from a peer).
                field: match field.as_str() {
                    "center_hz" => "center_hz",
                    "bandwidth_hz" => "bandwidth_hz",
                    _ => Box::leak(field.into_boxed_str()),
                },
                value,
                min,
                max,
                suggested,
            },
            PortErrorWire::Unsupported { message } => PortError::Unsupported(message),
            PortErrorWire::Io { message } => PortError::Io(message),
        }
    }
}

/// The hint for an `OutOfRange` whose suggestion exceeds its maximum: the
/// source ships that rate and only a larger cap admits it.
pub fn over_cap(suggested: Option<u64>, max: u64) -> String {
    match suggested {
        Some(s) if s > max => {
            format!("; remote ships {s} Hz; pass max_bandwidth_hz={s} to allow it")
        }
        _ => String::new(),
    }
}

/// A signal source.
///
/// Contract: `tune` and `current` return the capture band, whose
/// `bandwidth_hz` equals the complex sample rate of the blocks the port emits;
/// a port's producer exits when it has no subscribers or is dropped;
/// consecutive blocks have `t0` rising by exactly
/// `samples · 1e9 / sample_rate_hz` (integer nanoseconds) unless samples were
/// really lost, in which case `t0` jumps forward and never backward;
/// unrecoverable failures are reported through [`Port::fault`], never by
/// panicking or by closing the stream silently.
#[async_trait]
pub trait Port: Send + Sync {
    fn id(&self) -> &SourceId;
    fn capabilities(&self) -> &Capabilities;
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError>;
    fn current(&self) -> Option<Tuning>;
    fn subscribe(&self) -> IqReceiver;
    /// The port's unrecoverable failure, if any (a remote receiver that is
    /// full or gone, a device that was unplugged). Jobs reading the port fail
    /// with it as their reason. A successful `tune` clears it.
    fn fault(&self) -> Option<String> {
        None
    }
    /// Called under the lease gate when the source's last holder released; a
    /// port may stop upstream work and clear `current`. The next `tune` must
    /// work as from scratch. Must not block for long: the gate is held.
    async fn idle(&self) {}
    /// The overall receive gain, if the source has an adjustable one.
    fn gain(&self) -> Option<GainInfo> {
        None
    }
    /// The port's stream statistics, if it keeps any (a device stream does; a
    /// recording or a remote receiver does not).
    fn stats(&self) -> Option<PortStats> {
        None
    }
    /// Set the overall receive gain to `db` dB, or turn the driver's automatic
    /// gain on with `None`; the result is the gain after the change. A refusal
    /// (no adjustable gain, no automatic gain, a value out of range) is
    /// `Unsupported` with the text the caller sees.
    async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        let _ = db;
        Err(PortError::Unsupported(
            "source has no adjustable gain".into(),
        ))
    }
}
