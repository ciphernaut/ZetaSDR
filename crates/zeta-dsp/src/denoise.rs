//! Denoise stage: RNNoise (the pure-Rust `nnnoiseless` crate) on audio.

use std::sync::Arc;

use async_trait::async_trait;
use nnnoiseless::DenoiseState;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

use crate::resample::Resampler;
use crate::{Input, frame_span, run_map};

/// RNNoise works at this sample rate only.
pub const DENOISE_RATE_HZ: u64 = 48_000;
/// RNNoise frame length in samples (10 ms at 48 kHz).
pub const DENOISE_FRAME: usize = 480;

/// RNNoise works on samples scaled to 16-bit full scale.
const RNNOISE_SCALE: f32 = 32_768.0;

/// Streaming RNNoise: any input rate and channel count in, mono 48 kHz out.
pub struct Denoiser {
    channels: usize,
    resampler: Option<Resampler>,
    state: Box<DenoiseState<'static>>,
    /// Mono 48 kHz samples not yet forming a whole frame.
    pending: Vec<f32>,
}

impl Denoiser {
    /// A denoiser for interleaved input at `in_rate_hz` with `channels` channels.
    pub fn new(in_rate_hz: u64, channels: u16) -> Result<Self, String> {
        if channels == 0 {
            return Err("channels must be >= 1".into());
        }
        let resampler = if in_rate_hz == DENOISE_RATE_HZ {
            None
        } else {
            Some(Resampler::new(in_rate_hz, DENOISE_RATE_HZ)?)
        };
        Ok(Self {
            channels: channels as usize,
            resampler,
            state: DenoiseState::new(),
            pending: Vec::new(),
        })
    }

    /// Denoise `interleaved` samples; returns every whole 480-sample frame now
    /// available (mono, 48 kHz), keeping the remainder for the next call.
    pub fn feed(&mut self, interleaved: &[f32]) -> Vec<f32> {
        let mono: Vec<f32> = if self.channels == 1 {
            interleaved.to_vec()
        } else {
            let n = self.channels as f32;
            interleaved
                .chunks_exact(self.channels)
                .map(|c| c.iter().sum::<f32>() / n)
                .collect()
        };
        match &mut self.resampler {
            Some(r) => self.pending.extend(r.process(&mono)),
            None => self.pending.extend(mono),
        }
        let whole = self.pending.len() / DENOISE_FRAME * DENOISE_FRAME;
        let mut out = Vec::with_capacity(whole);
        let mut input = [0.0f32; DENOISE_FRAME];
        let mut frame = [0.0f32; DENOISE_FRAME];
        for chunk in self.pending[..whole].chunks_exact(DENOISE_FRAME) {
            for (i, s) in input.iter_mut().zip(chunk) {
                *i = s * RNNOISE_SCALE;
            }
            self.state.process_frame(&mut frame, &input);
            out.extend(frame.iter().map(|s| s / RNNOISE_SCALE));
        }
        self.pending.drain(..whole);
        out
    }
}

/// `denoise` takes no parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct DenoiseParams {}

/// Removes broadband noise from an audio stream; output is mono 48 kHz.
pub struct DenoiseStage {
    spec: StageSpec,
}

impl DenoiseStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "denoise",
                inputs: vec![StreamKind::Audio],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(DenoiseParams),
            },
        }
    }
}

impl Default for DenoiseStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for DenoiseStage {
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
        let _: DenoiseParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("denoise params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        // Timing comes from the first frame: (rate, channels, denoiser).
        let mut run: Option<(u64, u16, Denoiser)> = None;
        run_map(inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    run = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &frame.payload
            else {
                return Err(StageError::Failed("denoise expects audio frames".into()));
            };
            let (rate, ch) = (sample_rate_hz.0, *channels);
            let (_, _, dn) = match &mut run {
                Some((r, _, _)) if *r != rate => {
                    return Err(StageError::Failed(format!(
                        "sample_rate_hz changed from {r} to {rate}"
                    )));
                }
                Some((_, c, _)) if *c != ch => {
                    return Err(StageError::Failed(format!(
                        "channels changed from {c} to {ch}"
                    )));
                }
                Some(r) => r,
                None => {
                    let dn = Denoiser::new(rate, ch)
                        .map_err(|e| StageError::Failed(format!("denoise: {e}")))?;
                    run.insert((rate, ch, dn))
                }
            };
            let cleaned = dn.feed(samples);
            if cleaned.is_empty() {
                return Ok(Vec::new());
            }
            let mut prov = frame.provenance.clone();
            prov.span = frame_span(prov.span.start_ns, cleaned.len(), DENOISE_RATE_HZ);
            Ok(vec![Frame::new(
                out.clone(),
                prov,
                Payload::Audio {
                    sample_rate_hz: Hz(DENOISE_RATE_HZ),
                    channels: 1,
                    samples: cleaned.into(),
                },
            )])
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{awgn_real, speech_like, tone};

    /// Mean power in burst windows over mean power in silence windows, dB.
    /// Windows are the inner 200 ms of each 300 ms burst / silence of the 600 ms period.
    fn burst_to_silence_db(x: &[f32]) -> f64 {
        let rate = 48_000usize;
        let (period, win, margin) = (rate * 600 / 1000, rate * 200 / 1000, rate * 50 / 1000);
        let (mut pb, mut nb, mut ps, mut ns) = (0.0f64, 0usize, 0.0f64, 0usize);
        let mut start = 0;
        while start + period <= x.len() {
            for &s in &x[start + margin..start + margin + win] {
                pb += (s as f64).powi(2);
                nb += 1;
            }
            let sil = start + rate * 300 / 1000 + margin;
            for &s in &x[sil..sil + win] {
                ps += (s as f64).powi(2);
                ns += 1;
            }
            start += period;
        }
        10.0 * ((pb / nb as f64) / (ps / ns as f64)).log10()
    }

    #[test]
    fn denoise_raises_snr_of_speech_like_in_white_noise() {
        let clean = speech_like(48_000, 6.0);
        // Noise 10 dB below the burst power; the fixture's silence would lower the mean.
        let sq = |s: &f32| (*s as f64).powi(2);
        let burst_power =
            clean.iter().map(sq).sum::<f64>() / clean.iter().filter(|&&s| s != 0.0).count() as f64;
        let mean_power = clean.iter().map(sq).sum::<f64>() / clean.len() as f64;
        let snr_db = 10.0 + 10.0 * (burst_power / mean_power).log10();
        let mut noisy = clean.clone();
        awgn_real(&mut noisy, snr_db as f32, 7);
        let before = burst_to_silence_db(&noisy);
        let out = Denoiser::new(48_000, 1).unwrap().feed(&noisy);
        let after = burst_to_silence_db(&out);
        assert!(
            after - before >= 6.0,
            "burst/silence ratio {before:.1} dB -> {after:.1} dB, improvement {:.1} dB",
            after - before
        );
    }

    #[test]
    fn silence_stays_silent() {
        let out = Denoiser::new(48_000, 1).unwrap().feed(&vec![0.0; 48_000]);
        assert_eq!(out.len(), 48_000);
        assert!(out.iter().all(|s| s.abs() < 1e-3));
    }

    #[test]
    fn odd_rate_input_is_resampled_to_48k() {
        let x = tone(53_333, 53_333, 440.0, 0.3);
        let mut d = Denoiser::new(53_333, 1).unwrap();
        let out = d.feed(&x);
        let err = (out.len() as f64 - 48_000.0).abs() / 48_000.0;
        assert!(err < 0.01, "got {} samples", out.len());
    }

    #[test]
    fn stereo_is_downmixed() {
        let mono = speech_like(48_000, 1.2);
        let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
        let a = Denoiser::new(48_000, 1).unwrap().feed(&mono);
        let b = Denoiser::new(48_000, 2).unwrap().feed(&stereo);
        assert_eq!(a, b);
    }

    #[test]
    fn partial_frames_carry_across_feeds() {
        let x = speech_like(48_000, 1.2);
        let whole = Denoiser::new(48_000, 1).unwrap().feed(&x);
        let mut d = Denoiser::new(48_000, 1).unwrap();
        let mut parts = Vec::new();
        for c in x.chunks(1000) {
            parts.extend(d.feed(c));
        }
        let full = x.len() / DENOISE_FRAME * DENOISE_FRAME;
        assert_eq!(whole.len(), full);
        assert_eq!(parts.len(), full);
        assert_eq!(whole, parts);
    }

    #[test]
    fn rejects_zero_rate_and_channels() {
        assert!(Denoiser::new(0, 1).is_err());
        assert!(Denoiser::new(48_000, 0).is_err());
    }
}
