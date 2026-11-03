//! Symbol timing recovery: one sample per symbol out of a decision signal.
//!
//! A linear fractional interpolator picks one strobe per symbol; a timing error
//! detector (Mueller and Müller, Gardner or early-late) measures how far each strobe
//! sits from the symbol's centre and a second-order loop (proportional plus integral,
//! gains from `loop_bw`) moves the next strobe and the period estimate, which stays
//! within `max_ppm` of the nominal samples per symbol.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, StreamId, StreamKind, Timestamp};

use crate::{Input, is_seam, run_map_ending};

/// The stage kind, as graphs name it.
pub const CLOCK_RECOVER_KIND: &str = "clock_recover";

/// Default loop noise bandwidth, normalised to the symbol rate.
pub const LOOP_BW_DEFAULT: f64 = 0.01;

/// Default furthest the period estimate may move from nominal, in parts per million.
pub const MAX_PPM_DEFAULT: u64 = 2000;

/// Input time between two `timing` events, in milliseconds.
pub const TIMING_EVERY_MS: u64 = 1000;

/// Largest `error_rms` over a timing window at which the loop reads `locked`.
/// `error_rms` is the RMS over the window of the detector's per-symbol output divided by
/// the signal's running mean power (DC removed, summed over channels; each output
/// clamped to ±4), so it is dimensionless and one constant serves 1 and 2 channels and
/// every detector. Every detector's output is a product of two samples, so locked it
/// reads the noise only: about 0.2 at 20 dB (M&M, early-late) and 0.35 for Gardner on
/// BPSK at 10 dB, acquisition included; noise alone, with no symbols to lock to, reads
/// about 1.0.
pub const LOCK_ERROR_RMS: f64 = 0.7;

/// Largest `max_ppm` accepted.
const MAX_PPM_LIMIT: u64 = 100_000;

/// Largest `loop_bw` accepted.
const LOOP_BW_MAX: f64 = 0.5;

/// Fewest samples per symbol the interpolator and the detectors work with.
const MIN_SAMPLES_PER_SYMBOL: f64 = 2.0;

/// Loop damping factor.
const DAMPING: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Time constant of the per-channel DC estimate the detectors subtract, in symbols.
const DC_TC_SYMBOLS: f64 = 32.0;

/// Time constant of the running mean power that normalises the detector, in symbols.
const POWER_TC_SYMBOLS: f64 = 32.0;

/// Furthest one normalised detector output may move the loop, either way.
const ERROR_CLAMP: f64 = 4.0;

/// Furthest one symbol's step may move from the period, as a fraction of it, so the
/// strobe always moves forward (a step in `[period/2, 3·period/2]`) even at the
/// largest `loop_bw`.
const STEP_REACH: f64 = 0.5;

/// Floor of the normalising power, so silence does not divide by zero.
const MIN_POWER: f64 = 1e-12;

/// Early-late gate offset either side of the strobe, as a fraction of the period.
const EARLY_LATE_FRACTION: f64 = 0.25;

/// Fewest symbols a partial timing window (cut by a seam, a lag or the input's end)
/// needs to be reported.
const TIMING_MIN_SYMBOLS: usize = 16;

const NS_PER_S: f64 = 1e9;
const MS_PER_S: u64 = 1000;
const PER_MILLION: f64 = 1e6;

fn default_loop_bw() -> f64 {
    LOOP_BW_DEFAULT
}

fn default_max_ppm() -> u64 {
    MAX_PPM_DEFAULT
}

/// The timing error detector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Algorithm {
    /// Mueller and Müller: one sample per symbol and its sign decision.
    Mm,
    /// Gardner: the zero crossing half a symbol between two strobes.
    Gardner,
    /// Early-late gate: the slope between samples a quarter symbol either side.
    EarlyLate,
}

/// `clock_recover` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ClockRecoverParams {
    /// The nominal symbol rate in baud.
    pub symbol_rate_baud: u32,
    /// The detector; `null` is `mm` on 1 channel and `gardner` on 2.
    #[serde(default)]
    pub algorithm: Option<Algorithm>,
    /// Loop noise bandwidth normalised to the symbol rate, 0 < bw <= 0.5.
    #[serde(default = "default_loop_bw")]
    pub loop_bw: f64,
    /// Furthest the period estimate may move from nominal, in ppm.
    #[serde(default = "default_max_ppm")]
    pub max_ppm: u64,
}

/// Samples per symbol of `rate_hz` at `baud`, refused under 2.
pub(crate) fn samples_per_symbol(rate_hz: u64, baud: u32) -> Result<f64, StageError> {
    let sps = rate_hz as f64 / baud as f64;
    if sps < MIN_SAMPLES_PER_SYMBOL {
        return Err(StageError::Failed(format!(
            "{CLOCK_RECOVER_KIND}: {rate_hz} Hz is {sps:.1} samples per symbol at {baud} Bd; needs at least 2"
        )));
    }
    Ok(sps)
}

/// What one timing window has seen.
#[derive(Default)]
struct Window {
    /// Input samples (per channel) taken since the window opened.
    samples: u64,
    err2: f64,
    errors: usize,
    first: Option<f64>,
    last: f64,
    symbols: usize,
}

/// The timing loop over one run of contiguous input.
struct Loop {
    rate_hz: u64,
    channels: usize,
    baud: u32,
    algorithm: Algorithm,
    sps: f64,
    period: f64,
    period_lo: f64,
    period_hi: f64,
    kp: f64,
    ki: f64,
    /// Time of the run's first input sample.
    t0_ns: u64,
    /// Raw input samples, interleaved, from absolute sample index `base`.
    buf: Vec<f32>,
    base: u64,
    /// Absolute sample position of the next strobe.
    next: f64,
    dc: Vec<f64>,
    dc_alpha: f64,
    power: f64,
    /// The previous strobe, DC removed.
    prev: Option<Vec<f64>>,
    window: Window,
    window_samples: u64,
    prov: Provenance,
}

impl Loop {
    fn new(
        p: &ClockRecoverParams,
        rate_hz: u64,
        channels: usize,
        first: &[f32],
        prov: Provenance,
    ) -> Result<Loop, StageError> {
        let sps = samples_per_symbol(rate_hz, p.symbol_rate_baud)?;
        let algorithm = p.algorithm.unwrap_or(if channels == 1 {
            Algorithm::Mm
        } else {
            Algorithm::Gardner
        });
        // Second-order loop gains for a detector of unit gain per symbol of timing error.
        let theta = p.loop_bw / (DAMPING + 1.0 / (4.0 * DAMPING));
        let d = 1.0 + 2.0 * DAMPING * theta + theta * theta;
        let reach = p.max_ppm as f64 / PER_MILLION;
        let frames = (first.len() / channels).max(1);
        let dc: Vec<f64> = (0..channels)
            .map(|c| {
                first
                    .iter()
                    .skip(c)
                    .step_by(channels)
                    .map(|&v| v as f64)
                    .sum::<f64>()
                    / frames as f64
            })
            .collect();
        let power = first
            .chunks(channels)
            .map(|s| {
                s.iter()
                    .zip(&dc)
                    .map(|(&v, m)| (v as f64 - m).powi(2))
                    .sum::<f64>()
            })
            .sum::<f64>()
            / frames as f64;
        Ok(Loop {
            rate_hz,
            channels,
            baud: p.symbol_rate_baud,
            algorithm,
            sps,
            period: sps,
            period_lo: sps * (1.0 - reach),
            period_hi: sps * (1.0 + reach),
            kp: 4.0 * DAMPING * theta / d,
            ki: 4.0 * theta * theta / d,
            t0_ns: prov.span.start_ns.0,
            buf: Vec::new(),
            base: 0,
            next: sps / 2.0,
            dc,
            dc_alpha: 1.0 / (DC_TC_SYMBOLS * sps),
            power: power.max(MIN_POWER),
            prev: None,
            window: Window::default(),
            window_samples: rate_hz * TIMING_EVERY_MS / MS_PER_S,
            prov,
        })
    }

    /// The input interpolated at absolute position `pos`, raw.
    fn at(&self, pos: f64) -> Vec<f64> {
        let i = pos.floor();
        let f = pos - i;
        let k = (i as u64 - self.base) as usize * self.channels;
        (0..self.channels)
            .map(|c| {
                let (a, b) = (
                    self.buf[k + c] as f64,
                    self.buf[k + self.channels + c] as f64,
                );
                a + (b - a) * f
            })
            .collect()
    }

    /// `x` with the DC estimate removed.
    fn ac(&self, x: &[f64]) -> Vec<f64> {
        x.iter().zip(&self.dc).map(|(v, m)| v - m).collect()
    }

    fn time_ns(&self, pos: f64) -> u64 {
        self.t0_ns + (pos * NS_PER_S / self.rate_hz as f64) as u64
    }

    /// The detector's output for the strobe `on` (DC removed) at `self.next`, `None`
    /// when it needs a previous strobe there is none of. Positive means the strobe is
    /// early (the symbol's centre lies later).
    fn detect(&self, on: &[f64]) -> Option<f64> {
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
        match self.algorithm {
            Algorithm::Mm => self.prev.as_ref().map(|p| {
                let sign = |v: &[f64]| v.iter().map(|x| x.signum()).collect::<Vec<_>>();
                dot(&sign(p), on) - dot(&sign(on), p)
            }),
            Algorithm::Gardner => self.prev.as_ref().map(|p| {
                let mid = self.ac(&self.at(self.next - self.period / 2.0));
                let diff: Vec<f64> = p.iter().zip(on).map(|(a, b)| a - b).collect();
                dot(&diff, &mid)
            }),
            Algorithm::EarlyLate => {
                let off = self.period * EARLY_LATE_FRACTION;
                let early = self.ac(&self.at(self.next - off));
                let late = self.ac(&self.at(self.next + off));
                let slope: Vec<f64> = late.iter().zip(&early).map(|(l, e)| l - e).collect();
                Some(dot(&slope, on))
            }
        }
    }

    /// Take one input frame's samples; the symbols strobed in it (interleaved, raw) and
    /// the position of the first.
    fn step(&mut self, samples: &[f32]) -> (Vec<f32>, Option<f64>) {
        for s in samples.chunks(self.channels) {
            for (m, &v) in self.dc.iter_mut().zip(s) {
                *m += (v as f64 - *m) * self.dc_alpha;
            }
        }
        self.buf.extend_from_slice(samples);
        let end = self.base + (self.buf.len() / self.channels) as u64;
        let look = match self.algorithm {
            Algorithm::EarlyLate => self.period * EARLY_LATE_FRACTION,
            _ => 0.0,
        };
        let mut out = Vec::new();
        let mut first = None;
        while (self.next + look).floor() as u64 + 1 < end {
            let raw = self.at(self.next);
            let on = self.ac(&raw);
            let p: f64 = on.iter().map(|v| v * v).sum();
            self.power += (p - self.power) / POWER_TC_SYMBOLS;
            let mut step = self.period;
            // The detector's output over the mean power: dimensionless.
            if let Some(e) = self.detect(&on) {
                let e = (e / self.power.max(MIN_POWER)).clamp(-ERROR_CLAMP, ERROR_CLAMP);
                self.window.err2 += e * e;
                self.window.errors += 1;
                self.period =
                    (self.period + self.sps * self.ki * e).clamp(self.period_lo, self.period_hi);
                // Bounded so the strobe always moves forward, whatever `loop_bw`.
                step = (self.period + self.sps * self.kp * e).clamp(
                    self.period * (1.0 - STEP_REACH),
                    self.period * (1.0 + STEP_REACH),
                );
            }
            first.get_or_insert(self.next);
            self.window.first.get_or_insert(self.next);
            self.window.last = self.next;
            self.window.symbols += 1;
            out.extend(raw.iter().map(|&v| v as f32));
            self.prev = Some(on);
            self.next += step;
        }
        // Keep what the next strobe's interpolation (a period back at most) needs.
        let keep = ((self.next - self.period).floor() as u64)
            .saturating_sub(1)
            .clamp(self.base, end);
        let drop = (keep - self.base) as usize * self.channels;
        self.buf.drain(..drop);
        self.base = keep;
        self.window.samples += (samples.len() / self.channels) as u64;
        (out, first)
    }

    /// The `timing` event for the window so far, and the window reopened.
    fn report(&mut self) -> serde_json::Value {
        let w = std::mem::take(&mut self.window);
        self.window.samples = w.samples.saturating_sub(self.window_samples);
        let rate_est_baud = match w.first {
            Some(f) if w.symbols >= 2 && w.last > f => {
                self.rate_hz as f64 * (w.symbols - 1) as f64 / (w.last - f)
            }
            _ => self.rate_hz as f64 / self.period,
        };
        let error_rms = if w.errors == 0 {
            f64::INFINITY
        } else {
            (w.err2 / w.errors as f64).sqrt()
        };
        let locked = error_rms < LOCK_ERROR_RMS;
        let error_rms = if error_rms.is_finite() {
            error_rms
        } else {
            0.0
        };
        serde_json::json!({
            "protocol": "timing",
            "rate_est_baud": rate_est_baud,
            "error_rms": error_rms,
            "locked": locked,
            "t_ns": self.time_ns(w.last),
            "summary": format!(
                "{rate_est_baud:.1} Bd, error {error_rms:.3}, {}",
                if locked { "locked" } else { "searching" }
            ),
        })
    }

    /// The event frame of `report` over the window's symbols.
    fn event_frame(&mut self, events: &StreamId, ctx: &StageContext) -> Frame {
        let start = self.time_ns(self.window.first.unwrap_or(self.window.last));
        let end = self.time_ns(self.window.last);
        let ev = self.report();
        ctx.note(serde_json::json!({"clock": {
            "rate_est_baud": ev["rate_est_baud"],
            "error_rms": ev["error_rms"],
            "locked": ev["locked"],
            "stream": events.0,
        }}));
        let mut prov = self.prov.clone();
        prov.span = zeta_core::types::Span {
            start_ns: Timestamp(start),
            end_ns: Timestamp(end),
        };
        Frame::new(events.clone(), prov, Payload::Event(ev))
    }

    /// The partial window's event, when it has symbols enough to say anything.
    fn flush(&mut self, events: &StreamId, ctx: &StageContext) -> Option<Frame> {
        (self.window.symbols >= TIMING_MIN_SYMBOLS).then(|| self.event_frame(events, ctx))
    }
}

/// Recovers the symbol clock of a 1-channel decision signal or 2-channel I/Q: one
/// sample per symbol out at `sample_rate_hz = symbol_rate_baud`, each frame spanning
/// its first symbol's time to the next strobe's (so frames within a run meet exactly), and a `timing` event every [`TIMING_EVERY_MS`] of
/// input (and for the partial window at a seam, a lag or the input's end) noted to the
/// job as `clock`. A seam ([`is_seam`]), a lag or a change of rate or channels restarts
/// the loop.
pub struct ClockRecoverStage {
    spec: StageSpec,
}

impl ClockRecoverStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: CLOCK_RECOVER_KIND,
                inputs: vec![StreamKind::Audio],
                outputs: vec![StreamKind::Audio, StreamKind::Event],
                params_schema: schemars::schema_for!(ClockRecoverParams),
            },
        }
    }
}

impl Default for ClockRecoverStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for ClockRecoverStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: ClockRecoverParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        if p.symbol_rate_baud == 0 {
            return Err("symbol_rate_baud: at least 1".into());
        }
        if !(p.loop_bw > 0.0 && p.loop_bw <= LOOP_BW_MAX) {
            return Err("loop_bw: 0 < bw <= 0.5".into());
        }
        if p.max_ppm > MAX_PPM_LIMIT {
            return Err(format!("max_ppm: at most {MAX_PPM_LIMIT}"));
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
        let p: ClockRecoverParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{CLOCK_RECOVER_KIND} params: {e}")))?;
        let [out, events] = <[StreamId; 2]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 2 outputs, got {}", o.len())))?;
        let mut lp: Option<Loop> = None;
        let mut prev_end: Option<u64> = None;
        run_map_ending(CLOCK_RECOVER_KIND, inputs, bus, cancel, move |input| {
            let mut published = Vec::new();
            let frame = match input {
                Some(Input::Frame(f)) => f,
                Some(Input::Lagged { .. }) | None => {
                    prev_end = None;
                    published.extend(lp.take().and_then(|mut l| l.flush(&events, &ctx)));
                    return Ok(published);
                }
            };
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &frame.payload
            else {
                return Err(StageError::Failed(format!(
                    "{CLOCK_RECOVER_KIND} stage expects audio frames"
                )));
            };
            let (rate, channels) = (sample_rate_hz.0, *channels as usize);
            if !(1..=2).contains(&channels) {
                return Err(StageError::Failed(format!(
                    "{CLOCK_RECOVER_KIND}: expects 1 or 2 channels, got {channels}"
                )));
            }
            if samples.len() % channels != 0 {
                return Err(StageError::Failed(format!(
                    "{CLOCK_RECOVER_KIND}: {} samples is not a whole number of {channels}-channel frames",
                    samples.len()
                )));
            }
            let span = &frame.provenance.span;
            let seam = prev_end.is_some_and(|e| is_seam(e, span.start_ns.0, rate));
            prev_end = Some(span.end_ns.0);
            if seam
                || lp
                    .as_ref()
                    .is_some_and(|l| l.rate_hz != rate || l.channels != channels)
            {
                published.extend(lp.take().and_then(|mut l| l.flush(&events, &ctx)));
            }
            let l = match &mut lp {
                Some(l) => l,
                None => lp.insert(Loop::new(
                    &p,
                    rate,
                    channels,
                    samples,
                    frame.provenance.clone(),
                )?),
            };
            let (symbols, first) = l.step(samples);
            if let Some(first) = first {
                let mut prov = frame.provenance.clone();
                // From the first symbol's time to the next strobe's, so the next frame
                // starts where this one ends however the loop spaced its symbols.
                prov.span = zeta_core::types::Span {
                    start_ns: Timestamp(l.time_ns(first)),
                    end_ns: Timestamp(l.time_ns(l.next)),
                };
                published.push(Frame::new(
                    out.clone(),
                    prov,
                    Payload::Audio {
                        sample_rate_hz: Hz(l.baud as u64),
                        channels: channels as u16,
                        samples: symbols.into(),
                    },
                ));
            }
            if l.window.samples >= l.window_samples {
                published.push(l.event_frame(&events, &ctx));
            }
            Ok(published)
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, JobId, Provenance, SourceId, StreamKind, Timestamp};

    use super::{ClockRecoverStage, LOCK_ERROR_RMS, samples_per_symbol};
    use crate::fixtures::awgn_real;
    use crate::frame_span;

    const BAUD: u32 = 4_800;
    const T0: u64 = 1_000_000_000;
    const BLOCK_MS: u64 = 20;
    const SKIP: usize = 100;
    const SEAM_GAP_NS: u64 = 2_000_000_000;
    /// A clock error that walks the strobe across 4 symbols of a 4000-bit stream, so a
    /// loop that did not track would sample every symbol edge on the way.
    const TRACK_PPM: f64 = 1000.0;

    /// Deterministic random bits.
    fn random_bits(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (s >> 63) as u8
            })
            .collect()
    }

    /// Rectangular ±1 pulses at `sps × (1 + ppm/1e6)` samples per symbol: each sample is
    /// the mean of the waveform over its interval, so a sample straddling a symbol edge
    /// is the linear mix of the two levels.
    fn nrz(bits: &[u8], sps: f64, ppm: f64) -> Vec<f32> {
        let period = sps * (1.0 + ppm / 1e6);
        let n = (bits.len() as f64 * period).floor() as usize;
        let level = |k: usize| {
            if bits[k.min(bits.len() - 1)] == 1 {
                1.0
            } else {
                -1.0
            }
        };
        (0..n)
            .map(|i| {
                let (a, b) = (i as f64 / period, (i + 1) as f64 / period);
                let k = a.floor() as usize;
                let edge = (k + 1) as f64;
                if b <= edge {
                    level(k) as f32
                } else {
                    let f = (edge - a) / (b - a);
                    (f * level(k) + (1.0 - f) * level(k + 1)) as f32
                }
            })
            .collect()
    }

    /// BPSK as interleaved I/Q: `nrz` on I, 0 on Q.
    fn bpsk_iq(bits: &[u8], sps: f64) -> Vec<f32> {
        nrz(bits, sps, 0.0)
            .into_iter()
            .flat_map(|i| [i, 0.0])
            .collect()
    }

    fn prov(t0: u64, n: usize, rate_hz: u64) -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: frame_span(Timestamp(t0), n, rate_hz),
        }
    }

    /// `samples` (interleaved, `channels`) at `rate_hz` from `t0` as 20 ms audio frames.
    fn frames(samples: &[f32], channels: u16, rate_hz: u64, t0: u64) -> Vec<Frame> {
        let per = (rate_hz * BLOCK_MS / 1000) as usize * channels as usize;
        let id = zeta_core::types::StreamId::new();
        samples
            .chunks(per)
            .enumerate()
            .map(|(k, c)| {
                let start = t0 + k as u64 * BLOCK_MS * 1_000_000;
                Frame::new(
                    id.clone(),
                    prov(start, c.len() / channels as usize, rate_hz),
                    Payload::Audio {
                        sample_rate_hz: Hz(rate_hz),
                        channels,
                        samples: Arc::from(c.to_vec()),
                    },
                )
            })
            .collect()
    }

    /// The input duration of `n` samples at `rate_hz`, in ns.
    fn dur_ns(n: usize, rate_hz: u64) -> u64 {
        n as u64 * 1_000_000_000 / rate_hz
    }

    struct Run {
        frames: Vec<Frame>,
        events: Vec<Value>,
        context: Value,
        event_stream: String,
    }

    /// Run the stage over `input` to its end: its audio frames, event payloads, the job
    /// context its notes build (merged key by key) and its event stream's id.
    async fn run_clock_frames(params: Value, input: Vec<Frame>) -> Run {
        try_run_clock_frames(params, input).await.unwrap()
    }

    /// [`run_clock_frames`], or the stage's error.
    async fn try_run_clock_frames(params: Value, input: Vec<Frame>) -> Result<Run, StageError> {
        let bus = Arc::new(StreamBus::new(4096));
        let p = prov(0, 0, 1);
        let out = bus.register(StreamKind::Audio, p.clone());
        let ev = bus.register(StreamKind::Event, p);
        let mut rx_out = bus.subscribe(&out).unwrap();
        let mut rx_ev = bus.subscribe(&ev).unwrap();
        let (tx, rx) = broadcast::channel(input.len().max(1));
        let notes = Arc::new(Mutex::new(Vec::new()));
        let sink = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_note(Arc::new(move |v| sink.lock().unwrap().push(v)));
        for f in input {
            tx.send(f).unwrap();
        }
        drop(tx);
        ClockRecoverStage::new()
            .run(
                params,
                vec![rx],
                vec![out, ev.clone()],
                bus.clone(),
                CancellationToken::new(),
                ctx,
            )
            .await?;
        let mut frames = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            frames.push(f);
        }
        let mut events = Vec::new();
        while let Ok(f) = rx_ev.try_recv() {
            let Payload::Event(v) = f.payload else {
                panic!("an event payload")
            };
            events.push(v);
        }
        let mut context = serde_json::Map::new();
        for n in notes.lock().unwrap().iter() {
            for (k, v) in n.as_object().unwrap() {
                context.insert(k.clone(), v.clone());
            }
        }
        Ok(Run {
            frames,
            events,
            context: Value::Object(context),
            event_stream: ev.0,
        })
    }

    /// Output frames start in time order, each after the previous one's start, and no
    /// two consecutive ones read as a seam.
    fn assert_spans_continuous(frames: &[Frame]) {
        for w in frames.windows(2) {
            let (a, b) = (&w[0].provenance.span, &w[1].provenance.span);
            assert!(b.start_ns.0 > a.start_ns.0, "{a:?} -> {b:?}");
            assert!(
                !crate::is_seam(a.end_ns.0, b.start_ns.0, BAUD as u64),
                "{a:?} -> {b:?}"
            );
        }
    }

    fn audio(f: &Frame) -> (u64, u16, &[f32]) {
        let Payload::Audio {
            sample_rate_hz,
            channels,
            samples,
        } = &f.payload
        else {
            panic!("an audio frame")
        };
        (sample_rate_hz.0, *channels, samples)
    }

    /// The stage's symbols (interleaved), its events and the job context.
    async fn run_clock(params: Value, input: Vec<Frame>) -> (Vec<f32>, Vec<Value>, Value) {
        let run = run_clock_frames(params, input).await;
        let symbols = run
            .frames
            .iter()
            .flat_map(|f| audio(f).2.iter().copied())
            .collect();
        (symbols, run.events, run.context)
    }

    /// The fewest bit errors over every alignment of `want` inside `got`, the inverted
    /// bits included (a binary decision may come out inverted); `want.len()` when `got`
    /// is shorter.
    fn align_errors(got: &[u8], want: &[u8]) -> usize {
        if got.len() < want.len() {
            return want.len();
        }
        (0..=got.len() - want.len())
            .map(|off| {
                let e = got[off..].iter().zip(want).filter(|(g, w)| g != w).count();
                e.min(want.len() - e)
            })
            .min()
            .unwrap_or(want.len())
    }

    /// The bits `want` minus a margin each end, so every alignment fits after `SKIP`.
    fn middle(bits: &[u8]) -> &[u8] {
        &bits[SKIP + 50..bits.len() - 50]
    }

    #[tokio::test]
    async fn mm_recovers_a_binary_stream_under_clock_error_and_noise() {
        let bits = random_bits(4000, 1);
        let ppm = 20.0;
        let mut x = nrz(&bits, 8.0, ppm)[3..].to_vec();
        awgn_real(&mut x, 20.0, 7);
        let rate = 8 * BAUD as u64;
        let (symbols, events, context) = run_clock(
            serde_json::json!({"symbol_rate_baud": BAUD}),
            frames(&x, 1, rate, T0),
        )
        .await;
        let got: Vec<u8> = symbols.iter().map(|&s| (s > 0.0) as u8).collect();
        assert_eq!(align_errors(&got[SKIP..], middle(&bits)), 0);
        let last = events.last().expect("a timing event");
        assert_eq!(last["protocol"], "timing");
        let want = BAUD as f64 / (1.0 + ppm / 1e6);
        let est = last["rate_est_baud"].as_f64().unwrap();
        assert!((est - want).abs() / want < 0.005, "{est} Bd");
        assert_eq!(last["locked"], true, "{last}");
        assert!(last["error_rms"].as_f64().unwrap() < LOCK_ERROR_RMS);
        assert!(
            last["summary"].as_str().unwrap().ends_with("locked"),
            "{last}"
        );
        assert_eq!(context["clock"]["locked"], true);
    }

    #[tokio::test]
    async fn mm_tracks_a_1000_ppm_clock_error() {
        let bits = random_bits(4000, 14);
        let mut x = nrz(&bits, 8.0, TRACK_PPM)[3..].to_vec();
        awgn_real(&mut x, 20.0, 15);
        let rate = 8 * BAUD as u64;
        let (symbols, events, _) = run_clock(
            serde_json::json!({"symbol_rate_baud": BAUD}),
            frames(&x, 1, rate, T0),
        )
        .await;
        let got: Vec<u8> = symbols.iter().map(|&s| (s > 0.0) as u8).collect();
        assert_eq!(align_errors(&got[SKIP..], middle(&bits)), 0);
        let want = BAUD as f64 / (1.0 + TRACK_PPM / 1e6);
        let est = events.last().unwrap()["rate_est_baud"].as_f64().unwrap();
        // Within half the clock error: on rectangular pulses M&M is blind across the
        // plateau, so the strobe wanders inside it and the window's mean spacing with it.
        assert!((est - want).abs() / want < 5e-4, "{est} Bd vs {want}");
    }

    #[tokio::test]
    async fn gardner_recovers_bpsk_at_4_sps() {
        let bits = random_bits(4000, 2);
        let mut x = bpsk_iq(&bits, 4.0 * (1.0 + TRACK_PPM / 1e6))[2 * 2..].to_vec();
        awgn_real(&mut x, 10.0, 8);
        let rate = 4 * BAUD as u64;
        let run = run_clock_frames(
            serde_json::json!({"symbol_rate_baud": BAUD}),
            frames(&x, 2, rate, T0),
        )
        .await;
        assert!(run.frames.iter().all(|f| audio(f).0 == BAUD as u64));
        assert!(run.frames.iter().all(|f| audio(f).1 == 2));
        let got: Vec<u8> = run
            .frames
            .iter()
            .flat_map(|f| audio(f).2.chunks(2).map(|iq| (iq[0] > 0.0) as u8))
            .collect();
        let want = middle(&bits);
        let errors = align_errors(&got[SKIP..], want);
        assert!(errors * 100 < want.len(), "{errors} errors");
    }

    #[tokio::test]
    async fn early_late_recovers_an_ook_envelope() {
        let bits = random_bits(4000, 3);
        let mut x: Vec<f32> = nrz(&bits, 8.0, TRACK_PPM)[5..]
            .iter()
            .map(|&v| (v + 1.0) / 2.0)
            .collect();
        awgn_real(&mut x, 20.0, 9);
        let rate = 8 * BAUD as u64;
        let (symbols, _, _) = run_clock(
            serde_json::json!({"symbol_rate_baud": BAUD, "algorithm": "early_late"}),
            frames(&x, 1, rate, T0),
        )
        .await;
        let got: Vec<u8> = symbols.iter().map(|&s| (s > 0.5) as u8).collect();
        assert_eq!(align_errors(&got[SKIP..], middle(&bits)), 0);
    }

    #[tokio::test]
    async fn a_seam_resets_the_loop_and_the_note_follows() {
        let rate = 8 * BAUD as u64;
        let (a_bits, b_bits) = (random_bits(4000, 4), random_bits(4000, 5));
        let mut a = nrz(&a_bits, 8.0, TRACK_PPM)[1..].to_vec();
        let mut b = nrz(&b_bits, 8.0, -TRACK_PPM)[5..].to_vec();
        awgn_real(&mut a, 20.0, 10);
        awgn_real(&mut b, 20.0, 11);
        let t1 = T0 + dur_ns(a.len(), rate) + SEAM_GAP_NS;
        let mut input = frames(&a, 1, rate, T0);
        input.extend(frames(&b, 1, rate, t1));
        let run = run_clock_frames(serde_json::json!({"symbol_rate_baud": BAUD}), input).await;
        let split = run
            .frames
            .iter()
            .position(|f| f.provenance.span.start_ns.0 >= t1)
            .expect("frames of the second stream");
        // The second stream's symbols are timed from its own start, not the first's.
        let symbol_ns = 1_000_000_000 / BAUD as u64;
        let b0 = run.frames[split].provenance.span.start_ns.0;
        assert!(b0 - t1 < symbol_ns, "{} ns after the seam", b0 - t1);
        let bits_of = |fs: &[Frame]| -> Vec<u8> {
            fs.iter()
                .flat_map(|f| audio(f).2.iter().map(|&s| (s > 0.0) as u8))
                .collect()
        };
        let (got_a, got_b) = (bits_of(&run.frames[..split]), bits_of(&run.frames[split..]));
        assert_eq!(align_errors(&got_a[SKIP..], middle(&a_bits)), 0);
        assert_eq!(align_errors(&got_b[SKIP..], middle(&b_bits)), 0);
        // Each stream reports its own timing, the first one at the seam.
        assert_eq!(run.events.len(), 2, "{:?}", run.events);
        let clock = &run.context["clock"];
        assert!(clock["rate_est_baud"].as_f64().unwrap() > 0.0, "{clock}");
        assert_eq!(clock["stream"], run.event_stream.as_str());
        assert_eq!(clock["locked"], true);
    }

    #[tokio::test]
    async fn output_frames_are_continuous_within_a_stream() {
        let rate = 8 * BAUD as u64;
        let mut x = nrz(&random_bits(4000, 6), 8.0, 0.0);
        awgn_real(&mut x, 20.0, 12);
        let run = run_clock_frames(
            serde_json::json!({"symbol_rate_baud": BAUD}),
            frames(&x, 1, rate, T0),
        )
        .await;
        for w in run.frames.windows(2) {
            let (end, next) = (
                w[0].provenance.span.end_ns.0,
                w[1].provenance.span.start_ns.0,
            );
            assert!(!crate::is_seam(end, next, BAUD as u64), "{end} -> {next}");
        }
    }

    /// Params for `algorithm` with the widest loop accepted.
    fn wide(algorithm: &str) -> Value {
        serde_json::json!({
            "symbol_rate_baud": BAUD,
            "loop_bw": super::LOOP_BW_MAX,
            "algorithm": algorithm,
        })
    }

    #[tokio::test]
    async fn the_widest_loop_over_noise_keeps_the_strobe_moving_forward() {
        let rate = 8 * BAUD as u64;
        let mut x = vec![0.0f32; 2 * rate as usize];
        awgn_real(&mut x, 0.0, 16);
        for algorithm in ["mm", "gardner", "early_late"] {
            let run = run_clock_frames(wide(algorithm), frames(&x, 1, rate, T0)).await;
            assert_spans_continuous(&run.frames);
            let n: usize = run.frames.iter().map(|f| audio(f).2.len()).sum();
            let symbols = x.len() / 8;
            assert!(
                n >= symbols * 2 / 3 && n <= symbols * 2,
                "{algorithm}: {n} symbols from {symbols}"
            );
        }
    }

    #[tokio::test]
    async fn the_widest_loop_over_a_1000_ppm_stream_strobes_each_symbol_once() {
        // On rectangular pulses no detector holds the bits at the widest loop (the
        // decisions need a narrower `loop_bw`); the strobe still moves forward, one per
        // symbol, and the frames stay continuous.
        let bits = random_bits(4000, 17);
        let mut x = nrz(&bits, 8.0, TRACK_PPM)[3..].to_vec();
        awgn_real(&mut x, 20.0, 18);
        let rate = 8 * BAUD as u64;
        for algorithm in ["mm", "gardner", "early_late"] {
            let run = run_clock_frames(wide(algorithm), frames(&x, 1, rate, T0)).await;
            assert_spans_continuous(&run.frames);
            let n: usize = run.frames.iter().map(|f| audio(f).2.len()).sum();
            assert!(
                n.abs_diff(bits.len()) * 100 < bits.len(),
                "{algorithm}: {n} symbols from {} bits",
                bits.len()
            );
        }
    }

    #[tokio::test]
    async fn a_ragged_two_channel_frame_is_refused_naming_the_count() {
        let rate = 4 * BAUD as u64;
        let input = vec![Frame::new(
            zeta_core::types::StreamId::new(),
            prov(T0, 2, rate),
            Payload::Audio {
                sample_rate_hz: Hz(rate),
                channels: 2,
                samples: Arc::from(vec![0.0f32; 5]),
            },
        )];
        match try_run_clock_frames(serde_json::json!({"symbol_rate_baud": BAUD}), input).await {
            Err(StageError::Failed(m)) => assert!(
                m.contains("5 samples is not a whole number of 2-channel frames"),
                "{m}"
            ),
            r => panic!("expected Failed, got {:?}", r.map(|r| r.frames.len())),
        }
    }

    #[tokio::test]
    async fn noise_alone_is_searching() {
        let rate = 8 * BAUD as u64;
        let mut x = vec![0.0f32; 2 * rate as usize];
        awgn_real(&mut x, 0.0, 13);
        let (_, events, _) = run_clock(
            serde_json::json!({"symbol_rate_baud": BAUD}),
            frames(&x, 1, rate, T0),
        )
        .await;
        assert!(!events.is_empty());
        for e in &events {
            assert_eq!(e["locked"], false, "{e}");
            assert!(e["summary"].as_str().unwrap().ends_with("searching"));
        }
    }

    #[test]
    fn under_two_samples_per_symbol_is_refused_naming_the_rates() {
        match samples_per_symbol(9_000, 4_800) {
            Err(StageError::Failed(m)) => assert!(
                m.contains("9000 Hz is 1.9 samples per symbol at 4800 Bd"),
                "{m}"
            ),
            r => panic!("expected Failed, got {r:?}"),
        }
        assert_eq!(samples_per_symbol(9_600, 4_800).unwrap(), 2.0);
    }

    #[test]
    fn params_are_checked_naming_the_field() {
        let st = ClockRecoverStage::new();
        let check = |v: Value| st.check_params(&v);
        assert_eq!(
            check(serde_json::json!({"symbol_rate_baud": 0})),
            Err("symbol_rate_baud: at least 1".to_string())
        );
        assert_eq!(
            check(serde_json::json!({"symbol_rate_baud": 1, "loop_bw": 0.0})),
            Err("loop_bw: 0 < bw <= 0.5".to_string())
        );
        assert_eq!(
            check(serde_json::json!({"symbol_rate_baud": 1, "loop_bw": 0.6})),
            Err("loop_bw: 0 < bw <= 0.5".to_string())
        );
        assert_eq!(
            check(serde_json::json!({"symbol_rate_baud": 1, "max_ppm": 100_001})),
            Err("max_ppm: at most 100000".to_string())
        );
        assert!(check(serde_json::json!({"symbol_rate_baud": 1, "x": 1})).is_err());
        assert!(check(serde_json::json!({"symbol_rate_baud": 1, "algorithm": "gardner"})).is_ok());
    }
}
