//! Analog television timing and the CCIR 405 pre/de-emphasis shelf shared by the
//! synthesised test card ([`crate::synth`]) and the video stages; the `video_fm_demod`
//! stage.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

use crate::util::lowpass_taps;
use crate::worker::{VIDEO_QUEUE_DEPTH, run_map_threaded};
use crate::{Input, frame_span};

/// Composite video level at sync tip, volts.
pub const SYNC_V: f32 = -0.3;
/// Composite video level at blanking, volts.
pub const BLANK_V: f32 = 0.0;
/// Composite video level at peak white, volts.
pub const WHITE_V: f32 = 0.7;

/// The line standard of a 625/50 (PAL) or 525/60 (NTSC) signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum VideoStandard {
    Pal,
    Ntsc,
}

/// The timing of one standard, in seconds and lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandardTable {
    pub line_s: f64,
    pub sync_s: f64,
    pub active_start_s: f64,
    pub active_s: f64,
    pub lines_per_field: [u32; 2],
    pub eq_lines: f64,
    pub broad_lines: f64,
    pub first_active_line: u32,
    pub active_lines: u32,
    pub luma_lowpass_hz: f64,
    pub preemphasis_crossover_hz: f64,
}

static PAL: StandardTable = StandardTable {
    line_s: 64e-6,
    sync_s: 4.7e-6,
    active_start_s: 10.5e-6,
    active_s: 52e-6,
    lines_per_field: [312, 313],
    eq_lines: 2.5,
    broad_lines: 2.5,
    first_active_line: 23,
    active_lines: 288,
    luma_lowpass_hz: 3.0e6,
    preemphasis_crossover_hz: 1.5e6,
};

static NTSC: StandardTable = StandardTable {
    line_s: 63.556e-6,
    sync_s: 4.7e-6,
    active_start_s: 9.4e-6,
    active_s: 52.6e-6,
    lines_per_field: [262, 263],
    eq_lines: 3.0,
    broad_lines: 3.0,
    first_active_line: 22,
    active_lines: 240,
    luma_lowpass_hz: 2.8e6,
    preemphasis_crossover_hz: 2.1e6,
};

impl VideoStandard {
    pub fn table(self) -> &'static StandardTable {
        match self {
            VideoStandard::Pal => &PAL,
            VideoStandard::Ntsc => &NTSC,
        }
    }
}

/// One first-order section `y[n] = b0 x[n] + b1 x[n-1] - a1 y[n-1]`: coefficients in f64,
/// state in f32.
#[derive(Clone, Copy, Debug)]
struct Section {
    b0: f64,
    b1: f64,
    a1: f64,
    x1: f32,
    y1: f32,
}

impl Section {
    fn new(b0: f64, b1: f64, a1: f64) -> Self {
        Section {
            b0,
            b1,
            a1,
            x1: 0.0,
            y1: 0.0,
        }
    }

    fn run(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let y = (self.b0 * f64::from(*s) + self.b1 * f64::from(self.x1)
                - self.a1 * f64::from(self.y1)) as f32;
            self.x1 = *s;
            self.y1 = y;
            *s = y;
        }
    }
}

/// A first-order shelf (pre-emphasis) and its exact inverse (de-emphasis), bilinear
/// transformed.
#[derive(Clone, Debug)]
pub struct Shelf {
    pre: Section,
    de: Section,
}

impl Shelf {
    /// The CCIR 405 network: 14 dB in all (-7 dB low, +7 dB high), unity at the
    /// standard's crossover. Pre-emphasis `(1 + s t1) / (sqrt5 (1 + s t2))` with
    /// `t1 = sqrt5 / w_c`, `t2 = t1 / 5`; de-emphasis is its inverse.
    pub fn ccir405(standard: VideoStandard, rate_hz: f64) -> Shelf {
        let wc = std::f64::consts::TAU * standard.table().preemphasis_crossover_hz;
        let t1 = 5f64.sqrt() / wc;
        let t2 = t1 / 5.0;
        let k = 2.0 * rate_hz;
        let (n0, n1) = (1.0 + k * t1, 1.0 - k * t1);
        let (d0, d1) = (1.0 + k * t2, 1.0 - k * t2);
        let r5 = 5f64.sqrt();
        Shelf {
            pre: Section::new(n0 / (r5 * d0), n1 / (r5 * d0), d1 / d0),
            de: Section::new(r5 * d0 / n0, r5 * d1 / n0, n1 / n0),
        }
    }

    /// The plain single-pole de-emphasis of `wfm::deemphasis` (`tau_us` microseconds),
    /// and its inverse as the pre-emphasis.
    pub fn one_pole(tau_us: f64, rate_hz: f64) -> Shelf {
        let a = 1.0 - (-1.0 / (rate_hz * tau_us * 1e-6)).exp();
        Shelf {
            pre: Section::new(1.0 / a, -(1.0 - a) / a, 0.0),
            de: Section::new(a, 0.0, -(1.0 - a)),
        }
    }

    /// Pre-emphasise `x` in place.
    pub fn pre(&mut self, x: &mut [f32]) {
        self.pre.run(x);
    }

    /// De-emphasise `x` in place.
    pub fn de(&mut self, x: &mut [f32]) {
        self.de.run(x);
    }

    /// Clear both directions' state.
    pub fn reset(&mut self) {
        for s in [&mut self.pre, &mut self.de] {
            s.x1 = 0.0;
            s.y1 = 0.0;
        }
    }
}

fn default_deviation() -> u64 {
    8_000_000
}

fn default_deemphasis() -> String {
    "ccir405".into()
}

fn default_composite_max() -> u64 {
    16_000_000
}

/// Parameters of `video_fm_demod`.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct VideoFmParams {
    /// Peak deviation that maps to 1 V of composite, Hz.
    #[serde(default = "default_deviation")]
    pub deviation_hz: u64,
    /// De-emphasis: "ccir405" (the CCIR 405 shelf at the standard's crossover), "none",
    /// or a single-pole time constant in microseconds ("0.5").
    #[serde(default = "default_deemphasis")]
    pub deemphasis: String,
    /// Highest composite rate, Hz: the input is decimated by the smallest whole factor
    /// that brings it to or under this.
    #[serde(default = "default_composite_max")]
    pub composite_max_hz: u64,
    /// The standard whose crossover the "ccir405" shelf uses; PAL's when absent.
    #[serde(default)]
    pub standard: Option<VideoStandard>,
}

/// The largest exact divisor rate of `in_rate_hz` at or under `max_hz` (`in_rate_hz`
/// itself when it is already at or under); `None` only for `in_rate_hz == 0`.
pub fn composite_rate_hz(in_rate_hz: u64, max_hz: u64) -> Option<u64> {
    if in_rate_hz == 0 {
        return None;
    }
    let max = max_hz.max(1);
    if in_rate_hz <= max {
        return Some(in_rate_hz);
    }
    let first = in_rate_hz.div_ceil(max);
    (first..=in_rate_hz)
        .find(|d| in_rate_hz.is_multiple_of(*d))
        .map(|d| in_rate_hz / d)
}

/// What `deemphasis` asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Deemphasis {
    None,
    Ccir405(VideoStandard),
    OnePole(f64),
}

impl Deemphasis {
    fn parse(s: &str, standard: Option<VideoStandard>) -> Result<Self, String> {
        match s.trim() {
            "none" => Ok(Deemphasis::None),
            "ccir405" => Ok(Deemphasis::Ccir405(standard.unwrap_or(VideoStandard::Pal))),
            t => match t.parse::<f64>() {
                Ok(tau) if tau.is_finite() && tau > 0.0 => Ok(Deemphasis::OnePole(tau)),
                _ => Err(format!(
                    "video_fm_demod params: deemphasis must be \"ccir405\", \"none\" or a time constant in microseconds, got {s:?}"
                )),
            },
        }
    }

    fn shelf(self, rate_hz: u64) -> Option<Shelf> {
        match self {
            Deemphasis::None => None,
            Deemphasis::Ccir405(st) => Some(Shelf::ccir405(st, rate_hz as f64)),
            Deemphasis::OnePole(tau) => Some(Shelf::one_pole(tau, rate_hz as f64)),
        }
    }
}

/// `atan2(y, x)` in f32 without branches, so a loop of it vectorises: an odd degree-11
/// polynomial for `atan` on `[0, 1]` (worst error about 2e-6 rad; the tests hold it to
/// 1e-5), folded into the right octant. `(0, 0)` and non-finite inputs give NaN or a
/// non-finite value the caller replaces.
#[inline(always)]
fn atan2_fast(y: f32, x: f32) -> f32 {
    let (ax, ay) = (x.abs(), y.abs());
    let a = ax.min(ay) / ax.max(ay);
    let s = a * a;
    let mut r = ((((-0.011_721_2 * s + 0.052_653_32) * s - 0.116_432_87) * s + 0.193_543_46) * s
        - 0.332_623_47)
        * s
        * a
        + 0.999_977_26 * a;
    r = if ay > ax {
        std::f32::consts::FRAC_PI_2 - r
    } else {
        r
    };
    r = if x < 0.0 { std::f32::consts::PI - r } else { r };
    if y < 0.0 { -r } else { r }
}

/// FM discriminator: `arg(x · conj(prev)) · rate / (2π · deviation)` per sample,
/// unclamped (a sync tip below the nominal deviation must keep its depth); a
/// non-finite result (no signal: a zero sample) gives 0. `prev` carries the last sample
/// across blocks. The angle is [`atan2_fast`], so the loop runs a vector of samples at a
/// time.
fn discriminate(
    iq: &[Complex<f32>],
    prev: &mut Complex<f32>,
    rate_hz: u64,
    deviation_hz: u64,
) -> Vec<f32> {
    let scale = (rate_hz as f64 / (std::f64::consts::TAU * deviation_hz as f64)) as f32;
    let one = |x: Complex<f32>, p: Complex<f32>| {
        let z = x * p.conj();
        let v = atan2_fast(z.im, z.re) * scale;
        if v.is_finite() { v } else { 0.0 }
    };
    let Some(&first) = iq.first() else {
        return Vec::new();
    };
    let mut out = vec![0.0f32; iq.len()];
    out[0] = one(first, *prev);
    for (o, w) in out[1..].iter_mut().zip(iq.windows(2)) {
        *o = one(w[1], w[0]);
    }
    *prev = iq[iq.len() - 1];
    out
}

/// A streaming real FIR decimator: carries its last `taps - 1` inputs and the phase of
/// the next kept sample across blocks, and computes only the kept outputs.
#[derive(Clone, Debug)]
struct Decimator {
    taps: Vec<f32>,
    decim: usize,
    history: Vec<f32>,
    /// Index into the next block of the next kept sample.
    phase: usize,
}

impl Decimator {
    fn new(taps: Vec<f32>, decim: usize) -> Self {
        let history = vec![0.0; taps.len() - 1];
        Decimator {
            taps,
            decim,
            history,
            phase: 0,
        }
    }

    fn reset(&mut self) {
        self.history.iter_mut().for_each(|v| *v = 0.0);
        self.phase = 0;
    }

    fn process(&mut self, x: &[f32]) -> Vec<f32> {
        if self.decim == 2 {
            return self.process_halfband(x);
        }
        self.process_general(x)
    }

    /// Any decimation: every tap of every kept output.
    fn process_general(&mut self, x: &[f32]) -> Vec<f32> {
        let h = self.history.len();
        let mut buf = std::mem::take(&mut self.history);
        buf.extend_from_slice(x);
        let mut out = Vec::with_capacity(x.len() / self.decim + 1);
        let mut n = self.phase;
        while n < x.len() {
            // buf[h + n] is x[n]; tap j multiplies x[n - j].
            let end = h + n;
            let acc = self
                .taps
                .iter()
                .zip(buf[end - h..=end].iter().rev())
                .fold(0.0f32, |a, (&t, &v)| a + t * v);
            out.push(acc);
            n += self.decim;
        }
        self.phase = n - x.len();
        self.history = buf.split_off(buf.len() - h);
        out
    }

    /// [`Decimator::process`] by 2 with half-band taps (every other tap from the centre
    /// zero, see [`halfband_taps`]): the window's even taps and its centre only, the
    /// window split into its even and odd samples so each tap is one unit-stride pass
    /// over the outputs (which vectorises), symmetric taps paired.
    fn process_halfband(&mut self, x: &[f32]) -> Vec<f32> {
        let h = self.history.len();
        let n_taps = self.taps.len();
        let centre = n_taps / 2;
        let mut buf = std::mem::take(&mut self.history);
        buf.extend_from_slice(x);
        // Outputs at n = phase, phase + 2, ... < x.len(); output m's window is
        // buf[n..n + n_taps] with n = phase + 2m.
        let first = self.phase;
        let m = if first < x.len() {
            (x.len() - first).div_ceil(2)
        } else {
            0
        };
        let tail = &buf[first..];
        let even: Vec<f32> = tail.iter().step_by(2).copied().collect();
        let odd: Vec<f32> = tail.iter().skip(1).step_by(2).copied().collect();
        let mut out = vec![0.0f32; m];
        if m > 0 {
            // The centre tap sits on an odd offset: odd[m + centre / 2].
            let c = self.taps[centre];
            let oc = &odd[centre / 2..centre / 2 + m];
            for (o, &v) in out.iter_mut().zip(oc) {
                *o = c * v;
            }
            // Even offsets j and n_taps - 1 - j: even[m + j / 2] and even[m + (n_taps - 1 - j) / 2].
            for j in (0..centre).step_by(2) {
                let t = self.taps[j];
                let (a, b) = (j / 2, (n_taps - 1 - j) / 2);
                let (ea, eb) = (&even[a..a + m], &even[b..b + m]);
                for ((o, &u), &v) in out.iter_mut().zip(ea).zip(eb) {
                    *o += t * (u + v);
                }
            }
        }
        let n = first + 2 * m;
        self.phase = n - x.len();
        self.history = buf.split_off(buf.len() - h);
        out
    }
}

/// Taps for decimation by 2: a Hann-windowed sinc cut at a quarter of the rate (the
/// half-band point), `n` taps with `n / 2` odd, so every tap an even distance from the
/// centre but the centre itself is zero (set exactly, not left to rounding).
fn halfband_taps(n: usize) -> Vec<f32> {
    assert!(
        n % 2 == 1 && (n / 2) % 2 == 1,
        "halfband_taps: n / 2 must be odd, got {n}"
    );
    let mut taps = lowpass_taps(0.25, 1.0, n);
    let centre = n / 2;
    for (j, t) in taps.iter_mut().enumerate() {
        if j != centre && j.abs_diff(centre) % 2 == 0 {
            *t = 0.0;
        }
    }
    let sum: f32 = taps.iter().sum();
    taps.iter_mut().for_each(|t| *t /= sum);
    taps
}

/// The input-rate-dependent part of [`VideoFmDemod`], rebuilt on a rate change.
#[derive(Clone, Debug)]
struct RateState {
    in_rate_hz: u64,
    composite_rate_hz: u64,
    /// `None` at decimation 1: the composite is the discriminator output as is.
    decimator: Option<Decimator>,
    shelf: Option<Shelf>,
}

/// Streaming wideband FM video demodulator: discriminator at the input rate, FIR
/// decimation to [`composite_rate_hz`] (none at decimation 1; a half-band filter at
/// decimation 2), then de-emphasis. Output
/// is composite volts (`Δf / deviation_hz`), unclamped.
#[derive(Clone, Debug)]
pub struct VideoFmDemod {
    deviation_hz: u64,
    composite_max_hz: u64,
    deemphasis: Deemphasis,
    prev: Complex<f32>,
    rate: Option<RateState>,
}

impl VideoFmDemod {
    /// Checks `p`: `deviation_hz` and `composite_max_hz` non-zero, `deemphasis`
    /// parsable; the error names the parameter.
    pub fn new(p: &VideoFmParams) -> Result<Self, String> {
        if p.deviation_hz == 0 {
            return Err("video_fm_demod params: deviation_hz must be >= 1".into());
        }
        if p.composite_max_hz == 0 {
            return Err("video_fm_demod params: composite_max_hz must be >= 1".into());
        }
        Ok(VideoFmDemod {
            deviation_hz: p.deviation_hz,
            composite_max_hz: p.composite_max_hz,
            deemphasis: Deemphasis::parse(&p.deemphasis, p.standard)?,
            prev: Complex::new(0.0, 0.0),
            rate: None,
        })
    }

    /// Build the filters for `in_rate_hz` (non-zero) and start from fresh state.
    fn rebuild(&mut self, in_rate_hz: u64) {
        let comp = composite_rate_hz(in_rate_hz, self.composite_max_hz).unwrap_or(in_rate_hz);
        let decim = (in_rate_hz / comp) as usize;
        // By 2 (a 30.72 Msps capture to 15.36 Msps) a half-band filter, whose zero taps
        // the decimator skips; other factors a low-pass at 0.45 of the composite rate.
        let decimator = (decim > 1).then(|| {
            if decim == 2 {
                return Decimator::new(halfband_taps(63), 2);
            }
            let n = (8 * decim + 1).clamp(63, 1025) | 1;
            Decimator::new(
                lowpass_taps(0.45 * comp as f64, in_rate_hz as f64, n),
                decim,
            )
        });
        self.prev = Complex::new(0.0, 0.0);
        self.rate = Some(RateState {
            in_rate_hz,
            composite_rate_hz: comp,
            decimator,
            shelf: self.deemphasis.shelf(comp),
        });
    }

    /// Demodulate one block of `iq` at `rate_hz`: composite volts and their rate. A rate
    /// change rebuilds the decimator and the shelf and starts from fresh state.
    pub fn process(
        &mut self,
        iq: &[Complex<f32>],
        rate_hz: u64,
    ) -> Result<(Vec<f32>, u64), String> {
        if rate_hz == 0 {
            return Err("video_fm_demod: input sample rate is 0".into());
        }
        if self.rate.as_ref().is_none_or(|r| r.in_rate_hz != rate_hz) {
            self.rebuild(rate_hz);
        }
        let d = discriminate(iq, &mut self.prev, rate_hz, self.deviation_hz);
        let Some(st) = self.rate.as_mut() else {
            return Err("video_fm_demod: no rate state".into());
        };
        let mut y = match st.decimator.as_mut() {
            Some(dec) => dec.process(&d),
            None => d,
        };
        if let Some(shelf) = st.shelf.as_mut() {
            shelf.de(&mut y);
        }
        Ok((y, st.composite_rate_hz))
    }

    /// Forget all streaming state (after lost input), keeping the current rate's filters.
    pub fn reset(&mut self) {
        self.prev = Complex::new(0.0, 0.0);
        if let Some(st) = self.rate.as_mut() {
            if let Some(dec) = st.decimator.as_mut() {
                dec.reset();
            }
            if let Some(shelf) = st.shelf.as_mut() {
                shelf.reset();
            }
        }
    }
}

/// When `video_fm_demod` notes its dropped inputs: when the count changed since the last
/// note, at most once per `every`.
#[derive(Clone, Debug)]
pub struct DropNote {
    noted: u64,
    at: Option<Instant>,
    every: Duration,
}

impl DropNote {
    pub fn new(every: Duration) -> Self {
        DropNote {
            noted: 0,
            at: None,
            every,
        }
    }

    /// Whether `dropped` (the count at `now`) is to be noted; true marks it noted.
    pub fn due(&mut self, dropped: u64, now: Instant) -> bool {
        if dropped == self.noted || self.at.is_some_and(|a| now.duration_since(a) < self.every) {
            return false;
        }
        self.mark(dropped, now);
        true
    }

    /// `dropped` was noted at `now` (with something else).
    pub fn mark(&mut self, dropped: u64, now: Instant) {
        self.noted = dropped;
        self.at = Some(now);
    }
}

/// Demodulates a wideband FM video carrier to composite video (an audio-kind stream at
/// the composite rate), with the DSP on a thread of its own behind a drop-on-full queue.
pub struct VideoFmDemodStage {
    spec: StageSpec,
}

impl VideoFmDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "video_fm_demod",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(VideoFmParams),
            },
        }
    }
}

impl Default for VideoFmDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for VideoFmDemodStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    /// The composite is an `audio` stream that is not for ears.
    fn output_roles(&self, _params: &serde_json::Value) -> Vec<Option<&'static str>> {
        vec![Some("composite")]
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: VideoFmParams = serde_json::from_value(params.clone())
            .map_err(|e| format!("video_fm_demod params: {e}"))?;
        VideoFmDemod::new(&p).map(|_| ())
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
        let kind = self.spec().kind;
        let p: VideoFmParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{kind} params: {e}")))?;
        let mut demod = VideoFmDemod::new(&p).map_err(StageError::Failed)?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut noted: Option<u64> = None;
        let mut drops = DropNote::new(Duration::from_secs(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let seen = dropped.clone();
        let mut complained = false;
        let f = move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    demod.reset();
                    return Vec::new();
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                if !std::mem::replace(&mut complained, true) {
                    eprintln!("zeta-dsp: {kind} expects iq frames; dropping others");
                }
                return Vec::new();
            };
            let rate = block.sample_rate_hz.0;
            let (samples, comp) = match demod.process(&block.samples, rate) {
                Ok(r) => r,
                Err(e) => {
                    if !std::mem::replace(&mut complained, true) {
                        eprintln!("zeta-dsp: {e}; dropping the block");
                    }
                    return Vec::new();
                }
            };
            // The rate once per rate, the drops when they grew (at most once a second),
            // always together in the one object.
            let n = seen.load(Ordering::Relaxed);
            let now = Instant::now();
            let rate_new = noted != Some(comp);
            if rate_new {
                noted = Some(comp);
                drops.mark(n, now);
            }
            if rate_new || drops.due(n, now) {
                ctx.note(json!({"video_fm": {"composite_rate_hz": comp, "dropped": n}}));
            }
            if samples.is_empty() {
                return Vec::new();
            }
            let mut prov = frame.provenance.clone();
            prov.span = frame_span(block.t0, block.samples.len(), rate);
            vec![Frame::new(
                out.clone(),
                prov,
                Payload::Audio {
                    sample_rate_hz: Hz(comp),
                    channels: 1,
                    samples: samples.into(),
                },
            )]
        };
        run_map_threaded(kind, inputs, bus, cancel, VIDEO_QUEUE_DEPTH, dropped, f).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fast_angle_is_within_1e_5_rad_everywhere() {
        let mut worst = 0.0f64;
        for k in 0..100_000 {
            let th = -std::f64::consts::PI + std::f64::consts::TAU * k as f64 / 100_000.0;
            for r in [1e-6f64, 0.3, 1.0, 1e4] {
                let (y, x) = ((r * th.sin()) as f32, (r * th.cos()) as f32);
                let want = f64::from(y).atan2(f64::from(x));
                let got = f64::from(atan2_fast(y, x));
                let mut d = (got - want).abs();
                // -π and π are the same angle.
                if d > std::f64::consts::PI {
                    d = std::f64::consts::TAU - d;
                }
                worst = worst.max(d);
            }
        }
        assert!(worst < 1e-5, "worst {worst} rad");
        for (y, x) in [(0.0f32, 1.0f32), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)] {
            assert!((atan2_fast(y, x) - y.atan2(x)).abs() < 1e-6, "({y}, {x})");
        }
    }

    #[test]
    fn a_zero_or_non_finite_sample_discriminates_to_0() {
        let z = Complex::new(0.0f32, 0.0);
        let one = Complex::new(1.0f32, 0.0);
        let nan = Complex::new(f32::NAN, 0.0);
        let mut prev = z;
        let d = discriminate(&[z, one, one, nan, one], &mut prev, 1_000_000, 100_000);
        assert_eq!(d, vec![0.0; 5]);
        assert_eq!(prev, one);
    }

    #[test]
    fn the_half_band_decimator_matches_the_plain_one_on_its_taps() {
        let taps = halfband_taps(63);
        let centre = 31;
        assert!((taps.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        for (j, t) in taps.iter().enumerate() {
            if j != centre && j.abs_diff(centre) % 2 == 0 {
                assert_eq!(*t, 0.0, "tap {j}");
            }
        }
        let x: Vec<f32> = (0..10_007)
            .map(|i| ((i as f32) * 0.37).sin() + ((i as f32) * 1.9).cos() * 0.3)
            .collect();
        // The plain loop on the same taps, fed in the same uneven blocks.
        let mut plain = Decimator::new(taps.clone(), 2);
        let mut half = Decimator::new(taps, 2);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let mut at = 0;
        for len in [1usize, 2, 333, 4_096, 7].iter().cycle() {
            if at >= x.len() {
                break;
            }
            let end = (at + len).min(x.len());
            a.extend(plain.process_general(&x[at..end]));
            b.extend(half.process(&x[at..end]));
            at = end;
        }
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), x.len().div_ceil(2));
        for (u, v) in a.iter().zip(&b) {
            assert!((u - v).abs() < 1e-5, "{u} vs {v}");
        }
    }

    #[test]
    fn drops_are_noted_when_they_change_at_most_once_a_second() {
        let t0 = Instant::now();
        let s = Duration::from_secs(1);
        let mut d = DropNote::new(s);
        assert!(!d.due(0, t0), "nothing dropped yet");
        assert!(d.due(3, t0));
        assert!(!d.due(3, t0 + 2 * s), "unchanged");
        assert!(!d.due(5, t0 + s / 2), "within the second");
        assert!(d.due(5, t0 + s));
        d.mark(9, t0 + 2 * s);
        assert!(!d.due(9, t0 + 4 * s), "noted with the rate");
        assert!(!d.due(10, t0 + 2 * s + s / 4));
        assert!(d.due(10, t0 + 3 * s));
    }
}
