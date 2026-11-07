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
pub mod bit_view;
pub mod burst_capture;
pub mod capture_in;
pub mod catalogue;
pub mod channel;
pub mod clock;
pub mod correlate;
pub mod crc;
pub mod denoise;
pub mod detect;
pub mod fft;
pub mod frame_decode;
pub mod frame_parse;
pub mod frame_quick;
pub mod framing;
pub mod fsk;
#[cfg(feature = "gpu")]
pub mod gpu_fft;
pub mod identify;
pub mod measure;
pub mod mix;
pub mod nbfm;
pub mod offline;
pub mod ook;
pub mod parsers;
pub mod pfb;
pub mod pocsag;
pub mod psk;
pub mod rds;
pub mod resample;
pub mod rs41_frame;
pub mod slicer;
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
    reg.register(Arc::new(detect::DetectStage::new()));
    reg.register(Arc::new(burst_capture::BurstCaptureStage::new()));
    reg.register(Arc::new(capture_in::CaptureInStage::new()));
    reg.register(Arc::new(resample::ResampleStage::new()));
    reg.register(Arc::new(fsk::FskDemodStage::new()));
    reg.register(Arc::new(ook::OokDemodStage::new()));
    reg.register(Arc::new(psk::PskDemodStage::new()));
    reg.register(Arc::new(clock::ClockRecoverStage::new()));
    reg.register(Arc::new(slicer::SlicerStage::new()));
    reg.register(Arc::new(correlate::CorrelateStage::new()));
    reg.register(Arc::new(frame_decode::FrameDecodeStage::new()));
    reg.register(Arc::new(frame_parse::FrameParseStage::new()));
    reg.register(Arc::new(bit_view::BitViewStage::new()));
    reg.register(Arc::new(denoise::DenoiseStage::new()));
    reg.register(Arc::new(vad::VadStage::new()));
    reg.register(Arc::new(mix::MixStage::new()));
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

/// This crate's classifiers, lowest layer first; decode-to-identify tries the rows
/// `modes` holds at each run.
pub fn classifiers(modes: zeta_core::modes::ModesHandle) -> Vec<Arc<dyn Classifier>> {
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

/// Fewest samples per symbol a layer-4 demodulator (`fsk_demod`, `ook_demod`,
/// `psk_demod`) accepts at its input.
pub const MIN_SAMPLES_PER_SYMBOL: u64 = 4;

/// Refuse an input of `rate_hz` under [`MIN_SAMPLES_PER_SYMBOL`] samples per symbol at
/// `symbol_rate_baud`, naming the rate to resample to; the message starts with `kind`.
pub fn sps_check(kind: &str, rate_hz: u64, symbol_rate_baud: u32) -> Result<(), String> {
    let need = MIN_SAMPLES_PER_SYMBOL * symbol_rate_baud as u64;
    if rate_hz >= need {
        return Ok(());
    }
    let sps = rate_hz as f64 / symbol_rate_baud as f64;
    Err(format!(
        "{kind}: {rate_hz} Hz is {sps:.1} samples per symbol at {symbol_rate_baud} Bd; resample to at least {need} Hz"
    ))
}

/// Least gap, in nanoseconds, that [`is_seam`] calls a time discontinuity.
pub const SEAM_MIN_NS: u64 = 1_000_000;

/// Sample periods of a frame's own rate that [`is_seam`] tolerates.
const SEAM_PERIODS: u64 = 2;

/// Whether a frame starting at `start_ns` is a time discontinuity after one that ended at
/// `prev_end_ns`: the gap (either way) exceeds the larger of [`SEAM_PERIODS`] sample
/// periods at `rate_hz` and [`SEAM_MIN_NS`]. `capture_in`'s block ends may fall 1 ns short
/// of the next block's start at rates not divisible by 20, which is no seam.
pub fn is_seam(prev_end_ns: u64, start_ns: u64, rate_hz: u64) -> bool {
    let periods = (SEAM_PERIODS * 1_000_000_000)
        .checked_div(rate_hz)
        .unwrap_or(0);
    prev_end_ns.abs_diff(start_ns) > periods.max(SEAM_MIN_NS)
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
    inputs: Vec<broadcast::Receiver<Frame>>,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
    mut f: F,
) -> Result<(), StageError>
where
    F: FnMut(Input) -> Result<Vec<Frame>, StageError> + Send,
{
    run_map_ending(kind, inputs, bus, cancel, move |input| match input {
        Some(input) => f(input),
        None => Ok(Vec::new()),
    })
    .await
}

/// [`run_map`] for a stage that flushes what it holds when its input ends: `f` is
/// called with `None` once the input closes, and what it returns is published before
/// the stage returns `Ok(())`.
pub(crate) async fn run_map_ending<F>(
    kind: &'static str,
    mut inputs: Vec<broadcast::Receiver<Frame>>,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
    mut f: F,
) -> Result<(), StageError>
where
    F: FnMut(Option<Input>) -> Result<Vec<Frame>, StageError> + Send,
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
                    for out in f(Some(Input::Frame(frame)))? {
                        bus.publish(out);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    note_lag(kind, n);
                    for out in f(Some(Input::Lagged { missed: n }))? {
                        bus.publish(out);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    for out in f(None)? {
                        bus.publish(out);
                    }
                    return Ok(());
                }
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
    fn is_seam_tolerates_a_nanosecond_short_gap() {
        assert!(!super::is_seam(1_000_000_000, 1_000_000_001, 48_000));
        assert!(!super::is_seam(1_000_000_001, 1_000_000_000, 48_000));
    }

    #[test]
    fn is_seam_flags_a_two_second_gap() {
        assert!(super::is_seam(1_000_000_000, 3_000_000_000, 48_000));
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
