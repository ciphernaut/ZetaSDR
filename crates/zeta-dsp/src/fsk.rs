//! FSK demodulation: a decision signal out of 2- or 4-level FSK.
//!
//! A quadrature discriminator gives the instantaneous frequency, a low-pass keeps the
//! keying and drops the noise above it, and a scale puts the outer tones at ±1, so
//! `clock_recover` and `slicer` read one sample stream whatever the deviation.

use std::f64::consts::TAU;
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

use crate::util::{StreamingFir, lowpass_taps};
use crate::{Input, is_seam, run_map, sps_check};

/// The stage kind, as graphs name it.
pub const FSK_DEMOD_KIND: &str = "fsk_demod";

/// The decision low-pass cutoff as a multiple of the symbol rate (times the order for
/// FSK): wide enough to keep a symbol's transition, narrow enough to drop the noise
/// above the keying.
pub const DISCRIMINATOR_LOWPASS_FACTOR: f64 = 0.6;

/// Time constant of a demodulator's running scale (the FSK peak, the OOK levels), in
/// symbol periods.
pub(crate) const NORMALISER_SYMBOLS: f64 = 64.0;

/// Length of the decision low-pass, in symbol periods (rounded up to an odd tap count).
const LOWPASS_SPAN_SYMBOLS: f64 = 2.0;

/// Highest decision low-pass cutoff, as a fraction of the input rate: under Nyquist at
/// [`crate::MIN_SAMPLES_PER_SYMBOL`] for 4-FSK, where the factor alone would pass it.
const MAX_CUTOFF_FRACTION: f64 = 0.45;

/// Floor of a running scale, so silence does not divide by zero.
pub(crate) const MIN_SCALE: f64 = 1e-12;

const ORDER_DEFAULT: u8 = 2;

fn default_order() -> u8 {
    ORDER_DEFAULT
}

/// `fsk_demod` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct FskDemodParams {
    /// The symbol rate in baud.
    pub symbol_rate_baud: u32,
    /// The number of tones, 2 or 4.
    #[serde(default = "default_order")]
    pub order: u8,
    /// The spacing between adjacent tones in hertz; `null` scales by the running peak.
    #[serde(default)]
    pub tone_spacing_hz: Option<u64>,
}

/// The decision low-pass at `cutoff_hz` (capped at [`MAX_CUTOFF_FRACTION`] of the rate)
/// spanning [`LOWPASS_SPAN_SYMBOLS`] symbols of `baud` at `rate_hz`, and the length of
/// its start-up transient in samples (its history, filled from zero).
pub(crate) fn decision_lowpass(cutoff_hz: f64, rate_hz: u64, baud: u32) -> (StreamingFir, usize) {
    let rate = rate_hz as f64;
    let taps = (LOWPASS_SPAN_SYMBOLS * rate / baud as f64).ceil() as usize | 1;
    let fir = StreamingFir::new(lowpass_taps(
        cutoff_hz.min(MAX_CUTOFF_FRACTION * rate),
        rate,
        taps,
    ));
    (fir, taps - 1)
}

/// Splits a sample stream into symbol periods (fractional lengths carried) and reports
/// each period's minimum and maximum as it closes; the first `warmup` samples (the
/// low-pass filling from zero) open no period, so they do not seed a running scale.
pub(crate) struct Periods {
    period: f64,
    warmup: usize,
    filled: f64,
    min: f64,
    max: f64,
    seen_min: f64,
    seen_max: f64,
}

impl Periods {
    pub(crate) fn new(rate_hz: u64, baud: u32, warmup: usize) -> Periods {
        Periods {
            period: rate_hz as f64 / baud as f64,
            warmup,
            filled: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            seen_min: f64::INFINITY,
            seen_max: f64::NEG_INFINITY,
        }
    }

    /// Take one sample; the period's `(min, max)` when this sample closes it.
    pub(crate) fn push(&mut self, v: f64) -> Option<(f64, f64)> {
        self.seen_min = self.seen_min.min(v);
        self.seen_max = self.seen_max.max(v);
        if self.warmup > 0 {
            self.warmup -= 1;
            return None;
        }
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.filled += 1.0;
        if self.filled < self.period {
            return None;
        }
        self.filled -= self.period;
        let closed = (self.min, self.max);
        (self.min, self.max) = (f64::INFINITY, f64::NEG_INFINITY);
        Some(closed)
    }

    /// The minimum and maximum of every sample so far, the warm-up included: what a
    /// running scale stands on until its first period closes, so the output stays
    /// bounded from the first sample.
    pub(crate) fn seen(&self) -> (f64, f64) {
        (self.seen_min, self.seen_max)
    }
}

/// How the discriminator output is scaled to put the outer tones at ±1.
enum Scale {
    /// By the outer tones' frequency, from `tone_spacing_hz`.
    Fixed(f64),
    /// By the running peak: the exponential average over [`NORMALISER_SYMBOLS`] of each
    /// symbol period's largest magnitude after the low-pass's warm-up, seeded by the
    /// first such period's (and until it closes, by the largest magnitude so far).
    Running { periods: Periods, peak: Option<f64> },
}

impl Scale {
    fn of(&mut self, v: f64) -> f64 {
        match self {
            Scale::Fixed(s) => *s,
            Scale::Running { periods, peak } => {
                if let Some((_, m)) = periods.push(v.abs()) {
                    *peak = Some(match *peak {
                        None => m,
                        Some(p) => p + (m - p) / NORMALISER_SYMBOLS,
                    });
                }
                peak.unwrap_or(periods.seen().1)
            }
        }
    }
}

/// The demodulator over one run of contiguous input.
struct Demod {
    rate_hz: u64,
    prev: Option<Complex<f32>>,
    fir: StreamingFir,
    scale: Scale,
}

impl Demod {
    fn new(p: &FskDemodParams, rate_hz: u64) -> Result<Demod, StageError> {
        sps_check(FSK_DEMOD_KIND, rate_hz, p.symbol_rate_baud).map_err(StageError::Failed)?;
        let order = p.order as f64;
        let cutoff = DISCRIMINATOR_LOWPASS_FACTOR * p.symbol_rate_baud as f64 * order;
        let (fir, warmup) = decision_lowpass(cutoff, rate_hz, p.symbol_rate_baud);
        let scale = match p.tone_spacing_hz {
            Some(s) => Scale::Fixed(s as f64 * (order - 1.0) / 2.0),
            None => Scale::Running {
                periods: Periods::new(rate_hz, p.symbol_rate_baud, warmup),
                peak: None,
            },
        };
        Ok(Demod {
            rate_hz,
            prev: None,
            fir,
            scale,
        })
    }

    fn process(&mut self, x: &[Complex<f32>]) -> Vec<f32> {
        let gain = self.rate_hz as f64 / TAU;
        let freq: Vec<f32> = x
            .iter()
            .map(|&s| {
                // The first sample of a run has no predecessor: read it as 0 Hz.
                let prev = self.prev.replace(s).unwrap_or(s);
                // atan2(0, 0) is 0, so a zero sample on either side gives 0.
                ((s * prev.conj()).arg() as f64 * gain) as f32
            })
            .collect();
        self.fir
            .process(&freq)
            .into_iter()
            .map(|v| {
                let v = v as f64;
                (v / self.scale.of(v).max(MIN_SCALE)) as f32
            })
            .collect()
    }
}

/// Demodulates 2- or 4-level FSK into a 1-channel decision signal at the input rate:
/// the quadrature discriminator `arg(x[n] · conj(x[n-1])) × rate / 2π` in hertz,
/// low-passed at [`DISCRIMINATOR_LOWPASS_FACTOR`] × `symbol_rate_baud` × `order`, then
/// scaled so the outer tones sit at ±1: by `tone_spacing_hz × (order - 1) / 2` when
/// given, else by the running peak (see [`NORMALISER_SYMBOLS`]). An input under
/// [`crate::MIN_SAMPLES_PER_SYMBOL`] samples per symbol fails the stage naming the rate
/// to resample to. A seam ([`is_seam`]), a lag or a rate change restarts the filter and
/// the running peak.
pub struct FskDemodStage {
    spec: StageSpec,
}

impl FskDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: FSK_DEMOD_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(FskDemodParams),
            },
        }
    }
}

impl Default for FskDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for FskDemodStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: FskDemodParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        if p.symbol_rate_baud == 0 {
            return Err("symbol_rate_baud: at least 1".into());
        }
        if !matches!(p.order, 2 | 4) {
            return Err("order: 2 or 4".into());
        }
        if p.tone_spacing_hz == Some(0) {
            return Err("tone_spacing_hz: at least 1".into());
        }
        Ok(())
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
        self.check_params(&params).map_err(StageError::Failed)?;
        let p: FskDemodParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{FSK_DEMOD_KIND} params: {e}")))?;
        let [out] = <[StreamId; 1]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 1 output, got {}", o.len())))?;
        let mut demod: Option<Demod> = None;
        let mut prev_end: Option<u64> = None;
        run_map(FSK_DEMOD_KIND, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    demod = None;
                    prev_end = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(format!(
                    "{FSK_DEMOD_KIND} stage expects iq frames"
                )));
            };
            let rate = block.sample_rate_hz.0;
            let span = &frame.provenance.span;
            let seam = prev_end.is_some_and(|e| is_seam(e, span.start_ns.0, rate));
            prev_end = Some(span.end_ns.0);
            if seam || demod.as_ref().is_some_and(|d| d.rate_hz != rate) {
                demod = None;
            }
            let d = match &mut demod {
                Some(d) => d,
                None => demod.insert(Demod::new(&p, rate)?),
            };
            let y = d.process(&block.samples);
            if y.is_empty() {
                return Ok(Vec::new());
            }
            Ok(vec![Frame::new(
                out.clone(),
                frame.provenance.clone(),
                Payload::Audio {
                    sample_rate_hz: Hz(rate),
                    channels: 1,
                    samples: y.into(),
                },
            )])
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use num_complex::Complex;
    use serde_json::json;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::port::IqBlock;
    use zeta_core::stage::{Stage, StageContext};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, JobId, Provenance, SourceId, StreamKind, Timestamp};

    use super::FskDemodStage;
    use crate::frame_span;
    use crate::util::nco;

    const RATE: u64 = 38_400;
    const BAUD: u32 = 4_800;
    const T0: u64 = 1_000_000_000;

    fn prov(t0: u64, n: usize) -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: frame_span(Timestamp(t0), n, RATE),
        }
    }

    /// The stage's output samples over `iq` fed as one frame.
    async fn demod(params: serde_json::Value, iq: Vec<Complex<f32>>) -> Vec<f32> {
        let bus = Arc::new(StreamBus::new(64));
        let out = bus.register(StreamKind::Audio, prov(0, 0));
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx) = broadcast::channel(4);
        let n = iq.len();
        tx.send(Frame::new(
            zeta_core::types::StreamId::new(),
            prov(T0, n),
            Payload::Iq(IqBlock {
                t0: Timestamp(T0),
                sample_rate_hz: Hz(RATE),
                center_hz: Hz(0),
                samples: iq.into(),
            }),
        ))
        .unwrap();
        drop(tx);
        FskDemodStage::new()
            .run(
                params,
                vec![rx],
                vec![out],
                bus.clone(),
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
            .unwrap();
        let mut y = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = f.payload
            else {
                panic!("an audio frame")
            };
            assert_eq!((sample_rate_hz.0, channels), (RATE, 1));
            y.extend(samples.iter().copied());
        }
        y
    }

    #[tokio::test]
    async fn a_2fsk_tone_pair_maps_to_plus_and_minus_one() {
        let sps = (RATE / BAUD as u64) as usize;
        // 8 symbols of -2400 Hz, then 8 of +2400 Hz.
        let mut iq = nco(8 * sps, -2400.0, RATE as f64);
        let last = *iq.last().unwrap();
        iq.extend(
            nco(8 * sps, 2400.0, RATE as f64)
                .into_iter()
                .map(|s| s * last),
        );
        let y = demod(
            json!({"symbol_rate_baud": BAUD, "tone_spacing_hz": 4800}),
            iq,
        )
        .await;
        assert_eq!(y.len(), 16 * sps);
        // One symbol well inside each run, clear of the filter's delay and the transition.
        let mean = |k: usize| y[k * sps..(k + 1) * sps].iter().sum::<f32>() / sps as f32;
        let (lo, hi) = (mean(5), mean(13));
        assert!((lo + 1.0).abs() < 0.1, "low tone reads {lo}");
        assert!((hi - 1.0).abs() < 0.1, "high tone reads {hi}");
    }

    #[test]
    fn too_few_samples_per_symbol_is_refused_naming_the_rate() {
        let e = crate::sps_check("fsk_demod", 12_000, 4800).unwrap_err();
        assert!(
            e.contains(
                "12000 Hz is 2.5 samples per symbol at 4800 Bd; resample to at least 19200 Hz"
            ),
            "{e}"
        );
        assert!(e.starts_with("fsk_demod: "), "{e}");
        assert!(crate::sps_check("fsk_demod", 19_200, 4800).is_ok());
    }

    #[test]
    fn params_are_checked_naming_the_field() {
        let s = FskDemodStage::new();
        assert!(s.check_params(&json!({"symbol_rate_baud": 4800})).is_ok());
        let e = s
            .check_params(&json!({"symbol_rate_baud": 4800, "order": 3}))
            .unwrap_err();
        assert_eq!(e, "order: 2 or 4");
        assert!(s.check_params(&json!({"order": 2})).is_err());
        assert!(
            s.check_params(&json!({"symbol_rate_baud": 4800, "x": 1}))
                .is_err()
        );
        assert_eq!(
            s.check_params(&json!({"symbol_rate_baud": 0})).unwrap_err(),
            "symbol_rate_baud: at least 1"
        );
    }

    #[tokio::test]
    async fn a_seam_resets_the_normaliser() {
        // 20 symbols at ±2400 Hz, a 1 s gap, then 20 at ±600 Hz: after the seam the
        // running peak starts again from the quieter stream, so it reads about ±1 at once.
        let sps = (RATE / BAUD as u64) as usize;
        let tones = |dev: f64| {
            let mut iq = Vec::new();
            for k in 0..20 {
                let f = if k % 4 < 2 { dev } else { -dev };
                let last = iq.last().copied().unwrap_or(Complex::new(1.0, 0.0));
                iq.extend(nco(sps, f, RATE as f64).into_iter().map(|s| s * last));
            }
            iq
        };
        let bus = Arc::new(StreamBus::new(64));
        let out = bus.register(StreamKind::Audio, prov(0, 0));
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx) = broadcast::channel(4);
        for (t0, iq) in [(T0, tones(2400.0)), (T0 + 2_000_000_000, tones(600.0))] {
            let n = iq.len();
            tx.send(Frame::new(
                zeta_core::types::StreamId::new(),
                prov(t0, n),
                Payload::Iq(IqBlock {
                    t0: Timestamp(t0),
                    sample_rate_hz: Hz(RATE),
                    center_hz: Hz(0),
                    samples: iq.into(),
                }),
            ))
            .unwrap();
        }
        drop(tx);
        FskDemodStage::new()
            .run(
                json!({"symbol_rate_baud": BAUD}),
                vec![rx],
                vec![out],
                bus.clone(),
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
            .unwrap();
        let mut frames = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            let Payload::Audio { samples, .. } = f.payload else {
                panic!("an audio frame")
            };
            frames.push(samples.to_vec());
        }
        assert_eq!(frames.len(), 2);
        let peak = frames[1][4 * sps..]
            .iter()
            .fold(0f32, |m, v| m.max(v.abs()));
        assert!(
            (0.8..1.3).contains(&peak),
            "after the seam the peak reads {peak}"
        );
    }
}
