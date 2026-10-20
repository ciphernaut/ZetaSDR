//! Voice-activity gate: frame energy over a tracked noise floor.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

use crate::{Input, frame_span, run_map};

/// Detector frame length.
const FRAME_MS: u64 = 20;
/// How far the noise floor may climb per frame (25 dB/s), so a steady carrier
/// is eventually absorbed into the floor.
const FLOOR_RISE_DB: f32 = 0.5;
/// Added to the mean square so digital silence is a finite level.
const ENERGY_EPS: f64 = 1e-12;

fn d6() -> f32 {
    6.0
}
fn d300() -> u32 {
    300
}
fn d200() -> u32 {
    200
}

/// `vad` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct VadParams {
    /// How far above the noise floor a frame must be to count as speech, dB.
    #[serde(default = "d6")]
    pub threshold_db: f32,
    /// Speech continues this long after the last frame above the threshold, ms.
    #[serde(default = "d300")]
    pub hangover_ms: u32,
    /// Speech starts after this much continuous time above the threshold, ms.
    #[serde(default = "d200")]
    pub min_speech_ms: u32,
}

/// A run of consecutive input: `frames` samples per channel, speech or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub speech: bool,
    pub frames: usize,
}

/// Streaming energy detector. Every input sample is labelled with the state in
/// effect when it arrives, so segments tile the input exactly; a decision made
/// when a 20 ms frame completes applies from the next sample. Speech therefore
/// begins up to `min_speech_ms` (plus one frame) after the first loud sample.
/// The first frame seeds the noise floor, so a stream should open on noise.
pub struct Vad {
    channels: usize,
    frame_len: usize,
    threshold_db: f32,
    min_frames: u32,
    hang_frames: u32,
    /// Sum of squares of the downmix and sample count of the open frame.
    acc: f64,
    acc_n: usize,
    floor_db: Option<f32>,
    speech: bool,
    /// Consecutive loud frames while silent / quiet frames while speaking.
    loud_run: u32,
    quiet_run: u32,
}

impl Vad {
    pub fn new(rate_hz: u64, channels: u16, p: &VadParams) -> Self {
        let frames_of = |ms: u32| (ms as u64).div_ceil(FRAME_MS) as u32;
        Self {
            channels: channels.max(1) as usize,
            frame_len: ((rate_hz * FRAME_MS / 1000) as usize).max(1),
            threshold_db: p.threshold_db,
            min_frames: frames_of(p.min_speech_ms).max(1),
            hang_frames: frames_of(p.hangover_ms).max(1),
            acc: 0.0,
            acc_n: 0,
            floor_db: None,
            speech: false,
            loud_run: 0,
            quiet_run: 0,
        }
    }

    /// Classify `interleaved` samples; the returned segments tile the input.
    pub fn feed(&mut self, interleaved: &[f32]) -> Vec<Segment> {
        let mut out: Vec<Segment> = Vec::new();
        let n = self.channels as f32;
        let mut run = 0;
        for s in interleaved.chunks_exact(self.channels) {
            let m = s.iter().sum::<f32>() / n;
            self.acc += (m as f64) * (m as f64);
            self.acc_n += 1;
            run += 1;
            if self.acc_n == self.frame_len {
                push(&mut out, self.speech, run);
                run = 0;
                self.end_frame();
            }
        }
        push(&mut out, self.speech, run);
        out
    }

    fn end_frame(&mut self) {
        let db = (10.0 * (self.acc / self.acc_n as f64 + ENERGY_EPS).log10()) as f32;
        self.acc = 0.0;
        self.acc_n = 0;
        // Down at once, up slowly; the first frame seeds the floor.
        let floor = match self.floor_db {
            Some(f) => db.min(f + FLOOR_RISE_DB),
            None => db,
        };
        self.floor_db = Some(floor);
        let loud = db > floor + self.threshold_db;
        if self.speech {
            self.quiet_run = if loud { 0 } else { self.quiet_run + 1 };
            if self.quiet_run >= self.hang_frames {
                self.speech = false;
                self.loud_run = 0;
            }
        } else {
            self.loud_run = if loud { self.loud_run + 1 } else { 0 };
            if self.loud_run >= self.min_frames {
                self.speech = true;
                self.quiet_run = 0;
            }
        }
    }
}

/// Append a run, merging it into the previous one of the same kind.
fn push(out: &mut Vec<Segment>, speech: bool, frames: usize) {
    if frames == 0 {
        return;
    }
    match out.last_mut() {
        Some(l) if l.speech == speech => l.frames += frames,
        _ => out.push(Segment { speech, frames }),
    }
}

/// Forwards audio only while speech is present and publishes an event at each
/// transition.
pub struct VadStage {
    spec: StageSpec,
}

impl VadStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "vad",
                inputs: vec![StreamKind::Audio],
                outputs: vec![StreamKind::Audio, StreamKind::Event],
                params_schema: schemars::schema_for!(VadParams),
            },
        }
    }
}

impl Default for VadStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for VadStage {
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
        let params: VadParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("vad params: {e}")))?;
        let Some(audio_out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 2 outputs".into()));
        };
        // Launch code may hand a stage only the outputs that edges give it, so
        // the event output can be absent; events are then not published.
        let event_out = outputs.get(1).cloned();
        // Timing comes from the first frame: (rate, channels, detector).
        let mut run: Option<(u64, u16, Vad)> = None;
        // Last speech state announced by an event.
        let mut speaking = false;
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
                return Err(StageError::Failed("vad expects audio frames".into()));
            };
            let (rate, ch) = (sample_rate_hz.0, *channels);
            let (_, _, vad) = match &mut run {
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
                    if rate == 0 || ch == 0 {
                        return Err(StageError::Failed(
                            "vad: sample rate and channels must be >= 1".into(),
                        ));
                    }
                    run.insert((rate, ch, Vad::new(rate, ch, &params)))
                }
            };
            let t0 = frame.provenance.span.start_ns;
            let width = ch as usize;
            let mut out = Vec::new();
            let mut offset = 0usize;
            for seg in vad.feed(samples) {
                let start = frame_span(t0, offset, rate).end_ns;
                if seg.speech != speaking {
                    speaking = seg.speech;
                    if let Some(ev) = &event_out {
                        let mut prov = frame.provenance.clone();
                        prov.span = frame_span(start, 0, rate);
                        out.push(Frame::new(
                            ev.clone(),
                            prov,
                            Payload::Event(serde_json::json!({
                                "protocol": "vad",
                                "speech": seg.speech,
                                "t_ns": start.0,
                            })),
                        ));
                    }
                }
                if seg.speech {
                    let mut prov = frame.provenance.clone();
                    prov.span = frame_span(start, seg.frames, rate);
                    out.push(Frame::new(
                        audio_out.clone(),
                        prov,
                        Payload::Audio {
                            sample_rate_hz: Hz(rate),
                            channels: ch,
                            samples: samples[offset * width..(offset + seg.frames) * width].into(),
                        },
                    ));
                }
                offset += seg.frames;
            }
            Ok(out)
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{awgn_real, speech_like, tone};

    fn params() -> VadParams {
        serde_json::from_value(serde_json::json!({})).unwrap()
    }

    fn segments(rate: u32, channels: u16, x: &[f32]) -> Vec<Segment> {
        segments_with(&params(), rate, channels, x)
    }

    /// Whole-stream segments of `x`, fed in 4800-frame pieces.
    fn segments_with(p: &VadParams, rate: u32, channels: u16, x: &[f32]) -> Vec<Segment> {
        let mut v = Vad::new(rate as u64, channels, p);
        let mut out: Vec<Segment> = Vec::new();
        for c in x.chunks(4_800 * channels as usize) {
            for s in v.feed(c) {
                push(&mut out, s.speech, s.frames);
            }
        }
        out
    }

    /// Noise-only lead-in (the first frame seeds the floor), then `speech_like`
    /// bursts, all in white noise 20 dB below the mean power.
    fn noisy_speech(rate: u32, seconds: f32) -> Vec<f32> {
        let mut x = vec![0.0; rate as usize / 10];
        x.extend(speech_like(rate, seconds));
        awgn_real(&mut x, 20.0, 3);
        x
    }

    #[test]
    fn speech_like_bursts_become_segments() {
        let rate = 48_000u32;
        let x = noisy_speech(rate, 6.0);
        let segs = segments(rate, 1, &x);
        assert_eq!(segs.iter().map(|s| s.frames).sum::<usize>(), x.len());
        let speech: Vec<usize> = segs.iter().filter(|s| s.speech).map(|s| s.frames).collect();
        assert_eq!(speech.len(), 10, "{segs:?}");
        let max = (rate as usize * (300 + 300)) / 1000;
        for (i, w) in speech.iter().enumerate() {
            assert!(*w <= max && *w >= rate as usize / 10, "burst {i}: {w}");
        }
        for w in segs.windows(2) {
            assert_ne!(w[0].speech, w[1].speech);
        }
    }

    #[test]
    fn steady_tone_settles_to_silence() {
        let rate = 48_000u32;
        let mut x = vec![0.0; rate as usize / 10];
        x.extend(tone(rate, rate as usize * 6, 1_000.0, 0.5));
        awgn_real(&mut x, 40.0, 5);
        let segs = segments(rate, 1, &x);
        let mut pos = 0;
        for s in &segs {
            if s.speech {
                assert!(pos + s.frames <= 2 * rate as usize, "{segs:?}");
            }
            pos += s.frames;
        }
        assert!(segs.iter().any(|s| s.speech), "tone never detected");
        assert!(!segs.last().unwrap().speech);
    }

    #[test]
    fn silence_and_noise_yield_no_speech() {
        let rate = 48_000u32;
        assert!(
            segments(rate, 1, &vec![0.0; 96_000])
                .iter()
                .all(|s| !s.speech)
        );
        let mut n = vec![0.0f32; 96_000];
        n[0] = 1.0;
        awgn_real(&mut n, 0.0, 9);
        assert!(segments(rate, 1, &n).iter().all(|s| !s.speech));
    }

    #[test]
    fn odd_rate_works() {
        let rate = 53_333u32;
        let x = noisy_speech(rate, 6.0);
        // The 300 ms gaps equal the default hangover, so at this rate frame
        // alignment would decide whether bursts merge; use a shorter one.
        let p = serde_json::from_value(serde_json::json!({"hangover_ms": 200})).unwrap();
        let segs = segments_with(&p, rate, 1, &x);
        assert_eq!(segs.iter().map(|s| s.frames).sum::<usize>(), x.len());
        assert_eq!(segs.iter().filter(|s| s.speech).count(), 10, "{segs:?}");
    }

    #[test]
    fn stereo_forwards_both_channels() {
        let rate = 48_000u32;
        let mono = noisy_speech(rate, 3.0);
        let same: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
        let a = segments(rate, 1, &mono);
        let b = segments(rate, 2, &same);
        // An in-phase stereo pair is classified exactly as its mono source,
        // and the segments count per-channel samples.
        assert_eq!(a, b);
        assert_eq!(b.iter().map(|s| s.frames).sum::<usize>(), mono.len());
        // Opposite-phase channels downmix to silence.
        let anti: Vec<f32> = mono.iter().flat_map(|&s| [s, -s]).collect();
        assert!(segments(rate, 2, &anti).iter().all(|s| !s.speech));
    }
}
