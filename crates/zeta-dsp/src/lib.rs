//! ZetaSDR DSP stages.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::classify::Classifier;
use zeta_core::stage::StageError;
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{Span, Timestamp};

pub mod acars;
pub mod afsk;
pub mod am;
pub mod aprs;
pub mod ax25;
pub mod bank;
pub mod catalogue;
pub mod channel;
pub mod denoise;
pub mod fft;
#[cfg(feature = "gpu")]
pub mod gpu_fft;
pub mod identify;
pub mod measure;
pub mod nbfm;
pub mod pfb;
pub mod pocsag;
pub mod rds;
pub mod resample;
pub mod squelch;
pub mod synth;
pub mod util;
pub mod vad;
pub mod wfm;

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
    reg.register(Arc::new(wfm::WfmDemodStage::new()));
    reg.register(Arc::new(am::AmDemodStage::new()));
    reg.register(Arc::new(am::SsbDemodStage::new()));
    reg.register(Arc::new(aprs::AprsDecodeStage::new()));
    reg.register(Arc::new(pocsag::PocsagDecodeStage::new()));
    reg.register(Arc::new(rds::RdsDecodeStage::new()));
    reg.register(Arc::new(acars::AcarsDecodeStage::new()));
    reg.register(Arc::new(measure::MeasureStage::new()));
    reg.register(Arc::new(denoise::DenoiseStage::new()));
    reg.register(Arc::new(vad::VadStage::new()));
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

/// This crate's classifiers, lowest layer first; decode-to-identify tries the rows of
/// `modes`.
pub fn classifiers(modes: Arc<zeta_core::modes::ModeRegistry>) -> Vec<Arc<dyn Classifier>> {
    vec![
        Arc::new(measure::Layer1Classifier),
        Arc::new(identify::DecodeToIdentify::new(modes)),
        Arc::new(catalogue::CatalogueClassifier),
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

/// [`LAGGED_FRAMES`] by stage kind, in the order each kind first lagged.
static LAGGED_BY_KIND: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());

/// Input frames skipped since start-up, per stage kind that ever lagged.
pub fn lagged_frames_by_kind() -> Vec<(&'static str, u64)> {
    LAGGED_BY_KIND.lock().unwrap().clone()
}

/// A `kind` stage skipped `n` input frames: count them in [`LAGGED_FRAMES`] and
/// [`lagged_frames_by_kind`], and say so on stderr. Public for stages of other
/// crates (`transcribe`), so every lag is counted by kind in one place.
pub fn note_lag(kind: &'static str, n: u64) {
    LAGGED_FRAMES.fetch_add(n, Ordering::Relaxed);
    {
        let mut by_kind = LAGGED_BY_KIND.lock().unwrap();
        match by_kind.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, c)) => *c += n,
            None => by_kind.push((kind, n)),
        }
    }
    eprintln!("zeta-dsp: {kind} stage lagged, {n} frames skipped");
}

/// What a stage's closure sees: a frame, or a marker that `missed` frames were
/// skipped before the next one, so stateful stages must reset.
pub(crate) enum Input {
    Frame(Frame),
    #[allow(dead_code)] // reported for stages that want the count
    Lagged {
        missed: u64,
    },
}

/// The loop every single-input stage runs: map each frame on `inputs[0]` through
/// `f` and publish what it returns. Cancellation wins over pending input and
/// returns `Cancelled`; lagged input is counted under `kind` (see [`note_lag`]),
/// logged to stderr once per lag event and passed to `f` as [`Input::Lagged`] so the
/// stage can drop its streaming state; a closed input returns `Ok(())`; an
/// error from `f` ends the stage with that error.
pub(crate) async fn run_map<F>(
    kind: &'static str,
    mut inputs: Vec<broadcast::Receiver<Frame>>,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
    mut f: F,
) -> Result<(), StageError>
where
    F: FnMut(Input) -> Result<Vec<Frame>, StageError> + Send,
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
                    for out in f(Input::Frame(frame))? {
                        bus.publish(out);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    note_lag(kind, n);
                    for out in f(Input::Lagged { missed: n })? {
                        bus.publish(out);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::Value;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Provenance, SourceId, Span, StreamId, Timestamp};

    use super::{Input, run_map};

    #[tokio::test]
    async fn run_map_passes_lag_marker() {
        let prov = Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let (tx, rx) = broadcast::channel(2);
        for i in 0..4 {
            tx.send(Frame::new(
                StreamId::new(),
                prov.clone(),
                Payload::Event(serde_json::json!(i)),
            ))
            .unwrap();
        }
        drop(tx);
        let mut seen = Vec::new();
        run_map(
            "marker",
            vec![rx],
            Arc::new(StreamBus::new(4)),
            CancellationToken::new(),
            |i| {
                seen.push(match i {
                    Input::Lagged { missed } => format!("lag{missed}"),
                    Input::Frame(f) => match f.payload {
                        Payload::Event(v) => v.to_string(),
                        _ => unreachable!(),
                    },
                });
                Ok(Vec::new())
            },
        )
        .await
        .unwrap();
        assert_eq!(seen, ["lag2", "2", "3"]);
    }

    #[tokio::test]
    async fn lag_line_names_the_kind() {
        let prov = Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let (tx, rx) = broadcast::channel(2);
        for i in 0..5 {
            tx.send(Frame::new(
                StreamId::new(),
                prov.clone(),
                Payload::Event(serde_json::json!(i)),
            ))
            .unwrap();
        }
        drop(tx);
        run_map(
            "passthrough",
            vec![rx],
            Arc::new(StreamBus::new(4)),
            CancellationToken::new(),
            |_| Ok(Vec::new()),
        )
        .await
        .unwrap();
        let by_kind = super::lagged_frames_by_kind();
        assert!(
            by_kind.iter().any(|&(k, n)| k == "passthrough" && n > 0),
            "{by_kind:?}"
        );
    }

    /// Every object schema with properties denies unknown fields and requires
    /// every property (optional values are required-nullable), except one
    /// with a schema `default`, which older persisted graphs may lack.
    fn check_strict(v: &Value, path: &str) {
        match v {
            Value::Object(m) => {
                if let Some(Value::Object(props)) = m.get("properties") {
                    assert_eq!(
                        m.get("additionalProperties"),
                        Some(&Value::Bool(false)),
                        "{path} allows extra fields"
                    );
                    let mut want: Vec<&str> = props
                        .iter()
                        .filter(|(_, p)| p.get("default").is_none())
                        .map(|(k, _)| k.as_str())
                        .collect();
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
