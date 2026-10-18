//! ZetaSDR DSP stages.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::StageError;
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{Span, Timestamp};

pub mod channel;
pub mod fft;
pub mod nbfm;
pub mod util;

#[cfg(feature = "test-fixtures")]
pub mod fixtures;

pub use util::crc16_x25;

/// Register this crate's stages.
pub fn register_all(reg: &mut zeta_core::stage::StageRegistry) {
    reg.register(Arc::new(channel::ChannelStage::new()));
    reg.register(Arc::new(fft::FftStage::new(Arc::new(fft::CpuFft::new()))));
    reg.register(Arc::new(nbfm::NbfmDemodStage::new()));
}

/// Span of `n` samples at `rate_hz` starting at `t0`; zero-length when `rate_hz` is 0.
pub(crate) fn frame_span(t0: Timestamp, n: usize, rate_hz: u64) -> Span {
    let dur = if rate_hz == 0 {
        0
    } else {
        (n as u128 * 1_000_000_000 / rate_hz as u128) as u64
    };
    Span {
        start_ns: t0,
        end_ns: Timestamp(t0.0.saturating_add(dur)),
    }
}

/// The loop every single-input stage runs: map each frame on `inputs[0]` through
/// `f` and publish what it returns. Cancellation wins over pending input and
/// returns `Cancelled`; lagged input is skipped; a closed input returns `Ok(())`;
/// an error from `f` ends the stage with that error.
pub(crate) async fn run_map<F>(
    mut inputs: Vec<broadcast::Receiver<Frame>>,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
    mut f: F,
) -> Result<(), StageError>
where
    F: FnMut(Frame) -> Result<Vec<Frame>, StageError> + Send,
{
    if inputs.len() != 1 {
        return Err(StageError::Failed("expected 1 input".into()));
    }
    let rx = &mut inputs[0];
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(StageError::Cancelled),
            r = rx.recv() => match r {
                Ok(frame) => {
                    for out in f(frame)? {
                        bus.publish(out);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}
