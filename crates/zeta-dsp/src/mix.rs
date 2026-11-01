//! The `mix` stage: the audio of the jobs a node lists, summed for listening.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use zeta_core::plans::CHANNEL_MATCH_HZ;
use zeta_core::stage::{Backend, MixInput, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, JobId, Provenance, SourceId, StreamId, StreamKind, Timestamp};

use crate::{frame_span, note_lag};

/// The stage's kind.
pub const MIX_STAGE: &str = "mix";
/// How long a burst's first samples wait before its queue feeds the sum, so block-sized
/// delivery does not starve it.
pub const PREBUFFER_MS: u64 = 40;

/// The only audio format mixed.
const RATE_HZ: u64 = 48_000;
/// The clock's period.
const TICK_MS: u64 = 20;
/// The most one input gives per tick.
const TICK_SAMPLES: usize = 960;
/// Ticks between polls of the node's list (500 ms).
const POLL_TICKS: u64 = 25;
/// The longest a queue may grow (200 ms): a fast radio clock loses samples rather than
/// build delay.
const QUEUE_CAP_SAMPLES: usize = 9_600;
/// How long an input stays active after its last samples.
const HANGOVER: Duration = Duration::from_millis(500);
/// Samples this soon after a queue ran dry mean it starved, not that a burst ended.
const UNDERRUN_WINDOW: Duration = Duration::from_millis(100);
/// How far a frame's span may run from the wall clock before it re-anchors.
const REANCHOR_NS: u64 = 100_000_000;
/// How often the span clock takes the system clock again (see [`WallClock`]).
const WALL_REBASE: Duration = Duration::from_secs(3600);
/// The limiter's knee: unity gain inside it.
const KNEE: f32 = 0.8;

/// `mix` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct MixParams {
    /// The sources whose jobs are mixed; `null` for every source.
    #[serde(deserialize_with = "nullable_sources")]
    #[schemars(required, schema_with = "nullable_sources_schema")]
    pub sources: Option<Vec<SourceId>>,
    /// The channels left out of the sum.
    #[serde(default)]
    pub muted: Vec<MixMute>,
}

/// A channel muted in a mix: any job on `source` centred within
/// [`CHANNEL_MATCH_HZ`] of `center_hz`.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct MixMute {
    pub source: SourceId,
    pub center_hz: u64,
}

/// Deserialize a required `sources`: a missing key is an error, `null` is `None`.
fn nullable_sources<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<SourceId>>, D::Error> {
    <Option<Vec<SourceId>> as Deserialize>::deserialize(d)
}

/// Schema of the required, nullable `sources`.
fn nullable_sources_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["array", "null"],
        "items": {"type": "string"},
        "description": "The sources whose jobs are mixed; null for every source."
    })
}

/// Unity inside the knee, then a `tanh` curve that never reaches ±1 (far out `tanh` rounds
/// to 1, so its result is held to the largest `f32` under 1).
pub fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let limited = KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh();
        x.signum() * limited.min(1.0 - f32::EPSILON / 2.0)
    }
}

/// Sums the audio of the jobs `StageContext::mix_inputs` lists on one 20 ms clock. It has
/// no input ports, as its set changes while it runs, and ends only on cancel.
pub struct MixStage {
    spec: StageSpec,
}

impl MixStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: MIX_STAGE,
                inputs: vec![],
                outputs: vec![StreamKind::Audio, StreamKind::Event],
                params_schema: schemars::schema_for!(MixParams),
            },
        }
    }
}

impl Default for MixStage {
    fn default() -> Self {
        Self::new()
    }
}

/// The wall clock as the stage's timer sees it: the system clock when last taken, advanced
/// by the timer, so spans and hangovers share one clock. Taken again hourly, which heals a
/// system clock that was stepped (a Pi's first NTP sync).
struct WallClock {
    base_ns: u64,
    at: Instant,
}

impl WallClock {
    fn new() -> Self {
        Self {
            base_ns: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64),
            at: Instant::now(),
        }
    }

    fn now_ns(&mut self) -> u64 {
        if self.at.elapsed() >= WALL_REBASE {
            *self = Self::new();
        }
        self.base_ns
            .saturating_add(self.at.elapsed().as_nanos() as u64)
    }
}

/// One job being mixed.
struct Mixed {
    job: JobId,
    stream: StreamId,
    source: SourceId,
    center_hz: u64,
    rx: broadcast::Receiver<Frame>,
    /// The stream's sender is gone; a poll swaps it.
    closed: bool,
    /// Consecutive polls that did not list the job.
    absent: u32,
    muted: bool,
    /// The rate and channels that got this stream skipped.
    skipped: Option<(u64, u16)>,
    queue: VecDeque<f32>,
    /// When the queue starts or started feeding; `None` while idle.
    feed_at: Option<Instant>,
    /// When a feeding queue last ran dry, until the next samples.
    dried_at: Option<Instant>,
    last_heard: Option<Instant>,
    active: bool,
    dropped_samples: u64,
    underruns: u64,
}

impl Mixed {
    fn new(found: &MixInput, rx: broadcast::Receiver<Frame>) -> Self {
        Self {
            job: found.job.clone(),
            stream: found.stream.clone(),
            source: found.source.clone(),
            center_hz: found.center_hz,
            rx,
            closed: false,
            absent: 0,
            muted: false,
            skipped: None,
            queue: VecDeque::new(),
            feed_at: None,
            dried_at: None,
            last_heard: None,
            active: false,
            dropped_samples: 0,
            underruns: 0,
        }
    }

    /// Nothing queued and nothing owed: what a mute leaves.
    fn silence(&mut self) {
        self.queue.clear();
        self.feed_at = None;
        self.dried_at = None;
        self.last_heard = None;
        self.active = false;
    }

    /// Take what the receiver holds into the queue, without waiting.
    fn drain(&mut self, now: Instant, wall_ns: u64, last: &mut Option<Value>) {
        loop {
            match self.rx.try_recv() {
                Ok(frame) => self.take(frame, now, wall_ns, last),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Lagged(n)) => note_lag(MIX_STAGE, n),
                Err(TryRecvError::Closed) => {
                    self.closed = true;
                    return;
                }
            }
        }
    }

    fn take(&mut self, frame: Frame, now: Instant, wall_ns: u64, last: &mut Option<Value>) {
        let Payload::Audio {
            sample_rate_hz,
            channels,
            samples,
        } = frame.payload
        else {
            return;
        };
        if self.skipped.is_some() {
            return;
        }
        if sample_rate_hz.0 != RATE_HZ || channels != 1 {
            self.skipped = Some((sample_rate_hz.0, channels));
            return;
        }
        if self.muted || samples.is_empty() {
            return;
        }
        self.queue.extend(samples.iter().copied());
        self.last_heard = Some(now);
        *last = Some(json!({
            "job": self.job.0,
            "source": self.source.0,
            "center_hz": self.center_hz,
            "t_ns": wall_ns,
        }));
    }

    /// Lose the queue's oldest samples over the cap.
    fn cap(&mut self) {
        let over = self.queue.len().saturating_sub(QUEUE_CAP_SAMPLES);
        if over > 0 {
            self.queue.drain(..over);
            self.dropped_samples += over as u64;
        }
    }

    /// Add this tick's samples to `sum`, growing it to the longest contribution. A queue
    /// that merely empties stays feeding: it has run dry when a tick finds nothing in it.
    fn feed(&mut self, now: Instant, sum: &mut Vec<f32>) {
        if self.feed_at.is_none() && !self.queue.is_empty() {
            self.feed_at = Some(now + Duration::from_millis(PREBUFFER_MS));
            if self
                .dried_at
                .take()
                .is_some_and(|d| now - d < UNDERRUN_WINDOW)
            {
                self.underruns += 1;
            }
        }
        if !self.feed_at.is_some_and(|at| now >= at) {
            return;
        }
        if self.queue.is_empty() {
            self.feed_at = None;
            self.dried_at = Some(now);
            return;
        }
        let n = self.queue.len().min(TICK_SAMPLES);
        if sum.len() < n {
            sum.resize(n, 0.0);
        }
        for (out, s) in sum.iter_mut().zip(self.queue.drain(..n)) {
            *out += s;
        }
    }

    /// Active until the hangover after its last samples, and held through a relaunch: while
    /// its old stream is gone or the job is missing from the list its new samples cannot
    /// reach the queue, which says nothing of whether the channel still talks.
    fn update_active(&mut self, now: Instant) {
        let heard = self.last_heard.is_some_and(|t| now - t < HANGOVER);
        let held = self.active && (self.closed || self.absent > 0);
        self.active = !self.muted && (heard || held);
    }

    fn id_json(&self) -> Value {
        json!({"job": self.job.0, "source": self.source.0, "center_hz": self.center_hz})
    }
}

/// The running stage's inputs and what it last heard.
struct Mix {
    params: MixParams,
    inputs: Vec<Mixed>,
    last: Option<Value>,
}

impl Mix {
    /// Apply one answer of the node's list: join the new, swap the restarted, refresh the
    /// rest, and drop a job missing twice running (one poll can catch a relaunch with no
    /// outputs yet).
    fn apply_poll(&mut self, listed: &[MixInput], bus: &StreamBus, now: Instant, wall_ns: u64) {
        let listed: Vec<&MixInput> = listed
            .iter()
            .filter(|l| {
                self.params
                    .sources
                    .as_ref()
                    .is_none_or(|named| named.contains(&l.source))
            })
            .collect();
        self.inputs.retain_mut(|input| {
            if listed.iter().any(|l| l.job == input.job) {
                input.absent = 0;
                true
            } else {
                input.absent += 1;
                input.absent < 2
            }
        });
        for found in listed {
            let index = self.inputs.iter().position(|i| i.job == found.job);
            let input = match index {
                Some(i) => &mut self.inputs[i],
                None => {
                    let Some(rx) = bus.subscribe(&found.stream) else {
                        continue;
                    };
                    self.inputs.push(Mixed::new(found, rx));
                    self.inputs.last_mut().expect("just pushed")
                }
            };
            if input.stream != found.stream
                && let Some(rx) = bus.subscribe(&found.stream)
            {
                input.drain(now, wall_ns, &mut self.last);
                input.rx = rx;
                input.stream = found.stream.clone();
                input.closed = false;
                input.skipped = None;
                if input.active {
                    input.last_heard = Some(now);
                }
            }
            input.source = found.source.clone();
            input.center_hz = found.center_hz;
            let muted = self.params.muted.iter().any(|m| {
                m.source == input.source
                    && m.center_hz.abs_diff(input.center_hz) <= CHANNEL_MATCH_HZ
            });
            if muted && !input.muted {
                input.silence();
            }
            input.muted = muted;
        }
    }

    /// The inputs active now, sorted by centre.
    fn active_set(&self) -> Vec<Value> {
        let mut active: Vec<&Mixed> = self.inputs.iter().filter(|i| i.active).collect();
        active.sort_by_key(|i| (i.center_hz, &i.job.0));
        active.into_iter().map(Mixed::id_json).collect()
    }

    /// `job_context.mix`.
    fn context(&self, active: &[Value]) -> Value {
        let by_job = |f: &dyn Fn(&Mixed) -> u64| -> BTreeMap<&str, u64> {
            self.inputs
                .iter()
                .map(|i| (i.job.0.as_str(), f(i)))
                .filter(|&(_, n)| n > 0)
                .collect()
        };
        let muted: Vec<Value> = self
            .inputs
            .iter()
            .filter(|i| i.muted)
            .map(Mixed::id_json)
            .collect();
        let skipped: Vec<String> = self
            .inputs
            .iter()
            .filter_map(|i| {
                i.skipped.map(|(rate, channels)| {
                    format!(
                        "{} {} {} Hz: {rate} Hz, {channels} channels",
                        i.job.0, i.source.0, i.center_hz
                    )
                })
            })
            .collect();
        json!({
            "inputs": self.inputs.len(),
            "active": active,
            "last": self.last,
            "muted": muted,
            "dropped_ms": by_job(&|i| i.dropped_samples * 1000 / RATE_HZ),
            "skipped": skipped,
            "underruns": by_job(&|i| i.underruns),
        })
    }
}

/// The event's one line: the active centres, or `silent`.
fn summary(active: &[Value]) -> String {
    if active.is_empty() {
        return "silent".into();
    }
    active
        .iter()
        .map(|a| {
            format!(
                "{:.6} MHz",
                a["center_hz"].as_u64().unwrap_or(0) as f64 / 1e6
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The provenance `bus` registered for `out`.
fn provenance_of(bus: &StreamBus, out: &StreamId) -> Result<Provenance, StageError> {
    bus.list()
        .into_iter()
        .find(|s| &s.id == out)
        .map(|s| s.provenance)
        .ok_or_else(|| {
            StageError::Failed(format!("mix: output stream {} is not registered", out.0))
        })
}

#[async_trait]
impl Stage for MixStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: Value,
        _inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let Some(hook) = ctx.mix_inputs.clone() else {
            return Err(StageError::Failed("mix needs a node's job list".into()));
        };
        let params: MixParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("mix params: {e}")))?;
        let [audio_out, event_out] = outputs.as_slice() else {
            return Err(StageError::Failed("mix needs two outputs".into()));
        };
        let audio_base = provenance_of(&bus, audio_out)?;
        let event_base = provenance_of(&bus, event_out)?;

        let mut mix = Mix {
            params,
            inputs: Vec::new(),
            last: None,
        };
        let mut active: Vec<Value> = Vec::new();
        let mut noted = mix.context(&active);
        ctx.note(json!({"mix": noted}));

        let mut clock = WallClock::new();
        let mut next_start_ns: Option<u64> = None;
        let mut tick_index: u64 = 0;
        let mut ticker = tokio::time::interval(Duration::from_millis(TICK_MS));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Burst);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                _ = ticker.tick() => {}
            }
            let now = Instant::now();
            let wall_ns = clock.now_ns();

            let polled = tick_index.is_multiple_of(POLL_TICKS);
            tick_index += 1;
            if polled {
                mix.apply_poll(&hook(), &bus, now, wall_ns);
            }

            let mut sum: Vec<f32> = Vec::new();
            for input in &mut mix.inputs {
                input.drain(now, wall_ns, &mut mix.last);
                input.cap();
                input.feed(now, &mut sum);
            }

            if sum.is_empty() {
                next_start_ns = None;
            } else {
                let start_ns = next_start_ns
                    .filter(|s| s.abs_diff(wall_ns) <= REANCHOR_NS)
                    .unwrap_or(wall_ns);
                let span = frame_span(Timestamp(start_ns), sum.len(), RATE_HZ);
                next_start_ns = Some(span.end_ns.0);
                let samples: Vec<f32> = sum.into_iter().map(soft_limit).collect();
                bus.publish(Frame::new(
                    audio_out.clone(),
                    Provenance {
                        span,
                        ..audio_base.clone()
                    },
                    Payload::Audio {
                        sample_rate_hz: Hz(RATE_HZ),
                        channels: 1,
                        samples: samples.into(),
                    },
                ));
            }

            for input in &mut mix.inputs {
                input.update_active(now);
            }
            let set = mix.active_set();
            let changed = set != active;
            if changed {
                active = set;
                bus.publish(Frame::new(
                    event_out.clone(),
                    Provenance {
                        span: frame_span(Timestamp(wall_ns), 0, RATE_HZ),
                        ..event_base.clone()
                    },
                    Payload::Event(json!({
                        "protocol": "mix",
                        "active": active,
                        "t_ns": wall_ns,
                        "summary": summary(&active),
                    })),
                ));
            }
            if changed || polled {
                let state = mix.context(&active);
                if state != noted {
                    ctx.note(json!({"mix": state}));
                    noted = state;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::task::JoinHandle;
    use tokio::time::sleep;
    use zeta_core::types::Span;

    use super::*;

    const C: u64 = 476_625_000;

    fn prov(job: &str, source: &str) -> Provenance {
        Provenance {
            source: SourceId(source.into()),
            job: Some(JobId(job.into())),
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(0),
            },
        }
    }

    /// A running mix over a stand-in job list.
    struct Bed {
        bus: Arc<StreamBus>,
        list: Arc<Mutex<Vec<MixInput>>>,
        notes: Arc<Mutex<Vec<Value>>>,
        audio: broadcast::Receiver<Frame>,
        events: broadcast::Receiver<Frame>,
        task: JoinHandle<Result<(), StageError>>,
    }

    impl Bed {
        /// A mix with `params` over the jobs `(job, source, center_hz)`, past its first poll.
        async fn start(params: Value, jobs: &[(&str, &str, u64)]) -> (Self, Vec<StreamId>) {
            Self::start_on(StreamBus::new(4096), params, jobs).await
        }

        async fn start_on(
            bus: StreamBus,
            params: Value,
            jobs: &[(&str, &str, u64)],
        ) -> (Self, Vec<StreamId>) {
            let bus = Arc::new(bus);
            let list = Arc::new(Mutex::new(Vec::new()));
            let notes = Arc::new(Mutex::new(Vec::new()));
            let audio_out = bus.register(StreamKind::Audio, prov("mix-job", "node"));
            let event_out = bus.register(StreamKind::Event, prov("mix-job", "node"));
            let audio = bus.subscribe(&audio_out).unwrap();
            let events = bus.subscribe(&event_out).unwrap();
            let hook_list = list.clone();
            let sink = notes.clone();
            let ctx = StageContext::new(SourceId("node".into()))
                .with_note(Arc::new(move |mut v: Value| {
                    sink.lock().unwrap().push(v["mix"].take());
                }))
                .with_mix_inputs(Arc::new(move || hook_list.lock().unwrap().clone()));
            let (b, outs) = (bus.clone(), vec![audio_out, event_out]);
            let mut bed = Self {
                bus,
                list,
                notes,
                audio,
                events,
                task: tokio::spawn(async move {
                    MixStage::new()
                        .run(params, vec![], outs, b, CancellationToken::new(), ctx)
                        .await
                }),
            };
            let streams = jobs.iter().map(|j| bed.add(j.0, j.1, j.2)).collect();
            bed.run_ms(1).await;
            (bed, streams)
        }

        fn add(&mut self, job: &str, source: &str, center_hz: u64) -> StreamId {
            let stream = self.bus.register(StreamKind::Audio, prov(job, source));
            self.list.lock().unwrap().push(MixInput {
                job: JobId(job.into()),
                stream: stream.clone(),
                source: SourceId(source.into()),
                center_hz,
            });
            stream
        }

        fn feed_as(&self, stream: &StreamId, samples: &[f32], rate: u64, channels: u16) {
            self.bus.publish(Frame::new(
                stream.clone(),
                prov("in", "s"),
                Payload::Audio {
                    sample_rate_hz: Hz(rate),
                    channels,
                    samples: samples.into(),
                },
            ));
        }

        fn feed(&self, stream: &StreamId, samples: &[f32]) {
            self.feed_as(stream, samples, RATE_HZ, 1);
        }

        /// Feed `total_ms` of `level` in blocks of `block_ms`, one each `block_ms`.
        async fn feed_for(&self, stream: &StreamId, level: f32, total_ms: u64, block_ms: u64) {
            let block = vec![level; (block_ms * RATE_HZ / 1000) as usize];
            for _ in 0..total_ms / block_ms {
                self.feed(stream, &block);
                self.run_ms(block_ms).await;
            }
        }

        async fn run_ms(&self, n: u64) {
            sleep(Duration::from_millis(n)).await;
        }

        fn frames(&mut self) -> Vec<Frame> {
            let mut out = Vec::new();
            while let Ok(f) = self.audio.try_recv() {
                out.push(f);
            }
            out
        }

        fn event_list(&mut self) -> Vec<Value> {
            let mut out = Vec::new();
            while let Ok(f) = self.events.try_recv() {
                let Payload::Event(v) = f.payload else {
                    panic!("not an event");
                };
                out.push(v);
            }
            out
        }

        fn mix(&self) -> Value {
            self.notes.lock().unwrap().last().unwrap().clone()
        }
    }

    fn samples_of(f: &Frame) -> Vec<f32> {
        let Payload::Audio { samples, .. } = &f.payload else {
            panic!("not audio");
        };
        samples.to_vec()
    }

    fn all_samples(frames: &[Frame]) -> Vec<f32> {
        frames.iter().flat_map(samples_of).collect()
    }

    fn assert_contiguous(frames: &[Frame]) {
        for w in frames.windows(2) {
            assert_eq!(
                w[1].provenance.span.start_ns, w[0].provenance.span.end_ns,
                "a gap in the spans"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn one_input_comes_out_unchanged_once_and_in_order() {
        let (mut bed, s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        let ramp: Vec<f32> = (0..4800).map(|i| i as f32 / 4800.0 * 0.5).collect();
        bed.feed(&s[0], &ramp);
        bed.run_ms(300).await;
        let frames = bed.frames();
        assert_eq!(all_samples(&frames), ramp);
        for f in &frames {
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &f.payload
            else {
                panic!("not audio");
            };
            assert!(samples.len() <= 960);
            assert_eq!((sample_rate_hz.0, *channels), (48_000, 1));
            assert_eq!(f.provenance.job, Some(JobId("mix-job".into())));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn silence_publishes_no_frame() {
        let (mut bed, _s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        bed.run_ms(2000).await;
        assert!(bed.frames().is_empty());
        assert_eq!(bed.mix()["inputs"], 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_waits_for_the_prebuffer_then_feeds() {
        let (mut bed, s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        bed.feed(&s[0], &vec![0.1; 9600]);
        bed.run_ms(PREBUFFER_MS).await;
        assert!(bed.frames().is_empty(), "fed before the prebuffer");
        bed.run_ms(40).await;
        assert!(!bed.frames().is_empty(), "still silent after the prebuffer");
    }

    #[tokio::test(start_paused = true)]
    async fn block_delivery_rides_with_no_underrun() {
        for block_ms in [16, 100] {
            let (mut bed, s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
            bed.feed_for(&s[0], 0.1, 2000, block_ms).await;
            bed.run_ms(600).await;
            let fed = (2000 / block_ms) * block_ms * RATE_HZ / 1000;
            let frames = bed.frames();
            assert_contiguous(&frames);
            assert_eq!(all_samples(&frames).len() as u64, fed, "{block_ms} ms");
            let mix = bed.mix();
            assert_eq!(mix["underruns"], json!({}), "{block_ms} ms");
            assert_eq!(mix["dropped_ms"], json!({}), "{block_ms} ms");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn two_inputs_sum_and_a_collision_stays_in_range() {
        let (mut bed, s) = Bed::start(
            json!({"sources": null}),
            &[("a", "b210", C), ("b", "hackrf", C + 25_000)],
        )
        .await;
        bed.feed(&s[0], &[0.25; 960]);
        bed.feed(&s[1], &[0.25; 960]);
        bed.run_ms(100).await;
        assert_eq!(all_samples(&bed.frames()), vec![0.5; 960]);

        bed.feed(&s[0], &[0.9; 960]);
        bed.feed(&s[1], &[0.9; 960]);
        bed.run_ms(100).await;
        let out = all_samples(&bed.frames());
        assert_eq!(out.len(), 960);
        assert!(out.iter().all(|&x| x > 0.8 && x < 1.0), "{:?}", out[0]);
    }

    #[test]
    fn the_limiter_is_unity_under_the_knee() {
        assert_eq!(soft_limit(0.79), 0.79);
        assert_eq!(soft_limit(-0.8), -0.8);
        assert!(soft_limit(5.0) < 1.0);
        assert!(soft_limit(-5.0) > -1.0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_input_joins_and_a_removed_one_leaves_without_a_restart() {
        let (mut bed, _s) = Bed::start(json!({"sources": null}), &[]).await;
        let s = bed.add("a", "b210", C);
        bed.run_ms(500).await;
        assert_eq!(bed.mix()["inputs"], 1);
        bed.feed(&s, &[0.2; 960]);
        bed.run_ms(100).await;
        assert_eq!(all_samples(&bed.frames()), vec![0.2; 960]);

        bed.list.lock().unwrap().clear();
        bed.run_ms(1000).await;
        assert_eq!(bed.mix()["inputs"], 0);
        assert!(!bed.task.is_finished());
    }

    #[tokio::test(start_paused = true)]
    async fn active_fires_on_each_change_with_the_hangover() {
        let (mut bed, s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        bed.feed(&s[0], &[0.1; 4800]);
        bed.run_ms(50).await;
        let events = bed.event_list();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["protocol"], "mix");
        assert_eq!(
            events[0]["active"],
            json!([{"job": "a", "source": "b210", "center_hz": C}])
        );
        assert_eq!(events[0]["summary"], "476.625000 MHz");
        assert!(events[0]["t_ns"].as_u64().unwrap() > 0);

        bed.run_ms(350).await; // 400 ms after the samples
        assert!(bed.event_list().is_empty());

        bed.run_ms(200).await; // 600 ms after them
        let events = bed.event_list();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["active"], json!([]));
        assert_eq!(events[0]["summary"], "silent");
        assert_eq!(bed.mix()["last"]["job"], "a");
        assert_eq!(bed.mix()["last"]["center_hz"], C);
    }

    #[tokio::test(start_paused = true)]
    async fn a_muted_input_is_not_heard_and_never_active() {
        let (mut bed, s) = Bed::start(
            json!({"sources": null, "muted": [{"source": "b210", "center_hz": C + 1000}]}),
            &[("a", "b210", C)],
        )
        .await;
        bed.feed(&s[0], &[0.1; 960]);
        bed.run_ms(600).await;
        assert!(bed.frames().is_empty());
        assert!(bed.event_list().is_empty());
        assert_eq!(
            bed.mix()["muted"],
            json!([{"job": "a", "source": "b210", "center_hz": C}])
        );
        assert_eq!(bed.mix()["last"], Value::Null);

        let (mut bed, s) = Bed::start(
            json!({"sources": null, "muted": [{"source": "b210", "center_hz": C + 1001}]}),
            &[("a", "b210", C)],
        )
        .await;
        bed.feed(&s[0], &[0.1; 960]);
        bed.run_ms(100).await;
        assert_eq!(all_samples(&bed.frames()).len(), 960);
    }

    #[tokio::test(start_paused = true)]
    async fn a_mute_matches_the_source_too() {
        let (mut bed, s) = Bed::start(
            json!({"sources": null, "muted": [{"source": "hackrf", "center_hz": C}]}),
            &[("a", "b210", C)],
        )
        .await;
        bed.feed(&s[0], &[0.1; 960]);
        bed.run_ms(100).await;
        assert_eq!(all_samples(&bed.frames()).len(), 960);
    }

    #[tokio::test(start_paused = true)]
    async fn a_retune_into_a_mute_clears_the_queue() {
        let (mut bed, s) = Bed::start(
            json!({"sources": null, "muted": [{"source": "b210", "center_hz": C + 25_000}]}),
            &[("a", "b210", C)],
        )
        .await;
        bed.run_ms(480).await;
        bed.feed(&s[0], &vec![0.1; 9600]); // just before the poll at 500 ms
        bed.run_ms(5).await;
        bed.list.lock().unwrap()[0].center_hz = C + 25_000;
        bed.run_ms(100).await; // through the poll at 500 ms
        let before = all_samples(&bed.frames()).len();
        bed.run_ms(100).await;
        assert!(before < 9600, "{before}");
        assert!(bed.frames().is_empty());
        assert_eq!(bed.mix()["active"], json!([]));
    }

    #[tokio::test(start_paused = true)]
    async fn a_queue_over_200_ms_drops_its_oldest_and_counts_it() {
        let (mut bed, s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        let ramp: Vec<f32> = (0..19_200).map(|i| i as f32 / 19_200.0 * 0.5).collect();
        bed.feed(&s[0], &ramp);
        bed.run_ms(600).await;
        let dropped = bed.mix()["dropped_ms"]["a"].as_u64().unwrap();
        assert!((190..=210).contains(&dropped), "{dropped}");
        let out = all_samples(&bed.frames());
        assert!(out[0] >= ramp[9_600], "{}", out[0]);
    }

    #[tokio::test(start_paused = true)]
    async fn another_rate_or_stereo_is_skipped_once_and_listed() {
        let (mut bed, s) = Bed::start(
            json!({"sources": null}),
            &[("a", "b210", C), ("b", "b210", C + 25_000)],
        )
        .await;
        for _ in 0..3 {
            bed.feed_as(&s[0], &[0.1; 960], 44_100, 1);
            bed.feed_as(&s[1], &[0.1; 960], RATE_HZ, 2);
        }
        bed.run_ms(600).await;
        assert!(bed.frames().is_empty());
        assert!(bed.event_list().is_empty());
        let mix = bed.mix();
        assert_eq!(mix["active"], json!([]));
        let skipped = mix["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert_eq!(
            skipped[0],
            format!("a b210 {C} Hz: 44100 Hz, 1 channels").as_str()
        );
    }

    /// No event left the job out of `active` once it was in.
    fn assert_no_flicker(events: &[Value]) {
        for e in events {
            let list = e["active"].as_array().unwrap();
            assert!(list.iter().any(|a| a["job"] == "a"), "flicker: {e}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_restarted_input_keeps_its_place_and_does_not_flicker() {
        let (mut bed, s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        bed.feed_for(&s[0], 0.1, 200, 20).await;
        assert_eq!(bed.event_list().len(), 1);
        bed.frames();

        // A retune: the hook lists a new stream, the old one is gone.
        let second = bed.bus.register(StreamKind::Audio, prov("a", "b210"));
        bed.list.lock().unwrap()[0].stream = second.clone();
        bed.bus.unregister(&s[0]);
        bed.feed_for(&second, 0.1, 1000, 20).await;
        assert_no_flicker(&bed.event_list());
        assert_eq!(bed.mix()["inputs"], 1);
        assert!(!bed.frames().is_empty(), "the new stream is not heard");

        // A relaunch caught by a poll: no outputs, then a new stream, the audio silent
        // meanwhile for longer than the hangover.
        let entry = bed.list.lock().unwrap().remove(0);
        bed.bus.unregister(&second);
        bed.run_ms(520).await;
        assert_eq!(bed.mix()["inputs"], 1);
        let third = bed.bus.register(StreamKind::Audio, prov("a", "b210"));
        bed.list.lock().unwrap().push(MixInput {
            stream: third.clone(),
            ..entry
        });
        bed.run_ms(100).await;
        bed.frames();
        bed.feed_for(&third, 0.1, 400, 20).await;
        assert_no_flicker(&bed.event_list());
        assert_eq!(bed.mix()["inputs"], 1);
        assert!(!bed.frames().is_empty(), "the third stream is not heard");
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_named_sources_are_inputs() {
        let (bed, s) = Bed::start(
            json!({"sources": ["a"]}),
            &[("x", "a", C), ("y", "b", C + 25_000)],
        )
        .await;
        bed.run_ms(600).await;
        assert_eq!(bed.bus.receiver_count(&s[0]), Some(1));
        assert_eq!(bed.bus.receiver_count(&s[1]), Some(0));
        assert_eq!(bed.mix()["inputs"], 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_context_starts_empty_on_every_run() {
        let (bed, _s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        let first = bed.notes.lock().unwrap()[0].clone();
        assert_eq!(
            first,
            json!({
                "inputs": 0,
                "active": [],
                "last": null,
                "muted": [],
                "dropped_ms": {},
                "skipped": [],
                "underruns": {},
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_lagged_receiver_is_counted_by_kind() {
        fn lagged() -> u64 {
            crate::lagged_frames_by_kind()
                .iter()
                .find(|(k, _)| *k == "mix")
                .map_or(0, |&(_, n)| n)
        }
        let before = lagged();
        let (bed, s) = Bed::start_on(
            StreamBus::new(4),
            json!({"sources": null}),
            &[("a", "b210", C)],
        )
        .await;
        for _ in 0..20 {
            bed.feed(&s[0], &[0.1; 48]);
        }
        bed.run_ms(40).await;
        assert!(lagged() >= before + 16, "{} then {}", before, lagged());
    }

    #[tokio::test(start_paused = true)]
    async fn spans_are_contiguous_in_a_burst_and_reanchor_after_a_gap() {
        let (mut bed, s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        bed.feed(&s[0], &[0.1; 4800]);
        bed.run_ms(300).await;
        let first = bed.frames();
        assert_contiguous(&first);
        bed.run_ms(2000).await;
        bed.feed(&s[0], &[0.1; 4800]);
        bed.run_ms(300).await;
        let second = bed.frames();
        assert_contiguous(&second);
        let gap =
            second[0].provenance.span.start_ns.0 - first.last().unwrap().provenance.span.end_ns.0;
        assert!(gap >= 2_000_000_000 - REANCHOR_NS, "{gap}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_underrun_is_counted_when_samples_follow_a_dry_queue() {
        let (mut bed, s) = Bed::start(json!({"sources": null}), &[("a", "b210", C)]).await;
        bed.feed(&s[0], &[0.1; 960]);
        bed.run_ms(100).await; // gave its samples at about 60 ms, found dry at about 80 ms
        bed.feed(&s[0], &[0.1; 960]);
        bed.run_ms(500).await;
        assert_eq!(bed.mix()["underruns"]["a"], 1);
        assert_eq!(all_samples(&bed.frames()).len(), 1920);
    }

    #[tokio::test]
    async fn without_a_node_it_fails_saying_so() {
        let err = MixStage::new()
            .run(
                json!({"sources": null}),
                vec![],
                vec![],
                Arc::new(StreamBus::new(4)),
                CancellationToken::new(),
                StageContext::new(SourceId("node".into())),
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "stage failed: mix needs a node's job list");
    }

    #[test]
    fn sources_is_required() {
        assert!(serde_json::from_value::<MixParams>(json!({"muted": []})).is_err());
        let p: MixParams = serde_json::from_value(json!({"sources": null})).unwrap();
        assert!(p.sources.is_none() && p.muted.is_empty());
    }
}
