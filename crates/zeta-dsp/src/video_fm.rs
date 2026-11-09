//! Analog television timing and the CCIR 405 pre/de-emphasis shelf shared by the
//! synthesised test card ([`crate::synth`]) and the video stages; the `video_fm_demod`
//! stage.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

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

/// FM discriminator: `arg(x · conj(prev)) · rate / (2π · deviation)` per sample,
/// unclamped (a sync tip below the nominal deviation must keep its depth); a
/// non-finite result gives 0. `prev` carries the last sample across blocks.
fn discriminate(
    iq: &[Complex<f32>],
    prev: &mut Complex<f32>,
    rate_hz: u64,
    deviation_hz: u64,
) -> Vec<f32> {
    let scale = rate_hz as f64 / (std::f64::consts::TAU * deviation_hz as f64);
    iq.iter()
        .map(|&x| {
            let v = (f64::from((x * prev.conj()).arg()) * scale) as f32;
            *prev = x;
            if v.is_finite() { v } else { 0.0 }
        })
        .collect()
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
/// decimation to [`composite_rate_hz`] (none at decimation 1), then de-emphasis. Output
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
        let decimator = (decim > 1).then(|| {
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
            if noted != Some(comp) {
                noted = Some(comp);
                ctx.note(json!({"video_fm": {"composite_rate_hz": comp}}));
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
        run_map_threaded(
            kind,
            inputs,
            bus,
            cancel,
            VIDEO_QUEUE_DEPTH,
            Arc::new(AtomicU64::new(0)),
            f,
        )
        .await
    }
}
