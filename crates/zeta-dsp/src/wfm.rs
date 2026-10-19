//! Wideband (broadcast) FM demodulator stage: phase discriminator, de-emphasis,
//! 15 kHz audio low-pass, then resampling to the audio rate.

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

use crate::nbfm::{AudioFilter, nbfm_demod};
use crate::util::{StreamingFir, lowpass_taps};
use crate::{Input, frame_span, run_map};

/// Cutoff of the mono audio low-pass, Hz; above it sit the 19 kHz pilot and
/// the stereo and RDS subcarriers.
pub const WFM_AUDIO_CUTOFF_HZ: f64 = 15_000.0;
/// Taps of the audio low-pass at the input rate.
pub const WFM_AUDIO_TAPS: usize = 127;

/// FM de-emphasis in place: the single-pole IIR `y[n] = a·x[n] + (1−a)·y[n−1]`
/// with `a = 1 − exp(−1/(rate_hz·τ))`, `τ = tau_us` µs. `state` is `y[−1]` on
/// entry and is left holding the last output.
pub fn deemphasis(x: &mut [f32], rate_hz: u64, tau_us: u64, state: &mut f32) {
    let a = (1.0 - (-1.0 / (rate_hz as f64 * tau_us as f64 * 1e-6)).exp()) as f32;
    let mut y = *state;
    for s in x.iter_mut() {
        y = a * *s + (1.0 - a) * y;
        *s = y;
    }
    *state = y;
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct WfmParams {
    /// Peak deviation that maps to ±1.0, Hz (75 000 for broadcast FM).
    pub deviation_hz: u64,
    pub audio_rate_hz: u64,
    /// De-emphasis time constant, µs: 50 (Australia, Europe) or 75 (Americas).
    pub deemphasis_us: u64,
}

/// Streaming WFM demodulator: per block, discriminator at `deviation_hz`,
/// de-emphasis and a 15 kHz low-pass at the input rate, then a resampler to
/// `audio_rate_hz`; output clamped to ±1.0. State carries across blocks; the
/// low-pass and resampler are rebuilt when the input rate changes.
#[derive(Debug)]
pub struct WfmDemod {
    p: WfmParams,
    prev: Complex<f32>,
    deemph: f32,
    lpf: Option<(u64, StreamingFir)>,
    audio: AudioFilter,
}

impl WfmDemod {
    /// Checks `p`: `deviation_hz` and `audio_rate_hz` non-zero, `deemphasis_us`
    /// 50 or 75; the error names the parameter.
    pub fn new(p: &WfmParams) -> Result<Self, String> {
        if p.deviation_hz == 0 {
            return Err("wfm_demod params: deviation_hz must be >= 1".into());
        }
        if p.audio_rate_hz == 0 {
            return Err("wfm_demod params: audio_rate_hz must be >= 1".into());
        }
        if !matches!(p.deemphasis_us, 50 | 75) {
            return Err(format!(
                "wfm_demod params: deemphasis_us must be 50 or 75, got {}",
                p.deemphasis_us
            ));
        }
        Ok(Self {
            p: *p,
            prev: Complex::new(0.0, 0.0),
            deemph: 0.0,
            lpf: None,
            audio: AudioFilter::default(),
        })
    }

    /// Demodulate one block of `iq` at `rate_hz` to audio at `audio_rate_hz`.
    pub fn process(&mut self, iq: &[Complex<f32>], rate_hz: u64) -> Result<Vec<f32>, String> {
        if rate_hz == 0 {
            return Err("wfm_demod: input sample rate is 0".into());
        }
        let mut x = nbfm_demod(iq, rate_hz, self.p.deviation_hz, &mut self.prev);
        deemphasis(&mut x, rate_hz, self.p.deemphasis_us, &mut self.deemph);
        if self.lpf.as_ref().is_none_or(|(r, _)| *r != rate_hz) {
            // Above 2 × 15 kHz the cutoff stays put; below, the resampler's
            // own low-pass is what limits the band.
            let cutoff = WFM_AUDIO_CUTOFF_HZ.min(0.45 * rate_hz as f64);
            let taps = lowpass_taps(cutoff, rate_hz as f64, WFM_AUDIO_TAPS);
            self.lpf = Some((rate_hz, StreamingFir::new(taps)));
        }
        let x = self
            .lpf
            .as_mut()
            .map_or_else(Vec::new, |(_, f)| f.process(&x));
        let mut y = self.audio.process(&x, rate_hz, self.p.audio_rate_hz)?;
        for s in &mut y {
            *s = s.clamp(-1.0, 1.0);
        }
        Ok(y)
    }
}

/// Demodulates a wideband FM IQ stream to de-emphasised mono audio at
/// `audio_rate_hz`.
pub struct WfmDemodStage {
    spec: StageSpec,
}

impl WfmDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "wfm_demod",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(WfmParams),
            },
        }
    }
}

impl Default for WfmDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for WfmDemodStage {
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
        let p: WfmParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("wfm_demod params: {e}")))?;
        let mut demod = WfmDemod::new(&p).map_err(StageError::Failed)?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        run_map(inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    demod = WfmDemod::new(&p).map_err(StageError::Failed)?;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(
                    "wfm_demod stage expects iq frames".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wfm_stage_rejects_bad_deemphasis_naming_the_param() {
        let bus = Arc::new(StreamBus::new(4));
        let (_tx, rx) = broadcast::channel(4);
        let r = WfmDemodStage::new()
            .run(
                serde_json::json!({"deviation_hz": 75_000, "audio_rate_hz": 48_000, "deemphasis_us": 60}),
                vec![rx],
                vec![StreamId::new()],
                bus,
                CancellationToken::new(),
            )
            .await;
        match r {
            Err(StageError::Failed(m)) => assert!(m.contains("deemphasis_us"), "{m}"),
            r => panic!("expected Failed, got {r:?}"),
        }
    }
}
