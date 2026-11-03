//! PSK demodulation: carrier-locked I/Q at 4 samples per symbol out of BPSK, QPSK or
//! 8PSK.
//!
//! A coarse frequency offset is read once per stream from the line `x^order` makes
//! (the modulation removed), a root-raised-cosine matched filter shapes the pulse, the
//! filtered signal is decimated to [`OUT_SAMPLES_PER_SYMBOL`] and a Costas loop of the
//! order takes out the residual frequency and the phase, so `clock_recover` strobes the
//! symbols and `slicer` quantises them by angle (the first point at 0). The phase
//! ambiguity of the order (2, 4 or 8 equally good locks) is not resolved here:
//! `slicer`'s `differential` is the answer for a stream that needs it.

use std::f64::consts::{FRAC_1_SQRT_2, PI, TAU};
use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use rustfft::FftPlanner;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::util::StreamingComplexFir;
use crate::{Input, MIN_SAMPLES_PER_SYMBOL, is_seam, run_map_ending};

/// The stage kind, as graphs name it.
pub const PSK_DEMOD_KIND: &str = "psk_demod";

/// Samples per symbol of the output.
pub const OUT_SAMPLES_PER_SYMBOL: u64 = 4;

/// Default roll-off of the matched filter.
pub const ROLLOFF_DEFAULT: f64 = 0.35;

/// Default Costas loop bandwidth, as a fraction of the symbol rate.
pub const LOOP_BW_DEFAULT: f64 = 0.02;

/// Length of the matched filter, in symbol periods.
pub const RRC_SPAN_SYMBOLS: usize = 8;

/// Samples of a stream's start the coarse offset is read from.
pub const COARSE_FFT: usize = 4096;

/// Least ratio of the peak to the median of the `|X|²` spectrum of `x^order` that
/// [`coarse_offset`] trusts. White noise over [`COARSE_FFT`] bins peaks about 12 times
/// over its median (measured 11 to 19; the chance of 50 is about 4096 × 2^-50); the
/// line of a 1000-sample BPSK burst at 20 dB stood 28 000 times over it and QPSK over
/// a whole block 7 000.
pub const COARSE_MIN_PEAK_TO_MEDIAN: f64 = 50.0;

/// Further [`COARSE_FFT`] blocks of a run a coarse estimate under
/// [`COARSE_MIN_PEAK_TO_MEDIAN`] defers to before the offset falls back to 0 Hz (the
/// residual left to the Costas loop).
pub const MAX_COARSE_DEFERRALS: usize = 4;

const ORDER_DEFAULT: u8 = 2;

/// Damping of the Costas loop.
const DAMPING: f64 = FRAC_1_SQRT_2;

/// Time constant of the running amplitude the loop's error is weighted by, in output
/// samples.
const AMPLITUDE_SAMPLES: f64 = 64.0 * OUT_SAMPLES_PER_SYMBOL as f64;

/// Largest weight of one sample's phase error: a sample well above the running
/// amplitude (a noise spike) counts at most this many times an average one.
const MAX_WEIGHT: f64 = 2.0;

/// Floor of the running amplitude, so silence does not divide by zero.
const MIN_AMPLITUDE: f64 = 1e-12;

/// Distance from `t = ±1/(4 rolloff)` (in symbols) inside which the RRC takes its limit.
const RRC_SINGULAR: f64 = 1e-9;

const NS_PER_S: i128 = 1_000_000_000;

fn default_order() -> u8 {
    ORDER_DEFAULT
}

fn default_rolloff() -> f64 {
    ROLLOFF_DEFAULT
}

fn default_loop_bw() -> f64 {
    LOOP_BW_DEFAULT
}

/// `psk_demod` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PskDemodParams {
    /// The symbol rate in baud.
    pub symbol_rate_baud: u32,
    /// The number of phases, 2, 4 or 8.
    #[serde(default = "default_order")]
    pub order: u8,
    /// The matched filter's roll-off, `0 < r <= 1`.
    #[serde(default = "default_rolloff")]
    pub rolloff: f64,
    /// The Costas loop bandwidth as a fraction of the symbol rate, `0 < bw <= 0.5`.
    #[serde(default = "default_loop_bw")]
    pub loop_bw: f64,
}

/// The input decimation to [`OUT_SAMPLES_PER_SYMBOL`] at `rate_hz`, or the refusal
/// naming the smallest rate at or above `rate_hz` that is a whole multiple of
/// `OUT_SAMPLES_PER_SYMBOL × symbol_rate_baud` and at least [`MIN_SAMPLES_PER_SYMBOL`]
/// per symbol.
fn decimation(rate_hz: u64, symbol_rate_baud: u32) -> Result<usize, String> {
    let baud = symbol_rate_baud as u64;
    let grid = OUT_SAMPLES_PER_SYMBOL * baud;
    let least = MIN_SAMPLES_PER_SYMBOL * baud;
    if rate_hz >= least && rate_hz.is_multiple_of(grid) {
        return Ok((rate_hz / grid) as usize);
    }
    let need = rate_hz.max(least).div_ceil(grid) * grid;
    let sps = rate_hz as f64 / baud as f64;
    Err(format!(
        "{PSK_DEMOD_KIND}: {rate_hz} Hz is {sps:.2} samples per symbol at {symbol_rate_baud} Bd, \
         not a whole multiple of {OUT_SAMPLES_PER_SYMBOL}; resample to {need} Hz"
    ))
}

/// Root-raised-cosine taps of `rolloff` spanning [`RRC_SPAN_SYMBOLS`] at `sps` input
/// samples per symbol (an odd count), normalised to unity DC gain.
fn rrc_taps(rolloff: f64, sps: usize) -> Vec<f32> {
    let taps = RRC_SPAN_SYMBOLS * sps + 1;
    let mid = (taps / 2) as f64;
    let b = rolloff;
    let h: Vec<f64> = (0..taps)
        .map(|i| {
            let t = (i as f64 - mid) / sps as f64;
            if t == 0.0 {
                1.0 - b + 4.0 * b / PI
            } else if (t.abs() - 1.0 / (4.0 * b)).abs() < RRC_SINGULAR {
                let a = PI / (4.0 * b);
                b / 2f64.sqrt() * ((1.0 + 2.0 / PI) * a.sin() + (1.0 - 2.0 / PI) * a.cos())
            } else {
                ((PI * t * (1.0 - b)).sin() + 4.0 * b * t * (PI * t * (1.0 + b)).cos())
                    / (PI * t * (1.0 - (4.0 * b * t).powi(2)))
            }
        })
        .collect();
    let sum: f64 = h.iter().sum();
    h.iter().map(|v| (v / sum) as f32).collect()
}

/// A coarse offset estimate: the offset in radians per sample and how far its line
/// stands over the spectrum's median (power ratio).
struct Coarse {
    offset: f64,
    peak_to_median: f64,
}

/// The carrier offset of `x`: the peak of the `|X|²` spectrum of `x^order` over its
/// first [`COARSE_FFT`] samples (zero-padded when shorter), refined by a parabola
/// through the peak and its neighbours, divided by `order`, with the peak's ratio to
/// the spectrum's median. It is unambiguous only within ±rate / (2 × order).
fn coarse_offset(x: &[Complex<f32>], order: u32) -> Coarse {
    let mut buf: Vec<Complex<f32>> = x.iter().take(COARSE_FFT).map(|s| s.powu(order)).collect();
    buf.resize(COARSE_FFT, Complex::new(0.0, 0.0));
    FftPlanner::<f32>::new()
        .plan_fft_forward(COARSE_FFT)
        .process(&mut buf);
    let power: Vec<f64> = buf.iter().map(|b| b.norm_sqr() as f64).collect();
    let pw = |k: usize| power[k % COARSE_FFT];
    let peak = (0..COARSE_FFT)
        .max_by(|&a, &b| pw(a).total_cmp(&pw(b)))
        .unwrap_or(0);
    let mut sorted = power.clone();
    let (_, median, _) = sorted.select_nth_unstable_by(COARSE_FFT / 2, f64::total_cmp);
    let median = *median;
    let (l, c, r) = (pw(peak + COARSE_FFT - 1), pw(peak), pw(peak + 1));
    let denom = l - 2.0 * c + r;
    let delta = if denom.abs() > 0.0 {
        (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let bin = if peak > COARSE_FFT / 2 {
        peak as f64 - COARSE_FFT as f64
    } else {
        peak as f64
    };
    Coarse {
        offset: TAU * (bin + delta) / COARSE_FFT as f64 / order as f64,
        peak_to_median: if median > 0.0 {
            c / median
        } else if c > 0.0 {
            f64::INFINITY
        } else {
            0.0
        },
    }
}

/// A Costas loop of order `m` with an `m`-th power phase detector, on one sample per
/// call: the error `sin(m φ) / m` (φ the sample's angle after the loop's correction;
/// about φ near a lock, whichever of the `m` points the sample is near) weighted by
/// the sample's magnitude over the running amplitude (at most [`MAX_WEIGHT`]), so the
/// samples between symbols, small or not, count by how much they carry.
struct Costas {
    m: f64,
    kp: f64,
    ki: f64,
    phase: f64,
    freq: f64,
    amp: Option<f64>,
}

impl Costas {
    fn new(order: u8, loop_bw: f64) -> Costas {
        // Second-order loop gains for a detector of unit gain, updated per output sample.
        let bw = loop_bw / OUT_SAMPLES_PER_SYMBOL as f64;
        let theta = bw / (DAMPING + 1.0 / (4.0 * DAMPING));
        let d = 1.0 + 2.0 * DAMPING * theta + theta * theta;
        Costas {
            m: order as f64,
            kp: 4.0 * DAMPING * theta / d,
            ki: 4.0 * theta * theta / d,
            phase: 0.0,
            freq: 0.0,
            amp: None,
        }
    }

    fn step(&mut self, y: Complex<f32>) -> Complex<f64> {
        let y = Complex::new(y.re as f64, y.im as f64);
        let z = y * Complex::from_polar(1.0, -self.phase);
        let mag = z.norm();
        let amp = self.amp.get_or_insert(mag);
        *amp += (mag - *amp) / AMPLITUDE_SAMPLES;
        let weight = (mag / amp.max(MIN_AMPLITUDE)).min(MAX_WEIGHT);
        let err = (self.m * z.arg()).sin() / self.m * weight;
        self.freq += self.ki * err;
        self.phase = (self.phase + self.freq + self.kp * err).rem_euclid(TAU);
        z
    }
}

/// The demodulator over one run of contiguous input.
struct Demod {
    order: u8,
    rate_hz: u64,
    decim: usize,
    /// Time of the run's first input sample.
    t0_ns: u64,
    /// The matched filter's group delay, in input samples.
    delay: usize,
    /// Input held until a confident estimate (or the fallback) gives the offset.
    pending: Vec<Complex<f32>>,
    /// [`COARSE_FFT`] blocks of `pending` estimated so far.
    tried: usize,
    /// The coarse offset in radians per sample, once decided.
    offset: Option<f64>,
    /// The decision's `psk` note for the job context, until the stage takes it.
    note: Option<serde_json::Value>,
    mix_phase: f64,
    fir: StreamingComplexFir,
    /// Filtered samples to skip before the next one kept.
    skip: usize,
    costas: Costas,
    /// Output samples (I/Q pairs) so far.
    produced: u64,
}

impl Demod {
    fn new(p: &PskDemodParams, rate_hz: u64, t0_ns: u64) -> Result<Demod, StageError> {
        let decim = decimation(rate_hz, p.symbol_rate_baud).map_err(StageError::Failed)?;
        let sps = decim * OUT_SAMPLES_PER_SYMBOL as usize;
        let taps = rrc_taps(p.rolloff, sps);
        Ok(Demod {
            order: p.order,
            rate_hz,
            decim,
            t0_ns,
            delay: taps.len() / 2,
            pending: Vec::new(),
            tried: 0,
            offset: None,
            note: None,
            mix_phase: 0.0,
            fir: StreamingComplexFir::new(taps),
            skip: 0,
            costas: Costas::new(p.order, p.loop_bw),
            produced: 0,
        })
    }

    fn out_rate_hz(&self) -> u64 {
        self.rate_hz / self.decim as u64
    }

    /// The span of the next `n` output samples: output `j` stands for input sample
    /// `j × decim` less the matched filter's delay.
    fn next_span(&self, n: usize) -> Span {
        let at = |j: u64| {
            let input = j as i128 * self.decim as i128 - self.delay as i128;
            let ns = self.t0_ns as i128 + input * NS_PER_S / self.rate_hz as i128;
            Timestamp(ns.max(0) as u64)
        };
        Span {
            start_ns: at(self.produced),
            end_ns: at(self.produced + n as u64),
        }
    }

    /// The offset removed, the matched filter, the decimation and the loop over
    /// `x`; the output interleaved I/Q.
    fn filter(&mut self, x: &[Complex<f32>], offset: f64) -> Vec<f32> {
        let mixed: Vec<Complex<f32>> = x
            .iter()
            .map(|&s| {
                let lo = Complex::from_polar(1.0, -self.mix_phase);
                self.mix_phase = (self.mix_phase + offset).rem_euclid(TAU);
                s * Complex::new(lo.re as f32, lo.im as f32)
            })
            .collect();
        let mut out = Vec::with_capacity(2 * mixed.len().div_ceil(self.decim));
        for y in self.fir.process(&mixed) {
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            self.skip = self.decim - 1;
            let z = self.costas.step(y);
            out.extend([z.re as f32, z.im as f32]);
        }
        out
    }

    /// The output of `x`, interleaved I/Q; empty while the coarse offset waits for a
    /// confident [`COARSE_FFT`] block.
    fn process(&mut self, x: &[Complex<f32>]) -> Vec<f32> {
        if let Some(offset) = self.offset {
            return self.filter(x, offset);
        }
        self.pending.extend_from_slice(x);
        while self.offset.is_none() && self.pending.len() >= (self.tried + 1) * COARSE_FFT {
            let block = &self.pending[self.tried * COARSE_FFT..(self.tried + 1) * COARSE_FFT];
            let est = coarse_offset(block, self.order as u32);
            self.tried += 1;
            if est.peak_to_median >= COARSE_MIN_PEAK_TO_MEDIAN {
                self.decide(est.offset, true, self.tried - 1);
            } else if self.tried > MAX_COARSE_DEFERRALS {
                self.decide(0.0, false, MAX_COARSE_DEFERRALS);
            }
        }
        if self.offset.is_none() {
            return Vec::new();
        }
        self.release()
    }

    /// Fix the coarse offset and note it.
    fn decide(&mut self, offset: f64, confident: bool, deferrals: usize) {
        self.offset = Some(offset);
        self.note = Some(serde_json::json!({"psk": {
            "coarse_hz": offset * self.rate_hz as f64 / TAU,
            "coarse_confident": confident,
            "coarse_deferrals": deferrals,
        }}));
    }

    /// Filter what is pending with the decided offset.
    fn release(&mut self) -> Vec<f32> {
        let offset = self.offset.unwrap_or(0.0);
        let pending = std::mem::take(&mut self.pending);
        self.filter(&pending, offset)
    }

    /// The run's end: anything still pending (its offset read from the samples not yet
    /// estimated, else 0 Hz), then the matched filter's group delay of zeros, so the
    /// last input comes out.
    fn flush(&mut self) -> Vec<f32> {
        if self.pending.is_empty() && self.offset.is_none() {
            return Vec::new();
        }
        if self.offset.is_none() {
            let rest = &self.pending[self.tried * COARSE_FFT..];
            let est = (!rest.is_empty()).then(|| coarse_offset(rest, self.order as u32));
            match est {
                Some(e) if e.peak_to_median >= COARSE_MIN_PEAK_TO_MEDIAN => {
                    self.decide(e.offset, true, self.tried)
                }
                _ => self.decide(0.0, false, self.tried),
            }
        }
        let mut out = self.release();
        let zeros = vec![Complex::new(0.0, 0.0); self.delay];
        out.extend(self.filter(&zeros, 0.0));
        out
    }
}

/// The audio frame of `y` (interleaved I/Q, `d`'s next output samples) on `out`, with
/// `prov` and the span of those samples; `None` when `y` is empty. A coarse offset
/// decided since the last call is noted on `ctx` first.
fn publish(
    ctx: &StageContext,
    out: &StreamId,
    d: &mut Demod,
    prov: &Provenance,
    y: Vec<f32>,
) -> Option<Frame> {
    if let Some(note) = d.note.take() {
        ctx.note(note);
    }
    if y.is_empty() {
        return None;
    }
    let n = y.len() / 2;
    let mut prov = prov.clone();
    prov.span = d.next_span(n);
    d.produced += n as u64;
    Some(Frame::new(
        out.clone(),
        prov,
        Payload::Audio {
            sample_rate_hz: Hz(d.out_rate_hz()),
            channels: 2,
            samples: y.into(),
        },
    ))
}

/// Demodulates BPSK, QPSK or 8PSK (`order` 2, 4 or 8) into 2-channel interleaved I/Q
/// audio at [`OUT_SAMPLES_PER_SYMBOL`] × `symbol_rate_baud`: the coarse offset from
/// the peak of the FFT of `x^order` over a stream's first [`COARSE_FFT`] samples
/// (divided by `order`) removed, a root-raised-cosine matched filter of `rolloff` over
/// [`RRC_SPAN_SYMBOLS`], decimation, then a Costas loop of `order` whose gains come from
/// `loop_bw`, locked with the constellation's first point at angle 0 (one of `order`
/// rotations). The input rate must be a whole multiple of `OUT_SAMPLES_PER_SYMBOL ×
/// symbol_rate_baud`, else the stage fails naming the rate to give `resample`.
///
/// The coarse estimate is unambiguous only within ±rate / (2 × `order`): at the least
/// rate, 4 × baud, that is ±baud for BPSK, ±baud/2 for QPSK and ±baud/4 for 8PSK; at
/// 8 × baud ±2 × baud, ±baud and ±baud/2. A larger offset aliases, so a chain must give
/// `resample` a rate wide enough for the offset it expects, not merely the smallest
/// rate on the grid. An estimate whose line stands under
/// [`COARSE_MIN_PEAK_TO_MEDIAN`] over the spectrum's median (noise, no carrier yet)
/// defers to the run's next [`COARSE_FFT`] block, at most [`MAX_COARSE_DEFERRALS`]
/// times; at that bound, or at a flush with no confident line, the offset falls back
/// to 0 Hz and the Costas loop is left the residual. The decision is noted in the job
/// context under `psk`: `coarse_hz`, `coarse_confident` and `coarse_deferrals`. A seam
/// ([`is_seam`]), a lag, a rate change and the input's end flush the matched filter
/// (its group delay of zeros pushed through) and the next frame starts again: a new
/// offset estimate, filter, decimation phase and loop.
pub struct PskDemodStage {
    spec: StageSpec,
}

impl PskDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: PSK_DEMOD_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(PskDemodParams),
            },
        }
    }
}

impl Default for PskDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for PskDemodStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: PskDemodParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        if p.symbol_rate_baud == 0 {
            return Err("symbol_rate_baud: at least 1".into());
        }
        if !matches!(p.order, 2 | 4 | 8) {
            return Err("order: 2, 4 or 8".into());
        }
        if !(p.rolloff > 0.0 && p.rolloff <= 1.0) {
            return Err("rolloff: 0 < r <= 1".into());
        }
        if !(p.loop_bw > 0.0 && p.loop_bw <= 0.5) {
            return Err("loop_bw: 0 < bw <= 0.5".into());
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
        ctx: StageContext,
    ) -> Result<(), StageError> {
        self.check_params(&params).map_err(StageError::Failed)?;
        let p: PskDemodParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{PSK_DEMOD_KIND} params: {e}")))?;
        let [out] = <[StreamId; 1]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 1 output, got {}", o.len())))?;
        // The run's demodulator and its last frame's provenance.
        let mut run: Option<(Demod, Provenance)> = None;
        let tail = |ctx: &StageContext, out: &StreamId, run: &mut Option<(Demod, Provenance)>| {
            let (mut d, prov) = run.take()?;
            let y = d.flush();
            publish(ctx, out, &mut d, &prov, y)
        };
        run_map_ending(PSK_DEMOD_KIND, inputs, bus, cancel, move |input| {
            let frame = match input {
                Some(Input::Frame(f)) => f,
                Some(Input::Lagged { .. }) | None => {
                    return Ok(tail(&ctx, &out, &mut run).into_iter().collect());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(format!(
                    "{PSK_DEMOD_KIND} stage expects iq frames"
                )));
            };
            let rate = block.sample_rate_hz.0;
            let span = &frame.provenance.span;
            let mut published = Vec::new();
            if run.as_ref().is_some_and(|(d, prev)| {
                d.rate_hz != rate || is_seam(prev.span.end_ns.0, span.start_ns.0, rate)
            }) {
                published.extend(tail(&ctx, &out, &mut run));
            }
            let (d, prev) = match &mut run {
                Some(r) => r,
                None => run.insert((
                    Demod::new(&p, rate, span.start_ns.0)?,
                    frame.provenance.clone(),
                )),
            };
            *prev = frame.provenance.clone();
            let y = d.process(&block.samples);
            published.extend(publish(&ctx, &out, d, prev, y));
            Ok(published)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use num_complex::Complex;
    use serde_json::{Value, json};
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::port::IqBlock;
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, JobId, Provenance, SourceId, Span, StreamKind, Timestamp};

    use super::{COARSE_FFT, MAX_COARSE_DEFERRALS, PskDemodStage, RRC_SPAN_SYMBOLS};
    use crate::frame_span;
    use crate::synth::{Impairments, SynthSpec, awgn, synthesise};

    const RATE: u64 = 38_400;
    const BAUD: u32 = 4_800;
    /// Input samples per output sample at [`RATE`].
    const DECIM: usize = 2;
    /// The matched filter's group delay at [`RATE`], in input samples.
    const DELAY: usize = RRC_SPAN_SYMBOLS * (RATE / BAUD as u64) as usize / 2;
    const T0: u64 = 1_000_000_000;
    /// Input samples per frame fed by [`blocks`]: 50 ms.
    const BLOCK: usize = 1920;
    /// Symbols of a burst the loops settle over before its Q share is read.
    const SETTLE_SYMBOLS: usize = 32;

    fn prov(t0: u64, n: usize, rate: u64) -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: frame_span(Timestamp(t0), n, rate),
        }
    }

    /// The stage's output frames (span, interleaved I/Q) over `input` (`(t0, iq, rate)`
    /// frames).
    async fn run_frames(
        params: Value,
        input: Vec<(u64, Vec<Complex<f32>>, u64)>,
    ) -> Result<Vec<(Span, Vec<f32>)>, StageError> {
        Ok(run_noted(params, input).await?.0)
    }

    /// [`run_frames`] and the stage's job-context notes.
    async fn run_noted(
        params: Value,
        input: Vec<(u64, Vec<Complex<f32>>, u64)>,
    ) -> Result<(Vec<(Span, Vec<f32>)>, Vec<Value>), StageError> {
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
        let notes = Arc::new(Mutex::new(Vec::new()));
        let sink = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_note(Arc::new(move |v| sink.lock().unwrap().push(v)));
        PskDemodStage::new()
            .run(
                params,
                vec![rx],
                vec![out],
                bus.clone(),
                CancellationToken::new(),
                ctx,
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
            assert_eq!((sample_rate_hz.0, channels), (RATE / DECIM as u64, 2));
            frames.push((f.provenance.span, samples.to_vec()));
        }
        let notes = notes.lock().unwrap().clone();
        Ok((frames, notes))
    }

    /// `iq` as [`BLOCK`]-sample frames from [`T0`], contiguous.
    fn blocks(iq: &[Complex<f32>]) -> Vec<(u64, Vec<Complex<f32>>, u64)> {
        iq.chunks(BLOCK)
            .enumerate()
            .map(|(i, c)| {
                let t0 = T0 + (i * BLOCK) as u64 * 1_000_000_000 / RATE;
                (t0, c.to_vec(), RATE)
            })
            .collect()
    }

    /// Clean BPSK of `symbols` alternating-run bits at [`RATE`], `offset_hz` off centre.
    fn bpsk(symbols: usize, offset_hz: f64) -> Vec<Complex<f32>> {
        let bits: Vec<u8> = (0..symbols).map(|i| ((i * 7 / 3) % 2) as u8).collect();
        let hex: String = bits
            .chunks(8)
            .map(|b| {
                let v = b
                    .iter()
                    .enumerate()
                    .fold(0u8, |a, (i, &x)| a | (x << (7 - i)));
                format!("{v:02x}")
            })
            .collect();
        let spec = SynthSpec::Psk {
            symbol_rate_baud: BAUD,
            order: 2,
            bits: hex,
            repeat: 1,
            gap_ms: 0,
        };
        let imp = Impairments {
            snr_db: None,
            offset_hz,
            ppm: 0.0,
            echo: None,
        };
        synthesise(&spec, &imp, RATE as u32, 0)
    }

    #[tokio::test]
    async fn a_rate_off_the_grid_is_refused_naming_the_rate_to_give_resample() {
        let e = run_frames(
            json!({"symbol_rate_baud": BAUD}),
            vec![(T0, vec![Complex::new(1.0, 0.0); 100], 40_000)],
        )
        .await
        .unwrap_err();
        let StageError::Failed(m) = e else {
            panic!("a failure, got {e}")
        };
        assert!(m.contains("resample to 57600 Hz"), "{m}");
        assert!(m.starts_with("psk_demod: "), "{m}");
        // Under 4 samples per symbol names the least rate on the grid.
        let e = super::decimation(12_000, BAUD).unwrap_err();
        assert!(e.contains("resample to 19200 Hz"), "{e}");
        assert_eq!(super::decimation(38_400, BAUD), Ok(2));
    }

    #[test]
    fn params_are_checked_naming_the_field() {
        let s = PskDemodStage::new();
        let check = |v: Value| s.check_params(&v);
        assert!(check(json!({"symbol_rate_baud": 4800})).is_ok());
        assert_eq!(
            check(json!({"symbol_rate_baud": 4800, "order": 3})).unwrap_err(),
            "order: 2, 4 or 8"
        );
        assert_eq!(
            check(json!({"symbol_rate_baud": 4800, "rolloff": 0.0})).unwrap_err(),
            "rolloff: 0 < r <= 1"
        );
        assert_eq!(
            check(json!({"symbol_rate_baud": 4800, "loop_bw": 0.6})).unwrap_err(),
            "loop_bw: 0 < bw <= 0.5"
        );
        assert!(check(json!({"order": 2})).is_err());
        assert!(check(json!({"symbol_rate_baud": 4800, "x": 1})).is_err());
    }

    /// The share of `iq`'s power (interleaved, the second half) in Q: near 0 once a
    /// BPSK stream is locked on the real axis.
    fn q_share(iq: &[f32]) -> f32 {
        q_share_from(iq, iq.len() / 4)
    }

    /// The share in Q of `iq`'s power after its first `skip` I/Q pairs.
    fn q_share_from(iq: &[f32], skip: usize) -> f32 {
        let (mut i2, mut q2) = (0.0, 0.0);
        for p in iq[2 * skip..].chunks(2) {
            i2 += p[0] * p[0];
            q2 += p[1] * p[1];
        }
        q2 / (i2 + q2)
    }

    #[tokio::test]
    async fn a_seam_flushes_the_filter_and_starts_a_new_offset_estimate() {
        // 1024 symbols at +300 Hz, a 1 s gap, then 1024 at -700 Hz: carried over, the
        // first run's offset would leave a 1 kHz residual the loop cannot pull in.
        const SYMBOLS: usize = 1024;
        let (a, b) = (bpsk(SYMBOLS, 300.0), bpsk(SYMBOLS, -700.0));
        let n = a.len();
        assert!(n >= COARSE_FFT);
        let t1 = T0 + 2_000_000_000;
        let frames = run_frames(
            json!({"symbol_rate_baud": BAUD}),
            vec![(T0, a, RATE), (t1, b, RATE)],
        )
        .await
        .unwrap();
        // Each run: its output, then the matched filter's tail at the seam and the end.
        let lens: Vec<usize> = frames.iter().map(|(_, y)| y.len() / 2).collect();
        let whole = (n + DELAY).div_ceil(DECIM);
        assert_eq!(
            lens,
            [n / DECIM, whole - n / DECIM, n / DECIM, whole - n / DECIM]
        );
        // The second run's time starts after the gap, less the filter's delay.
        let delay_ns = DELAY as u64 * 1_000_000_000 / RATE;
        assert_eq!(frames[2].0.start_ns.0, t1 - delay_ns);
        for k in [0, 2] {
            let share = q_share(&frames[k].1);
            assert!(share < 0.05, "run {k}: {share} of the power in Q");
        }
    }

    /// The `psk` note of `notes`, the only one.
    fn coarse_note(notes: &[Value]) -> &Value {
        assert_eq!(notes.len(), 1, "{notes:?}");
        &notes[0]["psk"]
    }

    #[tokio::test]
    async fn noise_ahead_of_the_signal_defers_the_offset_to_a_later_block() {
        // Two blocks of noise, then BPSK at +1200 Hz and 20 dB.
        const NOISE: usize = 2 * COARSE_FFT;
        let mut sig = bpsk(1024, 1200.0);
        awgn(&mut sig, 20.0, 11);
        let mut noise = vec![Complex::new(0.0, 0.0); NOISE];
        awgn(&mut noise, 20.0, 12);
        let iq = [noise, sig].concat();
        let n = iq.len();
        let (frames, notes) = run_noted(json!({"symbol_rate_baud": BAUD}), blocks(&iq))
            .await
            .unwrap();
        let note = coarse_note(&notes);
        assert_eq!(note["coarse_confident"], true, "{note}");
        assert_eq!(note["coarse_deferrals"], 2, "{note}");
        let hz = note["coarse_hz"].as_f64().unwrap();
        assert!((hz - 1200.0).abs() < 5.0, "{hz} Hz");
        let y: Vec<f32> = frames.into_iter().flat_map(|(_, y)| y).collect();
        assert_eq!(y.len() / 2, (n + DELAY).div_ceil(DECIM));
        let share = q_share(&y);
        assert!(share < 0.05, "{share} of the power in Q");
    }

    #[tokio::test]
    async fn pure_noise_falls_back_to_0_hz_unconfident() {
        let mut iq = vec![Complex::new(0.0, 0.0); (MAX_COARSE_DEFERRALS + 2) * COARSE_FFT];
        awgn(&mut iq, 20.0, 13);
        let n = iq.len();
        let (frames, notes) = run_noted(json!({"symbol_rate_baud": BAUD}), blocks(&iq))
            .await
            .unwrap();
        let note = coarse_note(&notes);
        assert_eq!(note["coarse_confident"], false, "{note}");
        assert_eq!(note["coarse_hz"], 0.0, "{note}");
        assert_eq!(note["coarse_deferrals"], MAX_COARSE_DEFERRALS, "{note}");
        let out: usize = frames.iter().map(|(_, y)| y.len() / 2).sum();
        assert_eq!(out, (n + DELAY).div_ceil(DECIM));
    }

    #[tokio::test]
    async fn a_short_burst_is_estimated_at_the_flush_and_locks() {
        // 125 symbols (1000 samples) at +1200 Hz and 20 dB: under one coarse block, so
        // the whole burst waits for the flush.
        let mut iq = bpsk(125, 1200.0);
        awgn(&mut iq, 20.0, 14);
        let n = iq.len();
        assert!(n < COARSE_FFT);
        let (frames, notes) = run_noted(json!({"symbol_rate_baud": BAUD}), blocks(&iq))
            .await
            .unwrap();
        let note = coarse_note(&notes);
        assert_eq!(note["coarse_confident"], true, "{note}");
        assert_eq!(note["coarse_deferrals"], 0, "{note}");
        let hz = note["coarse_hz"].as_f64().unwrap();
        assert!((hz - 1200.0).abs() < 10.0, "{hz} Hz");
        assert_eq!(frames.len(), 1);
        let y = &frames[0].1;
        assert_eq!(y.len() / 2, (n + DELAY).div_ceil(DECIM));
        // The filter's delay, then the settling symbols, then the burst's body (the
        // tail of zeros through the filter excluded).
        let skip = DELAY / DECIM + SETTLE_SYMBOLS * super::OUT_SAMPLES_PER_SYMBOL as usize;
        let body = &y[..2 * (n / DECIM)];
        let share = q_share_from(body, skip);
        assert!(share < 0.05, "{share} of the power in Q");
    }
}
