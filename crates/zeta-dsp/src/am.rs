//! AM demodulator stage: envelope (magnitude), DC removal, 5 kHz audio
//! low-pass, resampling to the audio rate and a slow AGC. The DC-removal and
//! AGC helpers are shared with the SSB demodulator.

use std::f64::consts::PI;
use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

use crate::nbfm::AudioFilter;
use crate::util::{StreamingFir, lowpass_taps};
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
/// clamped to ±1.
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
            smooth: 0.0,
            ms: 0.0,
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

#[derive(Clone, Copy, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct AmParams {
    pub audio_rate_hz: u64,
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

    /// Demodulate one block of `iq` at `rate_hz` to audio at `audio_rate_hz`.
    pub fn process(&mut self, iq: &[Complex<f32>], rate_hz: u64) -> Result<Vec<f32>, String> {
        if rate_hz == 0 {
            return Err("am_demod: input sample rate is 0".into());
        }
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

/// Demodulates an AM IQ stream to audio at `audio_rate_hz`.
pub struct AmDemodStage {
    spec: StageSpec,
}

impl AmDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "am_demod",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
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

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
    ) -> Result<(), StageError> {
        let p: AmParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("am_demod params: {e}")))?;
        let mut demod = AmDemod::new(&p).map_err(StageError::Failed)?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        run_map(inputs, bus, cancel, move |input| {
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
                    samples: samples.into(),
                },
            )])
        })
        .await
    }
}
