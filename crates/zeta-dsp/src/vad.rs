//! Voice-activity gate: frame energy over a tracked noise floor.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, StreamId, StreamKind, Timestamp};

use crate::{Input, frame_span, run_map};

/// Detector frame length.
const FRAME_MS: u64 = 20;
/// How far the noise floor may climb per frame (25 dB/s), so a steady carrier
/// is eventually absorbed into the floor.
const FLOOR_RISE_DB: f32 = 0.5;
/// Digital silence: a frame with mean square below this (-80 dB) is quiet for
/// the gate and neither seeds nor lowers the noise floor, so a squelch gap of
/// exact zeros cannot pull the floor under the channel noise that follows it.
const SILENCE_MEAN_SQUARE: f64 = 1e-8;

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

/// Streaming energy detector. Its output lags the input by `min_speech_ms`
/// (plus the open frame): when that much continuous energy has been seen the
/// held-back onset is relabelled, so a speech run starts at its first loud
/// frame, not at the declaration. `feed` returns segments tiling the samples
/// released so far, in order; `flush` releases what is still held. A run
/// lasts from the first loud frame to `hangover_ms` after the last, so
/// utterances closer together than the hangover merge into one run. The first
/// non-silent frame seeds the noise floor, so a stream should open on noise.
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
    /// Labelled input not yet released, and its sample count.
    held: VecDeque<Segment>,
    held_n: usize,
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
            held: VecDeque::new(),
            held_n: 0,
        }
    }

    /// Classify `interleaved` samples; returns the segments now released (the
    /// input delayed by the look-back), tiling them exactly.
    pub fn feed(&mut self, interleaved: &[f32]) -> Vec<Segment> {
        let n = self.channels as f32;
        let mut run = 0;
        for s in interleaved.chunks_exact(self.channels) {
            let m = s.iter().sum::<f32>() / n;
            self.acc += (m as f64) * (m as f64);
            self.acc_n += 1;
            run += 1;
            if self.acc_n == self.frame_len {
                self.hold(self.speech, run);
                run = 0;
                self.end_frame();
            }
        }
        self.hold(self.speech, run);
        // Keep the onset frames (all complete) plus the open frame.
        self.release(self.min_frames as usize * self.frame_len + self.acc_n)
    }

    /// Release every held sample with its current label (end of input).
    pub fn flush(&mut self) -> Vec<Segment> {
        self.release(0)
    }

    fn end_frame(&mut self) {
        let mean_square = self.acc / self.acc_n as f64;
        self.acc = 0.0;
        self.acc_n = 0;
        let loud = if mean_square < SILENCE_MEAN_SQUARE {
            false
        } else {
            let db = (10.0 * mean_square.log10()) as f32;
            // Down at once, up slowly; the first frame seeds the floor.
            let floor = match self.floor_db {
                Some(f) => db.min(f + FLOOR_RISE_DB),
                None => db,
            };
            self.floor_db = Some(floor);
            db > floor + self.threshold_db
        };
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
                self.relabel_onset();
            }
        }
    }

    /// Label the last `min_frames` frames (the onset) as speech.
    fn relabel_onset(&mut self) {
        let mut want = self.min_frames as usize * self.frame_len;
        let mut moved = 0;
        while want > 0 {
            let Some(mut r) = self.held.pop_back() else {
                break;
            };
            if r.frames > want {
                self.held.push_back(Segment {
                    speech: r.speech,
                    frames: r.frames - want,
                });
                r.frames = want;
            }
            want -= r.frames;
            moved += r.frames;
        }
        self.held_n -= moved;
        self.hold(true, moved);
    }

    /// Append a labelled run to the held samples, merging equal neighbours.
    fn hold(&mut self, speech: bool, frames: usize) {
        if frames == 0 {
            return;
        }
        self.held_n += frames;
        match self.held.back_mut() {
            Some(l) if l.speech == speech => l.frames += frames,
            _ => self.held.push_back(Segment { speech, frames }),
        }
    }

    /// Release the oldest held samples, leaving `keep` of them.
    fn release(&mut self, keep: usize) -> Vec<Segment> {
        let mut out: Vec<Segment> = Vec::new();
        let mut take = self.held_n.saturating_sub(keep);
        self.held_n -= take;
        while take > 0 {
            let Some(mut r) = self.held.pop_front() else {
                break;
            };
            if r.frames > take {
                self.held.push_front(Segment {
                    speech: r.speech,
                    frames: r.frames - take,
                });
                r.frames = take;
            }
            take -= r.frames;
            match out.last_mut() {
                Some(l) if l.speech == r.speech => l.frames += r.frames,
                _ => out.push(r),
            }
        }
        out
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

/// The stage's streaming state. The detector releases labels for input older
/// than its look-back, so the samples they describe wait here in `pending`,
/// whose first sample is at `pending_t0`.
struct State {
    audio_out: StreamId,
    /// Absent when launch gave the stage no event output; events are then
    /// not published.
    event_out: Option<StreamId>,
    /// (rate, channels, detector), fixed by the first frame.
    run: Option<(u64, u16, Vad)>,
    pending: Vec<f32>,
    pending_t0: Timestamp,
    prov: Option<Provenance>,
    /// Last speech state announced by an event.
    speaking: bool,
}

impl State {
    /// Turn released segments into gated audio frames and transition events,
    /// consuming the matching samples from `pending`.
    fn emit(&mut self, segs: Vec<Segment>, rate: u64, ch: u16) -> Vec<Frame> {
        let Some(base) = self.prov.clone() else {
            return Vec::new();
        };
        let width = ch as usize;
        let mut out = Vec::new();
        let mut offset = 0usize;
        for seg in segs {
            let start = frame_span(self.pending_t0, offset, rate).end_ns;
            if seg.speech != self.speaking {
                self.speaking = seg.speech;
                if let Some(ev) = &self.event_out {
                    let mut prov = base.clone();
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
                let mut prov = base.clone();
                prov.span = frame_span(start, seg.frames, rate);
                out.push(Frame::new(
                    self.audio_out.clone(),
                    prov,
                    Payload::Audio {
                        sample_rate_hz: Hz(rate),
                        channels: ch,
                        samples: self.pending[offset * width..(offset + seg.frames) * width].into(),
                    },
                ));
            }
            offset += seg.frames;
        }
        self.pending_t0 = frame_span(self.pending_t0, offset, rate).end_ns;
        self.pending.drain(..offset * width);
        out
    }

    fn process(&mut self, frame: &Frame, params: &VadParams) -> Result<Vec<Frame>, StageError> {
        let Payload::Audio {
            sample_rate_hz,
            channels,
            samples,
        } = &frame.payload
        else {
            return Err(StageError::Failed("vad expects audio frames".into()));
        };
        let (rate, ch) = (sample_rate_hz.0, *channels);
        match &self.run {
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
            Some(_) => {}
            None => {
                if rate == 0 || ch == 0 {
                    return Err(StageError::Failed(
                        "vad: sample rate and channels must be >= 1".into(),
                    ));
                }
                self.run = Some((rate, ch, Vad::new(rate, ch, params)));
            }
        }
        if self.pending.is_empty() {
            self.pending_t0 = frame.provenance.span.start_ns;
        }
        self.pending.extend_from_slice(samples);
        self.prov = Some(frame.provenance.clone());
        let segs = match &mut self.run {
            Some((_, _, vad)) => vad.feed(samples),
            None => Vec::new(),
        };
        Ok(self.emit(segs, rate, ch))
    }

    /// Release what the detector still holds (the input closed).
    fn flush(&mut self) -> Vec<Frame> {
        let Some((rate, ch, mut vad)) = self.run.take() else {
            return Vec::new();
        };
        let segs = vad.flush();
        self.emit(segs, rate, ch)
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
        let state = Arc::new(Mutex::new(State {
            audio_out,
            event_out: outputs.get(1).cloned(),
            run: None,
            pending: Vec::new(),
            pending_t0: Timestamp(0),
            prov: None,
            speaking: false,
        }));
        let st = state.clone();
        run_map(inputs, bus.clone(), cancel, move |input| {
            let mut st = st.lock().expect("vad state");
            match input {
                Input::Frame(f) => st.process(&f, &params),
                Input::Lagged { .. } => {
                    st.run = None;
                    st.pending.clear();
                    Ok(Vec::new())
                }
            }
        })
        .await?;
        let tail = state.lock().expect("vad state").flush();
        for f in tail {
            bus.publish(f);
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{awgn_real, speech_like, tone};

    fn params() -> VadParams {
        serde_json::from_value(serde_json::json!({})).unwrap()
    }

    /// Hangover shorter than the fixture's 300 ms gaps, so bursts stay apart.
    fn short_hangover() -> VadParams {
        serde_json::from_value(serde_json::json!({"hangover_ms": 200})).unwrap()
    }

    /// Whole-stream segments of `x`, fed in 4800-frame pieces then flushed.
    fn segments_with(p: &VadParams, rate: u32, channels: u16, x: &[f32]) -> Vec<Segment> {
        let mut v = Vad::new(rate as u64, channels, p);
        let mut out: Vec<Segment> = Vec::new();
        let mut add = |segs: Vec<Segment>| {
            for s in segs {
                match out.last_mut() {
                    Some(l) if l.speech == s.speech => l.frames += s.frames,
                    _ => out.push(s),
                }
            }
        };
        for c in x.chunks(4_800 * channels as usize) {
            add(v.feed(c));
        }
        add(v.flush());
        out
    }

    fn segments(rate: u32, channels: u16, x: &[f32]) -> Vec<Segment> {
        segments_with(&params(), rate, channels, x)
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
        let segs = segments_with(&short_hangover(), rate, 1, &x);
        assert_eq!(segs.iter().map(|s| s.frames).sum::<usize>(), x.len());
        let speech: Vec<usize> = segs.iter().filter(|s| s.speech).map(|s| s.frames).collect();
        assert_eq!(speech.len(), 10, "{segs:?}");
        // A run is the 300 ms burst plus the 200 ms hangover, to a frame.
        let want = rate as usize * 500 / 1000;
        let frame = rate as usize * 20 / 1000;
        for (i, w) in speech.iter().enumerate() {
            assert!(w.abs_diff(want) <= frame, "burst {i}: {w} vs {want}");
        }
        // The first run starts at the first burst (100 ms in), within a frame.
        let first = segs
            .iter()
            .take_while(|s| !s.speech)
            .map(|s| s.frames)
            .sum::<usize>();
        assert!(
            first.abs_diff(rate as usize / 10) <= frame,
            "first run at {first}"
        );
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
    fn digital_silence_gap_does_not_lower_the_floor() {
        let rate = 48_000usize;
        // Channel noise at about -50 dB, a second of exact zeros, the noise again.
        let mut x = vec![0.0f32; 3 * rate];
        awgn_real(&mut x, 50.0, 11);
        x[rate..2 * rate].fill(0.0);
        let segs = segments(rate as u32, 1, &x);
        assert!(segs.iter().all(|s| !s.speech), "{segs:?}");
    }

    #[test]
    fn odd_rate_works() {
        let rate = 53_333u32;
        let x = noisy_speech(rate, 6.0);
        let segs = segments_with(&short_hangover(), rate, 1, &x);
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
