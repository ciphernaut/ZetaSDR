//! NBFM demodulator stage: phase discriminator, then resampling to the audio rate.

use std::f64::consts::PI;
use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

use crate::resample::Resampler;
use crate::{Input, frame_span, run_map};

/// Phase-discriminator FM demodulation: `arg(x[n] · conj(x[n-1]))` scaled by
/// `rate_hz / (2π · deviation_hz)`, so a tone at full deviation comes out at
/// amplitude 1.0. `prev` is the sample before `iq[0]` and is left holding the
/// last sample. Output is clamped to ±1.0; a zero-magnitude or non-finite
/// sample yields 0.0 (non-finite samples are carried forward as zero).
pub fn nbfm_demod(
    iq: &[Complex<f32>],
    rate_hz: u64,
    deviation_hz: u64,
    prev: &mut Complex<f32>,
) -> Vec<f32> {
    let gain = (rate_hz as f64 / (2.0 * PI * deviation_hz as f64)) as f32;
    iq.iter()
        .map(|&x| {
            let x = if x.re.is_finite() && x.im.is_finite() {
                x
            } else {
                Complex::new(0.0, 0.0)
            };
            // atan2(0, 0) is 0, so a zero sample on either side gives 0.
            let v = (x * prev.conj()).arg() * gain;
            *prev = x;
            if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) }
        })
        .collect()
}

/// Streaming resampler from the discriminator rate to the audio rate (a
/// [`Resampler`], low-pass at `0.45 × min(in, audio)`), carrying history and
/// phase across blocks. Rebuilt when the input or audio rate changes.
#[derive(Debug, Default)]
pub struct AudioFilter {
    key: Option<(u64, u64)>,
    resampler: Option<Resampler>,
}

impl AudioFilter {
    /// Resample `x` at `in_rate_hz` to `audio_rate_hz`; any pair of non-zero
    /// rates, up or down.
    pub fn process(
        &mut self,
        x: &[f32],
        in_rate_hz: u64,
        audio_rate_hz: u64,
    ) -> Result<Vec<f32>, String> {
        let key = (in_rate_hz, audio_rate_hz);
        if self.key != Some(key) {
            // Never keep streaming through the old pair's state if the new one fails.
            self.resampler = None;
            self.key = None;
            self.resampler = Some(Resampler::new(in_rate_hz, audio_rate_hz)?);
            self.key = Some(key);
        }
        Ok(self
            .resampler
            .as_mut()
            .map_or_else(Vec::new, |r| r.process(x)))
    }
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct NbfmParams {
    pub deviation_hz: u64,
    pub audio_rate_hz: u64,
}

/// Demodulates an NBFM IQ stream to audio at `audio_rate_hz`.
pub struct NbfmDemodStage {
    spec: StageSpec,
}

impl NbfmDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "nbfm_demod",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(NbfmParams),
            },
        }
    }
}

impl Default for NbfmDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for NbfmDemodStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: NbfmParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("nbfm_demod params: {e}")))?;
        if p.deviation_hz == 0 {
            return Err(StageError::Failed(
                "nbfm_demod params: deviation_hz must be >= 1".into(),
            ));
        }
        if p.audio_rate_hz == 0 {
            return Err(StageError::Failed(
                "nbfm_demod params: audio_rate_hz must be >= 1".into(),
            ));
        }
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut prev = Complex::new(0.0f32, 0.0);
        let mut filter = AudioFilter::default();
        run_map(inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    prev = Complex::new(0.0, 0.0);
                    filter = AudioFilter::default();
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(
                    "nbfm_demod stage expects iq frames".into(),
                ));
            };
            let rate = block.sample_rate_hz.0;
            let demod = nbfm_demod(&block.samples, rate, p.deviation_hz, &mut prev);
            let samples = filter
                .process(&demod, rate, p.audio_rate_hz)
                .map_err(StageError::Failed)?;
            if samples.is_empty() {
                return Ok(Vec::new());
            }
            let mut prov = frame.provenance.clone();
            prov.span = frame_span(block.t0, block.samples.len(), rate);
            Ok(vec![Frame::new(
                out.clone(),
                prov,
                Payload::Audio {
                    sample_rate_hz: Hz(p.audio_rate_hz),
                    channels: 1,
                    samples: samples.into(),
                },
            )])
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use zeta_core::port::IqBlock;
    use zeta_core::types::{Provenance, SourceId, Span, Timestamp};

    use super::*;
    use crate::fixtures::{awgn, nbfm_modulate, tone};

    /// Frequency (Hz) of the largest DFT bin of `x` in `[1, len/2)` and the RMS of `x`.
    fn peak_and_rms(x: &[f32], rate_hz: f64) -> (f64, f64) {
        let n = x.len();
        let mag = |k: usize| {
            let (re, im) = x
                .iter()
                .enumerate()
                .fold((0.0f64, 0.0f64), |(re, im), (i, &v)| {
                    let ph = -2.0 * std::f64::consts::PI * ((k * i) % n) as f64 / n as f64;
                    (re + v as f64 * ph.cos(), im + v as f64 * ph.sin())
                });
            re * re + im * im
        };
        let k = (1..n / 2)
            .max_by(|&a, &b| mag(a).partial_cmp(&mag(b)).unwrap())
            .unwrap();
        let rms = (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / n as f64).sqrt();
        (k as f64 * rate_hz / n as f64, rms)
    }

    #[test]
    fn demod_recovers_tone() {
        let audio = tone(48_000, 9_600, 1_000.0, 1.0);
        let iq = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, 0.0);
        let mut prev = Complex::new(1.0, 0.0);
        let out = nbfm_demod(&iq, 48_000, 2_500, &mut prev);
        assert_eq!(out.len(), iq.len());
        let (peak, rms) = peak_and_rms(&out[4_800..], 48_000.0);
        assert_eq!(peak, 1_000.0);
        assert!((rms - 0.707).abs() <= 0.0707, "rms {rms}");
    }

    #[test]
    fn demod_noise_is_bounded_and_finite() {
        // Zero signal plus noise, a zero run and a NaN: every output finite and within ±1.
        let mut iq = vec![Complex::new(0.0f32, 0.0); 4_800];
        awgn(&mut iq, 0.0, 7);
        iq.extend(std::iter::repeat_n(Complex::new(0.0, 0.0), 16));
        iq.push(Complex::new(f32::NAN, 0.0));
        iq.push(Complex::new(1.0, 0.0));
        let mut prev = Complex::new(0.0, 0.0);
        let out = nbfm_demod(&iq, 48_000, 2_500, &mut prev);
        assert_eq!(out.len(), iq.len());
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        assert!(out[4_801..4_816].iter().all(|&s| s == 0.0));
        // Wideband noise through the discriminator hits the clamp somewhere.
        assert!(out.iter().any(|s| s.abs() > 0.5));
    }

    #[test]
    fn audio_filter_streaming_matches_one_shot_and_rebuilds_on_rate_change() {
        let x = tone(200_000, 4_801, 1_000.0, 1.0);
        let whole = Resampler::new(200_000, 48_000).unwrap().process(&x);
        let mut f = AudioFilter::default();
        let mut parts = Vec::new();
        for chunk in [&x[..1001], &x[1001..1002], &x[1002..]] {
            parts.extend(f.process(chunk, 200_000, 48_000).unwrap());
        }
        assert_eq!(parts, whole);
        // A new rate pair starts from a fresh resampler.
        let y = f.process(&x[..960], 96_000, 48_000).unwrap();
        assert_eq!(
            y,
            Resampler::new(96_000, 48_000).unwrap().process(&x[..960])
        );
        assert!(f.process(&x, 0, 48_000).is_err());
    }

    fn prov() -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        }
    }

    type Run = tokio::task::JoinHandle<Result<(), StageError>>;

    fn spawn(
        params: serde_json::Value,
    ) -> (broadcast::Sender<Frame>, broadcast::Receiver<Frame>, Run) {
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Audio, prov());
        let rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(8);
        let h = tokio::spawn(async move {
            NbfmDemodStage::new()
                .run(
                    params,
                    vec![rx_in],
                    vec![out],
                    bus,
                    CancellationToken::new(),
                    zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
                )
                .await
        });
        (tx, rx_out, h)
    }

    fn iq_frame(samples: Vec<Complex<f32>>, rate_hz: u64) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(1_000_000_000),
                sample_rate_hz: Hz(rate_hz),
                center_hz: Hz(100_000_000),
                samples: samples.into(),
            }),
        )
    }

    #[tokio::test]
    async fn nbfm_stage_decimates_to_audio_rate() {
        let (tx, mut rx_out, h) =
            spawn(serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000}));
        let audio = tone(48_000, 4_800, 1_000.0, 1.0);
        let iq = nbfm_modulate(&audio, 48_000, 96_000, 2_500.0, 0.0);
        assert_eq!(iq.len(), 9_600);
        tx.send(iq_frame(iq, 96_000)).unwrap();
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.kind, StreamKind::Audio);
        let Payload::Audio {
            sample_rate_hz,
            samples,
            ..
        } = &f.payload
        else {
            panic!("expected an audio payload")
        };
        assert_eq!(*sample_rate_hz, Hz(48_000));
        assert_eq!(samples.len(), 4_800);
        let sp = &f.provenance.span;
        assert_eq!(sp.start_ns, Timestamp(1_000_000_000));
        assert_eq!(sp.end_ns.0 - sp.start_ns.0, 100_000_000);
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn nbfm_stage_resamples_a_non_integer_ratio() {
        let (tx, mut rx_out, h) =
            spawn(serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000}));
        tx.send(iq_frame(vec![Complex::new(1.0, 0.0); 2_000], 200_000))
            .unwrap();
        let f = rx_out.recv().await.unwrap();
        let Payload::Audio {
            sample_rate_hz,
            samples,
            ..
        } = &f.payload
        else {
            panic!("expected an audio payload")
        };
        assert_eq!(*sample_rate_hz, Hz(48_000));
        assert_eq!(samples.len(), 480);
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn nbfm_stage_rejects_zero_audio_rate() {
        let (_tx, _rx, h) = spawn(serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 0}));
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => {
                assert_eq!(m, "nbfm_demod params: audio_rate_hz must be >= 1")
            }
            r => panic!("expected Failed, got {r:?}"),
        }
    }

    #[tokio::test]
    async fn nbfm_resets_on_lag() {
        let (tx, mut rx_out, h) =
            spawn(serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000}));
        let a = nbfm_modulate(
            &tone(48_000, 4_800, 1_000.0, 1.0),
            48_000,
            96_000,
            2_500.0,
            0.3,
        );
        let b = nbfm_modulate(
            &tone(48_000, 4_800, 700.0, 1.0),
            48_000,
            96_000,
            2_500.0,
            2.0,
        );
        tx.send(iq_frame(a, 96_000)).unwrap();
        rx_out.recv().await.unwrap();
        tx.send(iq_frame(vec![Complex::new(0.0, 1.0); 9_600], 96_000))
            .unwrap();
        for _ in 0..8 {
            tx.send(iq_frame(b.clone(), 96_000)).unwrap();
        }
        let f = rx_out.recv().await.unwrap();
        let Payload::Audio { samples, .. } = &f.payload else {
            panic!("expected an audio payload")
        };
        let mut prev = Complex::new(0.0f32, 0.0);
        let demod = nbfm_demod(&b, 96_000, 2_500, &mut prev);
        let want = AudioFilter::default()
            .process(&demod, 96_000, 48_000)
            .unwrap();
        assert_eq!(samples.len(), want.len());
        for (g, w) in samples.iter().zip(&want) {
            assert!((g - w).abs() < 1e-6, "{g} vs {w}");
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }
}
