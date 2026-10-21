//! The `transcribe` stage: audio cut into utterances at gaps in time, each handed to the
//! node's speech backend, the text published as `transcript` events.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::resample::Resampler;

use crate::backend::{DEFAULT_LANGUAGE, SpeechBackend, SpeechError, Transcript, normalise};
use crate::wav::wav_s16_mono;

/// Why a transcribe job fails on a node started without a speech backend.
pub const NO_BACKEND: &str =
    "transcribe: this node has no speech backend (--stt-socket or --stt-node)";

/// The rate every backend is handed.
const RATE_16K: u64 = 16_000;
/// Default `min_utterance_ms`.
const MIN_UTTERANCE_MS_DEFAULT: u32 = 700;
/// Closed utterances waiting for the backend; one more arriving is dropped.
const QUEUE: usize = 4;
/// The `short` count is noted to the job at most this often.
const NOTE_EVERY: Duration = Duration::from_secs(1);
/// Consecutive backend errors that fail the stage.
const MAX_ERRORS: u32 = 2;

fn en() -> String {
    DEFAULT_LANGUAGE.to_string()
}
fn d300() -> u32 {
    300
}
fn d15() -> u32 {
    15
}
fn d700() -> u32 {
    MIN_UTTERANCE_MS_DEFAULT
}

/// `transcribe` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TranscribeParams {
    /// Language the backend is asked to transcribe (one of its `languages`).
    #[serde(default = "en")]
    pub language: String,
    /// A frame starting more than this after the previous frame's end closes the
    /// utterance, ms; so does this much wall-clock silence after the last frame (`vad`
    /// upstream forwards only speech, so after a transmission no frame may follow).
    #[serde(default = "d300")]
    pub gap_ms: u32,
    /// An utterance is closed when it reaches this length, s (0 counts as 1).
    #[serde(default = "d15")]
    pub max_utterance_s: u32,
    /// Utterances shorter than this, ms (700), are not sent: noise bursts behind an opening
    /// squelch. The job's context counts them under `transcribe.short`. A transcript that is
    /// `dropped` has a `reason`: `queue` (the stage's own queue of 4 was full) or `busy` (the
    /// speech daemon had no free slot for 30 s).
    #[serde(default = "d700")]
    pub min_utterance_ms: u32,
}

/// A closed utterance: 16 kHz mono samples and the time they span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub start_ns: u64,
    pub end_ns: u64,
    pub samples: Vec<i16>,
}

/// The utterance being built.
struct Open {
    start_ns: u64,
    /// End of the last frame taken in.
    end_ns: u64,
    resampler: Resampler,
    samples: Vec<f32>,
}

/// Cuts timed audio frames into utterances, downmixed (channel mean) and resampled to
/// 16 kHz. A frame starting more than `gap_ms` after the previous frame's end closes the
/// open utterance; so does reaching `max_utterance_s`, the rest of the audio starting the
/// next one at that instant. Closed utterances under `min_utterance_ms` are discarded and
/// counted ([`Accumulator::short`]). The first frame
/// fixes the rate and channel count; a change later is an error.
pub struct Accumulator {
    gap_ns: u64,
    max_samples: usize,
    min_samples: usize,
    short: u32,
    /// (rate, channels, a fresh resampler from the rate), fixed by the first frame.
    format: Option<(u64, u16, Resampler)>,
    open: Option<Open>,
}

impl Accumulator {
    pub fn new(gap_ms: u32, max_utterance_s: u32, min_utterance_ms: u32) -> Self {
        Self {
            gap_ns: gap_ms as u64 * 1_000_000,
            max_samples: max_utterance_s.max(1) as usize * RATE_16K as usize,
            min_samples: min_utterance_ms as usize * RATE_16K as usize / 1_000,
            short: 0,
            format: None,
            open: None,
        }
    }

    /// Utterances discarded so far for being under `min_utterance_ms`.
    pub fn short(&self) -> u32 {
        self.short
    }

    /// Take in `interleaved` samples at `rate_hz` / `channels` starting at `t0_ns`;
    /// returns the utterances this closes, oldest first.
    pub fn push(
        &mut self,
        t0_ns: u64,
        rate_hz: u64,
        channels: u16,
        interleaved: &[f32],
    ) -> Result<Vec<Utterance>, String> {
        match &self.format {
            Some((r, _, _)) if *r != rate_hz => {
                return Err(format!("sample_rate_hz changed from {r} to {rate_hz}"));
            }
            Some((_, c, _)) if *c != channels => {
                return Err(format!("channels changed from {c} to {channels}"));
            }
            Some(_) => {}
            None => {
                if rate_hz == 0 || channels == 0 {
                    return Err("transcribe: sample rate and channels must be >= 1".into());
                }
                let p =
                    Resampler::new(rate_hz, RATE_16K).map_err(|e| format!("transcribe: {e}"))?;
                self.format = Some((rate_hz, channels, p));
            }
        }
        let width = channels as usize;
        let n = interleaved.len() / width;
        let mono: Vec<f32> = interleaved
            .chunks_exact(width)
            .map(|s| s.iter().sum::<f32>() / width as f32)
            .collect();
        let end_ns = t0_ns.saturating_add(ns_of(n as u64, rate_hz));
        let mut closed = Vec::new();
        if let Some(o) = &self.open
            && t0_ns > o.end_ns.saturating_add(self.gap_ns)
            && let Some(u) = self.close()
        {
            closed.push(u);
        }
        let format = &self.format;
        let open = self.open.get_or_insert_with(|| Open {
            start_ns: t0_ns,
            end_ns: t0_ns,
            resampler: format
                .as_ref()
                .map(|f| f.2.clone())
                .expect("format fixed above"),
            samples: Vec::new(),
        });
        let resampled = open.resampler.process(&mono);
        open.samples.extend_from_slice(&resampled);
        open.end_ns = open.end_ns.max(end_ns);
        while open.samples.len() >= self.max_samples {
            let cut = open.start_ns + ns_of(self.max_samples as u64, RATE_16K);
            let rest = open.samples.split_off(self.max_samples);
            let full = std::mem::replace(&mut open.samples, rest);
            closed.push(Utterance {
                start_ns: open.start_ns,
                end_ns: cut,
                samples: to_s16(&full),
            });
            open.start_ns = cut;
        }
        Ok(closed.into_iter().filter(|u| self.long_enough(u)).collect())
    }

    /// Whether an utterance is open (audio taken in and not yet closed).
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Close the open utterance (the input ended, went quiet or skipped frames); `None`
    /// when there is none or it is under `min_utterance_ms`.
    pub fn flush(&mut self) -> Option<Utterance> {
        self.close().filter(|u| self.long_enough(u))
    }

    /// Whether `u` reaches the backend; a shorter one is counted.
    fn long_enough(&mut self, u: &Utterance) -> bool {
        let ok = u.samples.len() >= self.min_samples;
        if !ok {
            self.short += 1;
        }
        ok
    }

    fn close(&mut self) -> Option<Utterance> {
        let o = self.open.take()?;
        Some(Utterance {
            start_ns: o.start_ns,
            end_ns: o.end_ns,
            samples: to_s16(&o.samples),
        })
    }
}

/// Nanoseconds spanned by `n` samples at `rate_hz`.
fn ns_of(n: u64, rate_hz: u64) -> u64 {
    (n as u128 * 1_000_000_000 / rate_hz as u128) as u64
}

fn to_s16(x: &[f32]) -> Vec<i16> {
    x.iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect()
}

/// Turns audio into `transcript` events through the node's speech backend.
pub struct TranscribeStage {
    spec: StageSpec,
    backend: Option<Arc<dyn SpeechBackend>>,
    timeout: Duration,
}

impl TranscribeStage {
    /// The stage on `backend` (`None`: every run fails with [`NO_BACKEND`]), each call
    /// bounded by `timeout`.
    pub fn new(backend: Option<Arc<dyn SpeechBackend>>, timeout: Duration) -> Self {
        Self {
            spec: StageSpec {
                kind: "transcribe",
                inputs: vec![StreamKind::Audio],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(TranscribeParams),
            },
            backend,
            timeout,
        }
    }
}

/// An utterance on its way to the backend.
struct Job {
    span: Span,
    wav: Vec<u8>,
    duration_ms: u64,
}

type Answer = (Span, u64, Result<Transcript, SpeechError>);

/// The one backend call in flight, aborted when dropped (the stage ended or was
/// cancelled).
struct InFlight(JoinHandle<Answer>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The run's plumbing: publishes events and keeps the backend busy with one call at a
/// time and at most [`QUEUE`] utterances waiting.
struct Worker {
    backend: Arc<dyn SpeechBackend>,
    language: String,
    timeout: Duration,
    bus: Arc<StreamBus>,
    out: StreamId,
    /// Provenance of the latest input frame; events carry it with their own span.
    prov: Option<Provenance>,
    queue: VecDeque<Job>,
    in_flight: Option<InFlight>,
    errors: u32,
}

impl Worker {
    fn publish(&self, span: &Span, event: serde_json::Value) {
        let Some(mut prov) = self.prov.clone() else {
            return;
        };
        prov.span = span.clone();
        self.bus
            .publish(Frame::new(self.out.clone(), prov, Payload::Event(event)));
    }

    /// Hand `u` to the backend now, queue it, or drop it when the queue is full.
    fn offer(&mut self, u: Utterance) {
        let span = Span {
            start_ns: Timestamp(u.start_ns),
            end_ns: Timestamp(u.end_ns),
        };
        let job = Job {
            span: span.clone(),
            duration_ms: u.samples.len() as u64 * 1_000 / RATE_16K,
            wav: wav_s16_mono(&u.samples),
        };
        if self.in_flight.is_none() {
            self.start(job);
        } else if self.queue.len() < QUEUE {
            self.queue.push_back(job);
        } else {
            self.publish(
                &span,
                serde_json::json!({
                    "protocol": "transcript",
                    "dropped": true,
                    "reason": "queue",
                    "start_ns": span.start_ns.0,
                    "end_ns": span.end_ns.0,
                }),
            );
        }
    }

    fn start(&mut self, job: Job) {
        let (backend, language, timeout) =
            (self.backend.clone(), self.language.clone(), self.timeout);
        self.in_flight = Some(InFlight(tokio::spawn(async move {
            // The backend bounds its own call; this outer bound also covers its queue.
            let bound = timeout + backend.queue_wait();
            let r = tokio::time::timeout(bound, backend.transcribe(&job.wav, &language))
                .await
                .unwrap_or(Err(SpeechError::Timeout(timeout)));
            (job.span, job.duration_ms, r)
        })));
    }

    /// The call in flight finished: publish what it gave and start the next.
    fn answered(
        &mut self,
        joined: Result<Answer, tokio::task::JoinError>,
    ) -> Result<(), StageError> {
        self.in_flight = None;
        let (span, duration_ms, r) =
            joined.map_err(|e| StageError::Failed(format!("transcribe: backend call: {e}")))?;
        match r {
            Ok(t) => {
                self.errors = 0;
                self.publish(
                    &span,
                    serde_json::json!({
                        "protocol": "transcript",
                        "text": t.text,
                        "language": t.language,
                        "start_ns": span.start_ns.0,
                        "end_ns": span.end_ns.0,
                        "backend": self.backend.name(),
                        "duration_ms": duration_ms,
                        "queued_ms": t.queued_ms,
                    }),
                );
            }
            Err(SpeechError::Busy(_)) => {
                // The daemon is shared and full: a drop, not a fault of this job.
                self.publish(
                    &span,
                    serde_json::json!({
                        "protocol": "transcript",
                        "dropped": true,
                        "reason": "busy",
                        "start_ns": span.start_ns.0,
                        "end_ns": span.end_ns.0,
                    }),
                );
            }
            Err(e) => {
                self.errors += 1;
                self.publish(
                    &span,
                    serde_json::json!({
                        "protocol": "transcript",
                        "error": e.to_string(),
                        "start_ns": span.start_ns.0,
                        "end_ns": span.end_ns.0,
                    }),
                );
                if self.errors >= MAX_ERRORS {
                    return Err(StageError::Failed(format!(
                        "transcribe: backend {}: {e}",
                        self.backend.name()
                    )));
                }
            }
        }
        if let Some(next) = self.queue.pop_front() {
            self.start(next);
        }
        Ok(())
    }

    /// Wait for the call in flight; pending forever when there is none.
    async fn next_answer(&mut self) -> Result<Answer, tokio::task::JoinError> {
        match &mut self.in_flight {
            Some(InFlight(h)) => h.await,
            None => std::future::pending().await,
        }
    }
}

#[async_trait]
impl Stage for TranscribeStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        params: serde_json::Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let params: TranscribeParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("transcribe params: {e}")))?;
        let Some(backend) = self.backend.clone() else {
            return Err(StageError::Failed(NO_BACKEND.into()));
        };
        let language = normalise(&params.language);
        if !backend.languages().contains(&language) {
            return Err(StageError::Failed(format!(
                "transcribe: backend {} supports {}, not {language}",
                backend.name(),
                backend.languages().join(", ")
            )));
        }
        if inputs.len() != 1 {
            return Err(StageError::Failed("expected 1 input".into()));
        }
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut acc = Accumulator::new(
            params.gap_ms,
            params.max_utterance_s,
            params.min_utterance_ms,
        );
        let mut w = Worker {
            backend,
            language,
            timeout: self.timeout,
            bus,
            out,
            prov: None,
            queue: VecDeque::new(),
            in_flight: None,
            errors: 0,
        };
        let rx = &mut inputs[0];
        // Wall-clock silence after the last frame closes the open utterance; armed only
        // while one is open, reset by every frame. A frame already waiting wins over it
        // (the branch order is biased), so a gap in `t0` is always judged on the audio.
        let gap = Duration::from_millis(params.gap_ms as u64);
        let mut quiet = std::pin::pin!(tokio::time::sleep(gap));
        let mut armed = false;
        // The `short` count last noted, and when it may next be (set while one is held back).
        let mut noted = 0;
        let mut note_at = tokio::time::Instant::now();
        let mut note_held = false;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                a = w.next_answer() => w.answered(a)?,
                r = rx.recv() => match r {
                    Ok(frame) => {
                        let Payload::Audio { sample_rate_hz, channels, samples } = &frame.payload
                        else {
                            return Err(StageError::Failed(
                                "transcribe expects audio frames".into(),
                            ));
                        };
                        let closed = acc
                            .push(frame.provenance.span.start_ns.0, sample_rate_hz.0, *channels, samples)
                            .map_err(StageError::Failed)?;
                        w.prov = Some(frame.provenance.clone());
                        for u in closed {
                            w.offer(u);
                        }
                        quiet.as_mut().reset(tokio::time::Instant::now() + gap);
                        armed = acc.is_open();
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        zeta_dsp::note_lag("transcribe", n);
                        if let Some(u) = acc.flush() {
                            w.offer(u);
                        }
                        armed = false;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = &mut quiet, if armed => {
                    armed = false;
                    if let Some(u) = acc.flush() {
                        w.offer(u);
                    }
                }
                _ = tokio::time::sleep_until(note_at), if note_held => {}
            }
            if acc.short() > noted && tokio::time::Instant::now() >= note_at {
                noted = acc.short();
                ctx.note(serde_json::json!({"transcribe": {"short": noted}}));
                note_at = tokio::time::Instant::now() + NOTE_EVERY;
                note_held = false;
            } else {
                note_held = acc.short() > noted;
            }
        }
        // The input closed: transcribe what is still open or waiting.
        if let Some(u) = acc.flush() {
            w.offer(u);
        }
        if acc.short() > noted {
            ctx.note(serde_json::json!({"transcribe": {"short": acc.short()}}));
        }
        while w.in_flight.is_some() {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                a = w.next_answer() => w.answered(a)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u64 = 48_000;
    const SEC: u64 = 1_000_000_000;

    /// `seconds` of a 0.5 amplitude 440 Hz tone at 48 kHz.
    fn tone(seconds: f64) -> Vec<f32> {
        (0..(RATE as f64 * seconds) as usize)
            .map(|n| {
                0.5 * (2.0 * std::f64::consts::PI * 440.0 * n as f64 / RATE as f64).sin() as f32
            })
            .collect()
    }

    /// Feed `x` from `t0` in 100 ms frames; returns the utterances closed and the end time.
    fn feed(acc: &mut Accumulator, x: &[f32], t0: u64) -> (Vec<Utterance>, u64) {
        let mut out = Vec::new();
        let mut t = t0;
        for c in x.chunks(4_800) {
            out.extend(acc.push(t, RATE, 1, c).unwrap());
            t += c.len() as u64 * SEC / RATE;
        }
        (out, t)
    }

    #[test]
    fn params_default_and_are_strict() {
        let p: TranscribeParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            (
                p.language.as_str(),
                p.gap_ms,
                p.max_utterance_s,
                p.min_utterance_ms
            ),
            ("en", 300, 15, 700)
        );
        assert!(serde_json::from_value::<TranscribeParams>(serde_json::json!({"gap": 1})).is_err());
        let schema = serde_json::to_value(schemars::schema_for!(TranscribeParams)).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        for (k, prop) in schema["properties"].as_object().unwrap() {
            assert!(prop.get("default").is_some(), "{k} has no default");
        }
    }

    #[test]
    fn utterances_under_min_utterance_ms_are_discarded() {
        let mut acc = Accumulator::new(300, 15, 700);
        let (_, end) = feed(&mut acc, &tone(0.6), 0);
        assert_eq!(acc.flush(), None);
        assert_eq!(acc.short(), 1);
        let (_, end) = feed(&mut acc, &tone(0.8), end + SEC);
        assert!(acc.flush().is_some_and(|u| u.end_ns <= end));
        assert_eq!(acc.short(), 1);
    }

    #[test]
    fn gap_closes_and_short_is_discarded() {
        let mut acc = Accumulator::new(300, 15, 500);
        let (none, end) = feed(&mut acc, &tone(1.0), 0);
        assert!(none.is_empty());
        // 300 ms after the end is not yet a gap; 301 ms is.
        let (none, end2) = feed(&mut acc, &tone(0.1), end + 300_000_000);
        assert!(none.is_empty());
        let (closed, _) = feed(&mut acc, &tone(0.4), end2 + 301_000_000);
        assert_eq!(closed.len(), 1);
        assert_eq!((closed[0].start_ns, closed[0].end_ns), (0, end2));
        assert_eq!(closed[0].samples.len(), 17_600);
        // The 400 ms tail is too short to send.
        assert_eq!(acc.flush(), None);
    }

    #[test]
    fn max_length_splits_contiguous_audio() {
        let mut acc = Accumulator::new(300, 2, 500);
        let (closed, end) = feed(&mut acc, &tone(5.0), 0);
        let spans: Vec<(u64, u64, usize)> = closed
            .iter()
            .map(|u| (u.start_ns, u.end_ns, u.samples.len()))
            .collect();
        assert_eq!(
            spans,
            vec![(0, 2 * SEC, 32_000), (2 * SEC, 4 * SEC, 32_000)]
        );
        let last = acc.flush().unwrap();
        assert_eq!(
            (last.start_ns, last.end_ns, last.samples.len()),
            (4 * SEC, end, 16_000)
        );
    }

    #[test]
    fn downmix_and_format_changes() {
        let mut acc = Accumulator::new(300, 15, 500);
        let x = tone(1.0);
        let stereo: Vec<f32> = x.iter().flat_map(|&s| [s, 0.0]).collect();
        acc.push(0, RATE, 2, &stereo).unwrap();
        let half = acc.flush().unwrap();
        let mut mono = Accumulator::new(300, 15, 500);
        let quiet: Vec<f32> = x.iter().map(|s| s / 2.0).collect();
        mono.push(0, RATE, 1, &quiet).unwrap();
        assert_eq!(half, mono.flush().unwrap());
        assert_eq!(
            acc.push(0, 16_000, 2, &[0.0; 2]).unwrap_err(),
            "sample_rate_hz changed from 48000 to 16000"
        );
        assert_eq!(
            acc.push(0, RATE, 1, &[0.0]).unwrap_err(),
            "channels changed from 2 to 1"
        );
    }
}
