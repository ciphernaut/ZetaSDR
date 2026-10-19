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

/// A block of complex baseband samples.
#[derive(Clone, Debug)]
pub struct IqBlock {
    pub t0: Timestamp,
    pub sample_rate_hz: Hz,
    pub center_hz: Hz,
    pub samples: Arc<[Complex<f32>]>,
}

pub type IqReceiver = tokio::sync::broadcast::Receiver<IqBlock>;

#[derive(Debug, thiserror::Error)]
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
}
