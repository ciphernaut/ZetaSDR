//! ZetaSDR DSP stages.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::classify::Classifier;
use zeta_core::stage::StageError;
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{Span, Timestamp};

pub mod afsk;
pub mod aprs;
pub mod ax25;
pub mod channel;
pub mod fft;
#[cfg(feature = "gpu")]
pub mod gpu_fft;
pub mod identify;
pub mod measure;
pub mod nbfm;
pub mod util;

#[cfg(feature = "test-fixtures")]
pub mod fixtures;

pub use util::crc16_x25;

/// Register this crate's stages.
pub fn register_all(reg: &mut zeta_core::stage::StageRegistry) {
    reg.register(Arc::new(channel::ChannelStage::new()));
    reg.register(Arc::new(fft::FftStage::new(Arc::new(fft::CpuFft::new()))));
    #[cfg(feature = "gpu")]
    if let Some(gpu) = gpu_fft::GpuFft::try_new() {
        reg.register(Arc::new(fft::FftStage::new(Arc::new(gpu))));
    }
    reg.register(Arc::new(nbfm::NbfmDemodStage::new()));
    reg.register(Arc::new(aprs::AprsDecodeStage::new()));
    reg.register(Arc::new(measure::MeasureStage::new()));
}

/// Whether a hardware GPU adapter is usable for the GPU stages; always false
/// without the `gpu` feature.
pub fn gpu_available() -> bool {
    #[cfg(feature = "gpu")]
    {
        gpu_fft::GpuFft::try_new().is_some()
    }
    #[cfg(not(feature = "gpu"))]
    {
        false
    }
}

/// This crate's classifiers, lowest layer first.
pub fn classifiers() -> Vec<Arc<dyn Classifier>> {
    vec![
        Arc::new(measure::Layer1Classifier),
        Arc::new(identify::DecodeToIdentify),
    ]
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

/// Input frames skipped by this crate's stages because they lagged behind
/// their input, summed over every stage since start-up.
pub static LAGGED_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Current value of [`LAGGED_FRAMES`].
pub fn lagged_frames() -> u64 {
    LAGGED_FRAMES.load(Ordering::Relaxed)
}

/// The loop every single-input stage runs: map each frame on `inputs[0]` through
/// `f` and publish what it returns. Cancellation wins over pending input and
/// returns `Cancelled`; lagged input is skipped, counted in [`LAGGED_FRAMES`]
/// and logged to stderr once per lag event; a closed input returns `Ok(())`;
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
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    LAGGED_FRAMES.fetch_add(n, Ordering::Relaxed);
                    eprintln!("zeta-dsp: stage lagged, {n} frames skipped");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    /// Every object schema with properties denies unknown fields and requires
    /// every property (optional values are required-nullable).
    fn check_strict(v: &Value, path: &str) {
        match v {
            Value::Object(m) => {
                if let Some(Value::Object(props)) = m.get("properties") {
                    assert_eq!(
                        m.get("additionalProperties"),
                        Some(&Value::Bool(false)),
                        "{path} allows extra fields"
                    );
                    let mut want: Vec<&str> = props.keys().map(String::as_str).collect();
                    let mut req: Vec<&str> = m
                        .get("required")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    want.sort();
                    req.sort();
                    assert_eq!(req, want, "{path} does not require every property");
                }
                for (k, c) in m {
                    check_strict(c, &format!("{path}.{k}"));
                }
            }
            Value::Array(a) => a
                .iter()
                .enumerate()
                .for_each(|(i, c)| check_strict(c, &format!("{path}[{i}]"))),
            _ => {}
        }
    }

    #[test]
    fn stage_params_schemas_are_strict() {
        let mut reg = zeta_core::stage::StageRegistry::new();
        super::register_all(&mut reg);
        let stages = reg.describe();
        let stages = stages.as_array().unwrap();
        assert!(stages.iter().any(|s| s["kind"] == "measure"));
        for st in stages {
            check_strict(&st["params_schema"], &format!("stages.{}", st["kind"]));
        }
    }
}
