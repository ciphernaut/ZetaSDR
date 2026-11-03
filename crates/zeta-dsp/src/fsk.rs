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
use zeta_core::types::{Hz, Provenance, StreamId, StreamKind};

use crate::util::{StreamingFir, lowpass_taps};
use crate::{Input, frame_span, is_seam, run_map_ending, sps_check};

/// The stage kind, as graphs name it.
pub const FSK_DEMOD_KIND: &str = "fsk_demod";

/// The decision low-pass cutoff as a multiple of the symbol rate (times the order for
/// FSK): wide enough to keep a symbol's transition, narrow enough to drop the noise
/// above the keying.
pub const DISCRIMINATOR_LOWPASS_FACTOR: f64 = 0.6;

/// Time constant of a demodulator's running levels (the release of [`Levels`]), in
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
    /// The spacing between adjacent tones in hertz, which fixes the scale; `null` scales
    /// by the running half-range.
    #[serde(default)]
    pub tone_spacing_hz: Option<u64>,
}

/// The fraction of the way a symbol period's extreme beyond a running level moves it: a
/// fast but bounded attack, so one impulse lifts a level by a quarter of its excess and
/// the slow release ([`NORMALISER_SYMBOLS`]) brings it back.
const ATTACK: f64 = 0.25;

/// The warm-up of the running levels, in group delays of the decision low-pass: its own
/// history (two delays) filling from zero, which also spreads an upstream filter's
/// start-up transient (the resampler's) into it.
pub(crate) const WARMUP_DELAYS: usize = 2;

/// Symbol periods whose extremes seed the running levels: enough that a random stream
/// has shown both its lowest and highest level.
const SEED_PERIODS: usize = 8;

/// Time constant of the running centre, in symbol periods: longer than the levels', as
/// the centre carries the per-period midpoints' data-dependent jitter straight into the
/// output's offset (measured on 4-FSK at 20 dB and +500 Hz: 2 symbol errors in 1024 at
/// 64, none from 128 to 512).
const CENTRE_SYMBOLS: f64 = 256.0;

/// The decision low-pass at `cutoff_hz` (capped at [`MAX_CUTOFF_FRACTION`] of the rate)
/// spanning [`LOWPASS_SPAN_SYMBOLS`] symbols of `baud` at `rate_hz`, and its group delay
/// in samples (what a flush pushes through it so the last input comes out).
pub(crate) fn decision_lowpass(cutoff_hz: f64, rate_hz: u64, baud: u32) -> (StreamingFir, usize) {
    let rate = rate_hz as f64;
    let taps = (LOWPASS_SPAN_SYMBOLS * rate / baud as f64).ceil() as usize | 1;
    let fir = StreamingFir::new(lowpass_taps(
        cutoff_hz.min(MAX_CUTOFF_FRACTION * rate),
        rate,
        taps,
    ));
    (fir, taps / 2)
}

/// The running low and high levels of a decision signal, over symbol periods
/// (fractional lengths carried). Each period's minimum moves `lo` and its maximum `hi`:
/// by [`ATTACK`] of the way when it lies beyond the level, else by `1/NORMALISER_SYMBOLS`
/// (the release), every period, so a level always returns to what the signal shows. The
/// extremes of the first [`SEED_PERIODS`] periods seed both; until then the extremes so
/// far stand in, so the scaled output is bounded from the first sample. The first
/// `warmup` samples (the low-pass filling from zero history, and the start-up transient
/// of any filter upstream it spreads) take no part and scale to 0. Alongside, the
/// centre: the plain exponential average over [`CENTRE_SYMBOLS`] of each period's
/// midpoint `(min + max) / 2`, seeded with the mean of the seed periods. It moves the same
/// amount whichever way the signal goes, so a run of one tone does not shift it the way
/// the asymmetric attack would shift `(lo + hi) / 2`.
pub(crate) struct Levels {
    warmup: usize,
    period: f64,
    filled: f64,
    min: f64,
    max: f64,
    seen_min: f64,
    seen_max: f64,
    seen_sum: f64,
    seen_n: usize,
    closed: usize,
    levels: Option<(f64, f64)>,
    centre: Option<f64>,
}

impl Levels {
    pub(crate) fn new(rate_hz: u64, baud: u32, warmup: usize) -> Levels {
        Levels {
            warmup,
            period: rate_hz as f64 / baud as f64,
            filled: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            seen_min: f64::INFINITY,
            seen_max: f64::NEG_INFINITY,
            seen_sum: 0.0,
            seen_n: 0,
            closed: 0,
            levels: None,
            centre: None,
        }
    }

    /// Take one sample; the `(lo, hi)` to scale it by, `None` while warming up.
    pub(crate) fn push(&mut self, v: f64) -> Option<(f64, f64)> {
        if self.warmup > 0 {
            self.warmup -= 1;
            return None;
        }
        self.seen_min = self.seen_min.min(v);
        self.seen_max = self.seen_max.max(v);
        self.seen_sum += v;
        self.seen_n += 1;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.filled += 1.0;
        if self.filled >= self.period {
            self.filled -= self.period;
            let (pmin, pmax) = (self.min, self.max);
            (self.min, self.max) = (f64::INFINITY, f64::NEG_INFINITY);
            self.closed += 1;
            let step = |level: &mut f64, x: f64, beyond: bool| {
                let rate = if beyond {
                    ATTACK
                } else {
                    1.0 / NORMALISER_SYMBOLS
                };
                *level += (x - *level) * rate;
            };
            match (&mut self.levels, &mut self.centre) {
                (Some((lo, hi)), Some(centre)) => {
                    step(lo, pmin, pmin < *lo);
                    step(hi, pmax, pmax > *hi);
                    *centre += ((pmin + pmax) / 2.0 - *centre) / CENTRE_SYMBOLS;
                }
                _ if self.closed >= SEED_PERIODS => {
                    self.levels = Some((self.seen_min, self.seen_max));
                    self.centre = Some(self.seen_mean());
                }
                _ => {}
            }
        }
        Some(self.levels.unwrap_or((self.seen_min, self.seen_max)))
    }

    /// The running centre after the last [`Levels::push`].
    pub(crate) fn centre(&self) -> f64 {
        self.centre.unwrap_or_else(|| self.seen_mean())
    }

    /// The mean of every sample so far: the centre's seed, which a start-up spike (the
    /// discriminator over an upstream filter's zero history) moves by its area only, not
    /// by half its height as the extremes' midpoint would.
    fn seen_mean(&self) -> f64 {
        self.seen_sum / self.seen_n.max(1) as f64
    }
}

/// A demodulator producing a 1-channel decision signal over one run of contiguous input.
pub(crate) trait Decision: Send {
    fn rate_hz(&self) -> u64;
    /// The decision samples of `x`, one per input sample.
    fn process(&mut self, x: &[Complex<f32>]) -> Vec<f32>;
    /// The low-pass's group delay pushed through it at the run's end, so the last input
    /// samples come out.
    fn flush(&mut self) -> Vec<f32>;
}

/// The loop both decision demodulators run: IQ frames in, audio frames out at the input
/// rate. A seam ([`is_seam`]), a lag, a rate change and the input's end flush the run
/// (its tail published right after its last frame) and the next frame starts a new one
/// from `make`.
pub(crate) async fn run_decision<D, F>(
    kind: &'static str,
    inputs: Vec<broadcast::Receiver<Frame>>,
    out: StreamId,
    bus: Arc<StreamBus>,
    cancel: CancellationToken,
    mut make: F,
) -> Result<(), StageError>
where
    D: Decision,
    F: FnMut(u64) -> Result<D, StageError> + Send,
{
    let audio = |prov: Provenance, rate: u64, y: Vec<f32>| {
        Frame::new(
            out.clone(),
            prov,
            Payload::Audio {
                sample_rate_hz: Hz(rate),
                channels: 1,
                samples: y.into(),
            },
        )
    };
    // The run's demodulator and its last frame's provenance.
    let mut run: Option<(D, Provenance)> = None;
    let tail = |run: &mut Option<(D, Provenance)>| {
        let (mut d, mut prov) = run.take()?;
        let y = d.flush();
        if y.is_empty() {
            return None;
        }
        prov.span = frame_span(prov.span.end_ns, y.len(), d.rate_hz());
        Some(audio(prov, d.rate_hz(), y))
    };
    run_map_ending(kind, inputs, bus, cancel, move |input| {
        let frame = match input {
            Some(Input::Frame(f)) => f,
            Some(Input::Lagged { .. }) | None => return Ok(tail(&mut run).into_iter().collect()),
        };
        let Payload::Iq(block) = &frame.payload else {
            return Err(StageError::Failed(format!(
                "{kind} stage expects iq frames"
            )));
        };
        let rate = block.sample_rate_hz.0;
        let span = &frame.provenance.span;
        let mut published = Vec::new();
        if run.as_ref().is_some_and(|(d, prev)| {
            d.rate_hz() != rate || is_seam(prev.span.end_ns.0, span.start_ns.0, rate)
        }) {
            published.extend(tail(&mut run));
        }
        let (d, prev) = match &mut run {
            Some(r) => r,
            None => run.insert((make(rate)?, frame.provenance.clone())),
        };
        *prev = frame.provenance.clone();
        let y = d.process(&block.samples);
        if !y.is_empty() {
            published.push(audio(frame.provenance.clone(), rate, y));
        }
        Ok(published)
    })
    .await
}

/// The FSK demodulator over one run of contiguous input.
struct Demod {
    rate_hz: u64,
    prev: Option<Complex<f32>>,
    last_freq: f32,
    fir: StreamingFir,
    delay: usize,
    levels: Levels,
    /// The outer tones' frequency from `tone_spacing_hz`, else `None` for the running
    /// half-range.
    half_range: Option<f64>,
}

impl Demod {
    fn new(p: &FskDemodParams, rate_hz: u64) -> Result<Demod, StageError> {
        sps_check(FSK_DEMOD_KIND, rate_hz, p.symbol_rate_baud).map_err(StageError::Failed)?;
        let order = p.order as f64;
        let cutoff = DISCRIMINATOR_LOWPASS_FACTOR * p.symbol_rate_baud as f64 * order;
        let (fir, delay) = decision_lowpass(cutoff, rate_hz, p.symbol_rate_baud);
        Ok(Demod {
            rate_hz,
            prev: None,
            last_freq: 0.0,
            fir,
            delay,
            levels: Levels::new(rate_hz, p.symbol_rate_baud, WARMUP_DELAYS * delay),
            half_range: p.tone_spacing_hz.map(|s| s as f64 * (order - 1.0) / 2.0),
        })
    }

    /// Scale low-passed frequencies so the outer tones sit at ±1 about the running
    /// centre.
    fn scale(&mut self, filtered: Vec<f32>) -> Vec<f32> {
        filtered
            .into_iter()
            .map(|v| {
                let v = v as f64;
                let Some((lo, hi)) = self.levels.push(v) else {
                    return 0.0;
                };
                let half = self.half_range.unwrap_or((hi - lo) / 2.0);
                ((v - self.levels.centre()) / half.max(MIN_SCALE)) as f32
            })
            .collect()
    }
}

impl Decision for Demod {
    fn rate_hz(&self) -> u64 {
        self.rate_hz
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
        if let Some(&f) = freq.last() {
            self.last_freq = f;
        }
        let filtered = self.fir.process(&freq);
        self.scale(filtered)
    }

    /// The last frequency held for the group delay: 0 Hz would be a tone of its own.
    fn flush(&mut self) -> Vec<f32> {
        let hold = vec![self.last_freq; self.delay];
        let filtered = self.fir.process(&hold);
        self.scale(filtered)
    }
}

/// Demodulates 2- or 4-level FSK into a 1-channel decision signal at the input rate:
/// the quadrature discriminator `arg(x[n] · conj(x[n-1])) × rate / 2π` in hertz,
/// low-passed at [`DISCRIMINATOR_LOWPASS_FACTOR`] × `symbol_rate_baud` × `order`, then
/// centred on the running centre (so a carrier offset reads 0) and scaled so
/// the outer tones sit at ±1: by `tone_spacing_hz × (order - 1) / 2` when given, else by
/// the running half-range (see [`Levels`]). An input under
/// [`crate::MIN_SAMPLES_PER_SYMBOL`] samples per symbol fails the stage naming the rate
/// to resample to. A seam ([`is_seam`]), a lag, a rate change and the input's end flush
/// the low-pass (the last frequency held for its group delay) and restart the filter and
/// the levels.
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
        run_decision(FSK_DEMOD_KIND, inputs, out, bus, cancel, move |rate| {
            Demod::new(&p, rate)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::sync::Arc;

    use num_complex::Complex;
    use serde_json::{Value, json};
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::port::IqBlock;
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, JobId, Provenance, SourceId, StreamKind, Timestamp};

    use super::FskDemodStage;
    use crate::frame_span;

    const RATE: u64 = 38_400;
    const BAUD: u32 = 4_800;
    const SPS: usize = (RATE / BAUD as u64) as usize;
    /// The decision low-pass's group delay at [`RATE`]: 17 taps.
    const DELAY: usize = 8;
    const T0: u64 = 1_000_000_000;
    /// Symbols per run of one tone in the test signals.
    const RUN: usize = 4;

    fn prov(t0: u64, n: usize, rate: u64) -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: frame_span(Timestamp(t0), n, rate),
        }
    }

    /// Phase-continuous 2-FSK at [`RATE`]: `symbols` symbols alternating in runs of
    /// [`RUN`] between `offset - dev` and `offset + dev`, low tone first.
    fn keyed(symbols: usize, dev: f64, offset: f64) -> Vec<Complex<f32>> {
        let mut phase = 0.0f64;
        (0..symbols * SPS)
            .map(|i| {
                let high = (i / SPS / RUN) % 2 == 1;
                phase += TAU * (offset + if high { dev } else { -dev }) / RATE as f64;
                let (s, c) = phase.sin_cos();
                Complex::new(c as f32, s as f32)
            })
            .collect()
    }

    /// The stage's output frames' samples over `input` (`(t0, iq, rate)` frames).
    async fn run_frames(
        params: Value,
        input: Vec<(u64, Vec<Complex<f32>>, u64)>,
    ) -> Result<Vec<Vec<f32>>, StageError> {
        let bus = Arc::new(StreamBus::new(64));
        let out = bus.register(StreamKind::Audio, prov(0, 0, RATE));
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx) = broadcast::channel(input.len().max(1));
        for (t0, iq, rate) in input {
            tx.send(Frame::new(
                zeta_core::types::StreamId::new(),
                prov(t0, iq.len(), rate),
                Payload::Iq(IqBlock {
                    t0: Timestamp(t0),
                    sample_rate_hz: Hz(rate),
                    center_hz: Hz(0),
                    samples: iq.into(),
                }),
            ))
            .unwrap();
        }
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
            .await?;
        let mut frames = Vec::new();
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
            frames.push(samples.to_vec());
        }
        Ok(frames)
    }

    /// The mean output over input symbol `k` (the low-pass's delay allowed for).
    fn symbol_mean(y: &[f32], k: usize) -> f32 {
        let w = &y[k * SPS + DELAY..(k + 1) * SPS + DELAY];
        w.iter().sum::<f32>() / SPS as f32
    }

    /// The means over the middle symbol of the last low and the last high run of a
    /// 128-symbol [`keyed`] signal.
    async fn last_run_means(params: Value, offset: f64) -> (f32, f32) {
        const SYMBOLS: usize = 128;
        let iq = keyed(SYMBOLS, 2400.0, offset);
        let n = iq.len();
        let y: Vec<f32> = run_frames(params, vec![(T0, iq, RATE)])
            .await
            .unwrap()
            .concat();
        // The flush lets the last input samples out: one output per input, plus the delay.
        assert_eq!(y.len(), n + DELAY);
        let (low, high) = (SYMBOLS - 2 * RUN, SYMBOLS - RUN);
        (
            symbol_mean(&y, low + RUN / 2),
            symbol_mean(&y, high + RUN / 2),
        )
    }

    #[tokio::test]
    async fn a_2fsk_tone_pair_maps_to_plus_and_minus_one() {
        let (lo, hi) = last_run_means(
            json!({"symbol_rate_baud": BAUD, "tone_spacing_hz": 4800}),
            0.0,
        )
        .await;
        assert!((lo + 1.0).abs() < 0.1, "low tone reads {lo}");
        assert!((hi - 1.0).abs() < 0.1, "high tone reads {hi}");
    }

    #[tokio::test]
    async fn a_2fsk_pair_offset_by_500_hz_still_maps_to_plus_and_minus_one() {
        for params in [
            json!({"symbol_rate_baud": BAUD}),
            json!({"symbol_rate_baud": BAUD, "tone_spacing_hz": 4800}),
        ] {
            let (lo, hi) = last_run_means(params.clone(), 500.0).await;
            assert!((lo + 1.0).abs() < 0.1, "{params}: low tone reads {lo}");
            assert!((hi - 1.0).abs() < 0.1, "{params}: high tone reads {hi}");
        }
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

    #[tokio::test]
    async fn the_stage_refuses_an_input_under_4_samples_per_symbol() {
        let e = run_frames(
            json!({"symbol_rate_baud": BAUD}),
            vec![(T0, vec![Complex::new(1.0, 0.0); 100], 12_000)],
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string().contains(
                "fsk_demod: 12000 Hz is 2.5 samples per symbol at 4800 Bd; resample to at least 19200 Hz"
            ),
            "{e}"
        );
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
    async fn a_seam_resets_the_levels_and_flushes_the_run() {
        // 32 symbols at ±2400 Hz, a 1 s gap, then 32 at ±600 Hz: after the seam the levels
        // start again from the quieter stream, so it reads about ±1; carried over they
        // would read about ±1/4.
        const SYMBOLS: usize = 32;
        let (a, b) = (keyed(SYMBOLS, 2400.0, 0.0), keyed(SYMBOLS, 600.0, 0.0));
        let n = a.len();
        let frames = run_frames(
            json!({"symbol_rate_baud": BAUD}),
            vec![(T0, a, RATE), (T0 + 2_000_000_000, b, RATE)],
        )
        .await
        .unwrap();
        // The first run, its tail at the seam, the second run, its tail at the end.
        let lens: Vec<usize> = frames.iter().map(Vec::len).collect();
        assert_eq!(lens, [n, DELAY, n, DELAY]);
        let peak = frames[2][SYMBOLS / 2 * SPS..]
            .iter()
            .fold(0f32, |m, v| m.max(v.abs()));
        assert!(
            (0.8..1.3).contains(&peak),
            "after the seam the peak reads {peak}"
        );
    }
}
