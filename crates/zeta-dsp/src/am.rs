//! AM and SSB demodulator stages. AM: envelope (magnitude), DC removal,
//! 5 kHz audio low-pass, resampling to the audio rate and a slow AGC. SSB:
//! Weaver-style sideband selection (shift the wanted sideband onto 0 Hz,
//! complex low-pass, shift back, real part), resampling and the same AGC.

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

use zeta_core::ops::Sideband;

use crate::nbfm::AudioFilter;
use crate::squelch::{
    default_true, squelch_event, squelch_outputs, squelch_streams, stage_squelch,
};
use crate::util::{StreamingComplexFir, StreamingFir, lowpass_taps, nco};
use crate::{Input, frame_span, run_map};

/// Cutoff of the AM audio low-pass, Hz.
pub const AM_AUDIO_CUTOFF_HZ: f64 = 5_000.0;
/// Taps of the audio low-pass at the input rate.
pub const AM_AUDIO_TAPS: usize = 127;
/// Corner of the DC-removal high-pass, Hz.
pub(crate) const DC_CORNER_HZ: f64 = 20.0;
/// RMS the AGC steers the output to.
pub(crate) const AGC_TARGET_RMS: f32 = 0.2;
/// AGC attack time constant, seconds.
pub(crate) const AGC_ATTACK_S: f64 = 0.010;
/// AGC release time constant, seconds.
pub(crate) const AGC_RELEASE_S: f64 = 0.500;
/// Largest AGC gain.
pub(crate) const AGC_MAX_GAIN: f32 = 1000.0;

/// AM envelope detection: the magnitude of each IQ sample.
pub fn am_demod(iq: &[Complex<f32>]) -> Vec<f32> {
    iq.iter().map(|c| c.norm()).collect()
}

/// Streaming one-pole DC-removal high-pass, `y[n] = x[n] − x[n−1] + r·y[n−1]`
/// with `r = 1 − 2π·20/rate`.
#[derive(Clone, Debug)]
pub(crate) struct DcBlock {
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlock {
    pub(crate) fn new(rate_hz: u64) -> Self {
        Self {
            r: (1.0 - 2.0 * PI * DC_CORNER_HZ / rate_hz as f64) as f32,
            x1: 0.0,
            y1: 0.0,
        }
    }

    pub(crate) fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let y = *s - self.x1 + self.r * self.y1;
            self.x1 = *s;
            self.y1 = y;
            *s = y;
        }
    }
}

/// Slow per-sample AGC: the mean square of its input, smoothed over the attack
/// time, is tracked (attack up, release down; smoothing first keeps the
/// audio's own cycles from biasing the estimate toward its peaks) and the
/// input is scaled by `target / rms`, gain capped at [`AGC_MAX_GAIN`]; output
/// clamped to ±1. The estimate starts at the target level (unity gain), so
/// the first block is not amplified by the maximum gain.
#[derive(Clone, Debug)]
pub(crate) struct Agc {
    attack: f32,
    release: f32,
    smooth: f32,
    ms: f32,
}

impl Agc {
    pub(crate) fn new(rate_hz: u64) -> Self {
        let coef = |tau_s: f64| (1.0 - (-1.0 / (rate_hz as f64 * tau_s)).exp()) as f32;
        Self {
            attack: coef(AGC_ATTACK_S),
            release: coef(AGC_RELEASE_S),
            smooth: AGC_TARGET_RMS * AGC_TARGET_RMS,
            ms: AGC_TARGET_RMS * AGC_TARGET_RMS,
        }
    }

    pub(crate) fn process(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            self.smooth += self.attack * (*s * *s - self.smooth);
            let p = self.smooth;
            let a = if p > self.ms {
                self.attack
            } else {
                self.release
            };
            self.ms += a * (p - self.ms);
            let gain = if self.ms > 0.0 {
                (AGC_TARGET_RMS / self.ms.sqrt()).min(AGC_MAX_GAIN)
            } else {
                AGC_MAX_GAIN
            };
            *s = (*s * gain).clamp(-1.0, 1.0);
        }
    }
}

/// `iq` with every non-finite sample mapped to 0, as `nbfm_demod` does, so
/// one bad sample cannot poison the filter and AGC state of later blocks.
fn finite(iq: &[Complex<f32>]) -> std::borrow::Cow<'_, [Complex<f32>]> {
    if iq.iter().all(|x| x.re.is_finite() && x.im.is_finite()) {
        return iq.into();
    }
    iq.iter()
        .map(|&x| {
            if x.re.is_finite() && x.im.is_finite() {
                x
            } else {
                Complex::new(0.0, 0.0)
            }
        })
        .collect::<Vec<_>>()
        .into()
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct AmParams {
    pub audio_rate_hz: u64,
    /// Fixed squelch threshold, dBFS (-120..=0); `null` tracks the noise floor.
    #[serde(default)]
    pub squelch_db: Option<i64>,
    /// The squelch is on (the default); `false` passes every block.
    #[serde(default = "default_true")]
    pub squelch: bool,
}

/// Streaming AM demodulator. State carries across blocks; the filters are
/// rebuilt when the input rate changes.
#[derive(Debug)]
pub struct AmDemod {
    p: AmParams,
    rate_state: Option<(u64, DcBlock, StreamingFir)>,
    audio: AudioFilter,
    agc: Agc,
}

impl AmDemod {
    /// Checks `p`: `audio_rate_hz` non-zero; the error names the parameter.
    pub fn new(p: &AmParams) -> Result<Self, String> {
        if p.audio_rate_hz == 0 {
            return Err("am_demod params: audio_rate_hz must be >= 1".into());
        }
        Ok(Self {
            p: *p,
            rate_state: None,
            audio: AudioFilter::default(),
            agc: Agc::new(p.audio_rate_hz),
        })
    }

    /// Demodulate one block of `iq` at `rate_hz` to audio at `audio_rate_hz`;
    /// non-finite samples are taken as 0.
    pub fn process(&mut self, iq: &[Complex<f32>], rate_hz: u64) -> Result<Vec<f32>, String> {
        if rate_hz == 0 {
            return Err("am_demod: input sample rate is 0".into());
        }
        let iq = finite(iq);
        let iq = &iq[..];
        if self.rate_state.as_ref().is_none_or(|(r, ..)| *r != rate_hz) {
            let cutoff = AM_AUDIO_CUTOFF_HZ.min(0.45 * rate_hz as f64);
            let taps = lowpass_taps(cutoff, rate_hz as f64, AM_AUDIO_TAPS);
            self.rate_state = Some((rate_hz, DcBlock::new(rate_hz), StreamingFir::new(taps)));
        }
        let mut x = am_demod(iq);
        let Some((_, dc, lpf)) = self.rate_state.as_mut() else {
            return Ok(Vec::new());
        };
        dc.process(&mut x);
        let x = lpf.process(&x);
        let mut y = self.audio.process(&x, rate_hz, self.p.audio_rate_hz)?;
        self.agc.process(&mut y);
        Ok(y)
    }
}

/// Demodulates an AM IQ stream to audio at `audio_rate_hz`, behind the same
/// squelch as `nbfm_demod`: no audio while closed, a `squelch` event per
/// transition (and for the first block) on the second output.
pub struct AmDemodStage {
    spec: StageSpec,
}

impl AmDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "am_demod",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio, StreamKind::Event],
                params_schema: schemars::schema_for!(AmParams),
            },
        }
    }
}

impl Default for AmDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for AmDemodStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    /// No event output with the squelch off: it would stay silent.
    fn outputs_for(&self, params: &serde_json::Value) -> Vec<StreamKind> {
        squelch_outputs(params)
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
        let p: AmParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("am_demod params: {e}")))?;
        let mut demod = AmDemod::new(&p).map_err(StageError::Failed)?;
        let mut squelch =
            stage_squelch("am_demod", p.squelch, p.squelch_db).map_err(StageError::Failed)?;
        let (out, events) = squelch_streams(&outputs, squelch.is_some())?;
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    demod = AmDemod::new(&p).map_err(StageError::Failed)?;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(
                    "am_demod stage expects iq frames".into(),
                ));
            };
            let rate = block.sample_rate_hz.0;
            let mut prov = frame.provenance.clone();
            prov.span = frame_span(block.t0, block.samples.len(), rate);
            let mut published = Vec::new();
            if let (Some(sq), Some(events)) = (squelch.as_mut(), &events) {
                if let Some(open) = sq.feed(&block.samples, rate) {
                    published.push(Frame::new(
                        events.clone(),
                        prov.clone(),
                        Payload::Event(squelch_event(sq, open)),
                    ));
                }
                if !sq.is_open() {
                    return Ok(published);
                }
            }
            let samples = demod
                .process(&block.samples, rate)
                .map_err(StageError::Failed)?;
            if !samples.is_empty() {
                published.push(Frame::new(
                    out.clone(),
                    prov,
                    Payload::Audio {
                        sample_rate_hz: Hz(p.audio_rate_hz),
                        channels: 1,
                        samples: samples.into(),
                    },
                ));
            }
            Ok(published)
        })
        .await
    }
}

/// Narrowest accepted `ssb_demod` audio bandwidth, Hz.
pub const SSB_MIN_BANDWIDTH_HZ: u64 = 500;
/// Widest accepted `ssb_demod` audio bandwidth, Hz.
pub const SSB_MAX_BANDWIDTH_HZ: u64 = 6_000;
/// Target transition width of the SSB sideband filter, Hz; sets its length.
const SSB_TRANSITION_HZ: u64 = 300;
/// Bounds on the SSB sideband filter length, taps.
const SSB_MIN_TAPS: usize = 31;
const SSB_MAX_TAPS: usize = 2_047;

#[derive(Clone, Copy, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SsbParams {
    pub sideband: Sideband,
    pub audio_rate_hz: u64,
    /// Audio bandwidth, Hz (500..=6000).
    pub audio_bandwidth_hz: u64,
}

/// Per-input-rate state of [`SsbDemod`].
#[derive(Debug)]
struct SsbRateState {
    rate_hz: u64,
    lpf: StreamingComplexFir,
    /// Phase of the shift oscillator, cycles in `[0, 1)`, carried across blocks.
    phase: f64,
}

/// Streaming SSB demodulator (Weaver method). The input is shifted by
/// `−bandwidth/2` (USB) or `+bandwidth/2` (LSB) so the wanted sideband is
/// centred on 0 Hz, low-passed at `bandwidth/2` (which removes the other
/// sideband), shifted back by the same oscillator and the real part taken,
/// then resampled to `audio_rate_hz` and levelled by the AGC. State carries
/// across blocks; the filter is rebuilt when the input rate changes.
#[derive(Debug)]
pub struct SsbDemod {
    p: SsbParams,
    rate_state: Option<SsbRateState>,
    audio: AudioFilter,
    agc: Agc,
}

impl SsbDemod {
    /// Checks `p`: `audio_rate_hz` non-zero and `audio_bandwidth_hz` in
    /// 500..=6000; the error names the parameter.
    pub fn new(p: &SsbParams) -> Result<Self, String> {
        if p.audio_rate_hz == 0 {
            return Err("ssb_demod params: audio_rate_hz must be >= 1".into());
        }
        if !(SSB_MIN_BANDWIDTH_HZ..=SSB_MAX_BANDWIDTH_HZ).contains(&p.audio_bandwidth_hz) {
            return Err(format!(
                "ssb_demod params: audio_bandwidth_hz must be {SSB_MIN_BANDWIDTH_HZ}..={SSB_MAX_BANDWIDTH_HZ}, got {}",
                p.audio_bandwidth_hz
            ));
        }
        Ok(Self {
            p: *p,
            rate_state: None,
            audio: AudioFilter::default(),
            agc: Agc::new(p.audio_rate_hz),
        })
    }

    /// Demodulate one block of `iq` at `rate_hz` to audio at `audio_rate_hz`;
    /// non-finite samples are taken as 0.
    pub fn process(&mut self, iq: &[Complex<f32>], rate_hz: u64) -> Result<Vec<f32>, String> {
        if rate_hz == 0 {
            return Err("ssb_demod: input sample rate is 0".into());
        }
        let iq = finite(iq);
        let iq = &iq[..];
        if self
            .rate_state
            .as_ref()
            .is_none_or(|s| s.rate_hz != rate_hz)
        {
            let cutoff = (self.p.audio_bandwidth_hz as f64 / 2.0).min(0.45 * rate_hz as f64);
            let taps =
                ((4 * rate_hz / SSB_TRANSITION_HZ) as usize | 1).clamp(SSB_MIN_TAPS, SSB_MAX_TAPS);
            self.rate_state = Some(SsbRateState {
                rate_hz,
                lpf: StreamingComplexFir::new(lowpass_taps(cutoff, rate_hz as f64, taps)),
                phase: 0.0,
            });
        }
        let Some(st) = self.rate_state.as_mut() else {
            return Ok(Vec::new());
        };
        let half = self.p.audio_bandwidth_hz as f64 / 2.0;
        let shift_hz = match self.p.sideband {
            Sideband::Usb => -half,
            Sideband::Lsb => half,
        };
        let rot = {
            let (s, c) = (2.0 * PI * st.phase).sin_cos();
            Complex::new(c as f32, s as f32)
        };
        let osc: Vec<Complex<f32>> = nco(iq.len(), shift_hz, rate_hz as f64)
            .into_iter()
            .map(|o| o * rot)
            .collect();
        st.phase = (st.phase + iq.len() as f64 * shift_hz / rate_hz as f64).rem_euclid(1.0);
        let centred: Vec<Complex<f32>> = iq.iter().zip(&osc).map(|(x, o)| x * o).collect();
        let filtered = st.lpf.process(&centred);
        let x: Vec<f32> = filtered
            .iter()
            .zip(&osc)
            .map(|(y, o)| (y * o.conj()).re)
            .collect();
        let mut y = self.audio.process(&x, rate_hz, self.p.audio_rate_hz)?;
        self.agc.process(&mut y);
        Ok(y)
    }
}

/// Demodulates an SSB IQ stream (upper or lower sideband) to audio at `audio_rate_hz`.
pub struct SsbDemodStage {
    spec: StageSpec,
}

impl SsbDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "ssb_demod",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(SsbParams),
            },
        }
    }
}

impl Default for SsbDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for SsbDemodStage {
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
        let p: SsbParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("ssb_demod params: {e}")))?;
        let mut demod = SsbDemod::new(&p).map_err(StageError::Failed)?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    demod = SsbDemod::new(&p).map_err(StageError::Failed)?;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(
                    "ssb_demod stage expects iq frames".into(),
                ));
            };
            let rate = block.sample_rate_hz.0;
            let samples = demod
                .process(&block.samples, rate)
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
