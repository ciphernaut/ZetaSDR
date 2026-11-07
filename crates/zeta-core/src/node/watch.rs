//! The `watch` job (design 6): a controller that holds a rigid lease on its band like
//! `identify`, runs `fft → detect` over the capture, and identifies each signal `detect`
//! announces with an owned `identify` run on the signal's centre, one at a time. A proof
//! leaves the decoder running (inside the capture, or moved to a free source under
//! `decode_on: free`); the watch then owns it: it is listed in `job_context.watch`
//! (`signals`, `decoding`, `current`), stopped once its signal has gone and it has been
//! silent for [`SILENCE_MS`] (`lost`), and stopped with the watch on a `job_stop`, a
//! `replace` or a failure (a stage's, the loop's, a panic of the loop included). A
//! `graph_put` restart or a node restart re-adopts the decoders from the persisted
//! context instead: those whose job no longer runs are dropped, those whose signal does
//! not reappear within the detector's `gone_ms` are stopped. An unknown signal is tried
//! again after [`RETRY_MS`] while it persists, at most [`RETRY_MAX`] times; with
//! `on_unknown: capture` its bursts are captured once per run by an owned `capture`
//! child (see [`capture`]).
//!
//! Events, `protocol: "watch"`, go on the job's event stream beside `detect`'s `signal`
//! events: `state` `identifying | decoding | unknown | lost | capturing | captured`, with
//! `signal`, `job`, `mode`, `source`, `placed` (`capture | free`), `features` and
//! `allocations` and `origins` (for `unknown`), `captures` (for `captured`), `summary` and `t_ns`
//! (fields that do not apply are `null`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::TryRecvError;
use tokio_util::sync::CancellationToken;

use super::persist::now;
use super::{Inner, source_fault_reason};
use crate::graph::{self, Edge, Graph, Node as GraphNode, NodeId};
use crate::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use crate::modes::ModeState;
use crate::modes::score::score_rows;
use crate::ops::{DETECT_GONE_MS_DEFAULT, DETECT_STAGE, JobKind, SONDEHUB_STAGE};
use crate::port::{Port, Tuning};
use crate::stage::StageError;
use crate::stream::{Frame, Payload};
use crate::types::{Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind};

mod capture;

/// How long after an `unknown` verdict a signal that persists is identified again.
pub const RETRY_MS: u64 = 60_000;
/// How many times an unknown signal is identified again before it is left alone (until
/// it goes and comes back).
pub const RETRY_MAX: u32 = 3;
/// How long a decoder whose signal has gone may stay silent (no event) before it is
/// stopped as `lost`.
pub const SILENCE_MS: u64 = 120_000;

#[cfg(feature = "test-fixtures")]
static RETRY_OVERRIDE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "test-fixtures")]
static SILENCE_OVERRIDE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Tests only: use `retry_ms` for [`RETRY_MS`] and `silence_ms` for [`SILENCE_MS`] in
/// every watch of the process from now on (0 restores the constant).
#[cfg(feature = "test-fixtures")]
pub fn test_timing(retry_ms: u64, silence_ms: u64) {
    use std::sync::atomic::Ordering;
    RETRY_OVERRIDE_MS.store(retry_ms, Ordering::SeqCst);
    SILENCE_OVERRIDE_MS.store(silence_ms, Ordering::SeqCst);
}

fn retry_ms() -> u64 {
    #[cfg(feature = "test-fixtures")]
    {
        let v = RETRY_OVERRIDE_MS.load(std::sync::atomic::Ordering::SeqCst);
        if v > 0 {
            return v;
        }
    }
    RETRY_MS
}

/// Tests only: the watch event state after whose publication the next watch fails, and
/// whether by a panic of its loop (else the loop ends `Failed` at its next tick).
#[cfg(feature = "test-fixtures")]
static TEST_FAULT: std::sync::Mutex<Option<(String, bool)>> = std::sync::Mutex::new(None);
#[cfg(feature = "test-fixtures")]
static TEST_FAIL_NOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tests only: make the first watch that publishes an event in `state` from now on fail
/// right after it: its loop panics when `panic`, else it ends with the error
/// `watch: test failure` at its next tick.
#[cfg(feature = "test-fixtures")]
pub fn test_fault_after(state: &str, panic: bool) {
    *TEST_FAULT.lock().unwrap() = Some((state.to_string(), panic));
}

/// Tests only: how many of the next sink restarts fail to launch.
#[cfg(feature = "test-fixtures")]
static TEST_FAIL_SINK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Tests only: make the next `times` launches of a decoder graph with the sink fail.
#[cfg(feature = "test-fixtures")]
pub fn test_fail_sink_launch(times: u32) {
    TEST_FAIL_SINK.store(times, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(feature = "test-fixtures")]
fn sink_launch_fails() -> bool {
    use std::sync::atomic::Ordering;
    TEST_FAIL_SINK
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

#[cfg(feature = "test-fixtures")]
fn test_fault_at(state: &str) {
    let hit = {
        let mut f = TEST_FAULT.lock().unwrap();
        match f.as_ref() {
            Some((s, _)) if s == state => f.take(),
            _ => None,
        }
    };
    match hit {
        Some((_, true)) => panic!("watch: test panic after {state}"),
        Some((_, false)) => TEST_FAIL_NOW.store(true, std::sync::atomic::Ordering::SeqCst),
        None => {}
    }
}

#[cfg(feature = "test-fixtures")]
fn test_fail_now() -> bool {
    TEST_FAIL_NOW.swap(false, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(not(feature = "test-fixtures"))]
fn test_fail_now() -> bool {
    false
}

fn silence_ms() -> u64 {
    #[cfg(feature = "test-fixtures")]
    {
        let v = SILENCE_OVERRIDE_MS.load(std::sync::atomic::Ordering::SeqCst);
        if v > 0 {
            return v;
        }
    }
    SILENCE_MS
}

/// The refusal of a sink with no position and no station (the text `sink_sondehub` notes
/// when it idles for the same reason).
const SINK_NO_STATION: &str = "sink.params.lat: no station set (station set --lat --lon --alt-m)";
/// How often the loop looks at its streams, its identify run and its decoders.
const POLL: Duration = Duration::from_millis(50);
/// How long a resumed watch waits for the records it names (its decoders, a stale
/// identify) to be launched: a resume launches the watch first (the widest band).
const ADOPT_WAIT: Duration = Duration::from_secs(5);

/// Where a decoder the watch keeps runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placed {
    /// Inside the watch's own capture, where identify proved it.
    Capture,
    /// On a free source (`decode_on: free`).
    Free,
}

impl Placed {
    fn as_str(self) -> &'static str {
        match self {
            Placed::Capture => "capture",
            Placed::Free => "free",
        }
    }
}

/// The watch's policy, read from its `detect` node's params (which the stage ignores).
#[derive(Clone, Debug)]
pub(super) struct Policy {
    free: bool,
    prefer: Vec<SourceId>,
    gone_ms: u64,
    /// The `sink_sondehub` params every decoder graph the watch starts gets a sink with
    /// (`sondehub` in the `detect` node's params; none appends nothing).
    sondehub: Option<serde_json::Value>,
    /// With `on_unknown: capture`, the `burst_capture` params (the `capture` block, `{}`
    /// for the defaults) of the capture an unknown signal gets; `None` with `report`.
    capture: Option<serde_json::Value>,
}

impl Policy {
    /// The policy in `graph`'s `detect` node: `decode_on` (`capture`, default, or
    /// `free`), `prefer` (source ids) and the detector's `gone_ms`.
    fn of(graph: &Graph) -> Policy {
        let p = graph
            .nodes
            .iter()
            .find(|n| n.kind == DETECT_STAGE)
            .map(|n| n.params.clone())
            .unwrap_or_default();
        Policy {
            free: p["decode_on"] == "free",
            prefer: p["prefer"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(|s| SourceId(s.into())))
                        .collect()
                })
                .unwrap_or_default(),
            gone_ms: p["gone_ms"].as_u64().unwrap_or(DETECT_GONE_MS_DEFAULT),
            sondehub: Some(p["sondehub"].clone()).filter(|v| !v.is_null()),
            capture: (p["on_unknown"] == "capture").then(|| match &p["capture"] {
                serde_json::Value::Null => serde_json::json!({}),
                v => v.clone(),
            }),
        }
    }
}

/// A signal as `detect` announced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Signal {
    center_hz: u64,
    bandwidth_hz: u64,
    snr_db: f64,
    first_ns: u64,
    last_ns: u64,
}

impl Signal {
    /// The signal of a `detect` `signal` event.
    fn of(v: &serde_json::Value) -> Option<Signal> {
        Some(Signal {
            center_hz: v["center_hz"].as_u64()?,
            bandwidth_hz: v["bandwidth_hz"].as_u64()?,
            snr_db: v["snr_db"].as_f64().unwrap_or(0.0),
            first_ns: v["first_ns"].as_u64().unwrap_or(0),
            last_ns: v["last_ns"].as_u64().unwrap_or(0),
        })
    }

    /// Whether the band `center_hz` ± `bandwidth_hz`/2 overlaps the signal's.
    fn overlaps(&self, center_hz: u64, bandwidth_hz: u64) -> bool {
        2 * self.center_hz.abs_diff(center_hz) < self.bandwidth_hz + bandwidth_hz
    }

    fn mhz(&self) -> String {
        format!("{:.4} MHz", self.center_hz as f64 / 1e6)
    }
}

/// The index of the tracked signal whose band overlaps `center_hz` ± `bandwidth_hz`/2 and
/// whose centre lies nearest `center_hz`: a wide track that also covers a narrow one's
/// centre (two runs the detector sees side by side) never takes the narrow one's verdict
/// or announcement.
fn nearest(signals: &[Tracked], center_hz: u64, bandwidth_hz: u64) -> Option<usize> {
    signals
        .iter()
        .enumerate()
        .filter(|(_, x)| x.signal.overlaps(center_hz, bandwidth_hz))
        .min_by_key(|(_, x)| x.signal.center_hz.abs_diff(center_hz))
        .map(|(i, _)| i)
}

/// A signal the watch follows (`job_context.watch.signals`).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Tracked {
    #[serde(flatten)]
    signal: Signal,
    /// Whether `detect` still sees it (no `gone` since its last `new`).
    present: bool,
    /// Identify runs that ended without a proof.
    tries: u32,
    /// When it may be identified again after an `unknown`.
    next_retry_ns: Option<u64>,
    /// Decoding, or left alone after [`RETRY_MAX`] retries.
    handled: bool,
    /// The band its identify run takes: the best-scoring fitting row's width.
    band_hz: u64,
    /// That row's score, the queue's order.
    score: f32,
    /// The last unknown verdict's layer-1 record.
    #[serde(default)]
    features: Option<serde_json::Value>,
    /// The last unknown verdict's register answer (identify's `allocations`).
    #[serde(default)]
    allocations: Option<serde_json::Value>,
    /// Whether this run has started a capture of it (`on_unknown: capture`).
    #[serde(default)]
    captured: bool,
    /// The captures its capture wrote.
    #[serde(default)]
    captures: Vec<String>,
}

/// What came of giving a kept decoder the `sondehub` sink.
enum SinkOutcome {
    /// Not asked for, or nothing to attach it to.
    Off,
    /// The decoder runs with the sink.
    On,
    /// The decoder runs without it, for this reason.
    Refused(String),
    /// The decoder could not be relaunched at all.
    Lost(String),
    /// The watch was stopped meanwhile.
    Cancelled,
}

/// Where a decoder was placed and what the sink made of it.
struct Placement {
    job: JobId,
    source: SourceId,
    placed: Placed,
    /// Why it fell back to the capture, when it did.
    why: Option<String>,
    sink: SinkOutcome,
}

/// A decoder the watch keeps (`job_context.watch.decoding`).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Decoding {
    mode: String,
    job: JobId,
    source: SourceId,
    signal: Signal,
    since_ns: u64,
    placed: Placed,
    /// Why the decoder runs without the `sondehub` sink it was asked to have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sink_error: Option<String>,
    /// The last event of the decoder this run saw (else `since_ns`).
    #[serde(skip)]
    last_event_ns: u64,
    /// When `detect` reported its signal `gone` (cleared when it is seen again).
    #[serde(skip)]
    gone_since_ns: Option<u64>,
    /// A re-adopted decoder whose signal has not been seen again since: stopped when
    /// this passes.
    #[serde(skip)]
    confirm_by_ns: Option<u64>,
}

/// What a running watch needs; made at launch from the record ([`Seed`]) and the
/// streams the launch registered.
pub(super) struct Run {
    id: JobId,
    port: Arc<dyn Port>,
    tuning: Tuning,
    prov: Provenance,
    /// The `detect` output: the loop reads its `signal` events and publishes its own.
    out: StreamId,
    rx: Receiver<Frame>,
    cancel: CancellationToken,
    seed: Seed,
}

/// What a launch reads from the watch's record before it is recorded running.
pub(super) struct Seed {
    policy: Policy,
    /// The decoders a run before this one kept (persisted `watch.decoding`).
    adopt: Vec<Decoding>,
    /// The identify a run before a node restart was waiting on.
    stale: Option<JobId>,
}

impl Seed {
    pub(super) fn of(rec: &JobRecord) -> Seed {
        let w = &rec.context["watch"];
        Seed {
            policy: Policy::of(&rec.graph),
            adopt: serde_json::from_value(w["decoding"].clone()).unwrap_or_default(),
            stale: w["current"].as_str().map(|s| JobId(s.into())),
        }
    }
}

impl Run {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        id: JobId,
        port: Arc<dyn Port>,
        tuning: Tuning,
        prov: Provenance,
        out: StreamId,
        rx: Receiver<Frame>,
        cancel: CancellationToken,
        seed: Seed,
    ) -> Run {
        Run {
            id,
            port,
            tuning,
            prov,
            out,
            rx,
            cancel,
            seed,
        }
    }
}

/// The loop's state, persisted as `job_context.watch` on every change.
struct State {
    signals: Vec<Tracked>,
    decoding: Vec<Decoding>,
    /// The identify run in progress and the centre of the signal it is on.
    current: Option<(JobId, u64)>,
    /// The decoders' event streams, by job.
    subs: HashMap<JobId, Vec<Receiver<Frame>>>,
    /// A proved decoder while it is placed: neither `current` nor in `decoding` yet.
    pending: Option<JobId>,
    /// The capture children running, with the signal each is on.
    capturing: Vec<(JobId, Signal)>,
    /// The signals this run has captured, with what each capture wrote: a signal that
    /// goes and comes back is not captured again.
    captured: Vec<(Signal, Vec<String>)>,
    /// Set until [`watch_loop`]'s exit path has run: any other end of the state (a panic
    /// unwinding the loop) tears down what it started from a task the drop spawns.
    guard: Option<Guard>,
}

/// What [`State`]'s drop needs to tear down without the loop.
struct Guard {
    inner: Arc<Inner>,
    port: Arc<dyn Port>,
}

impl Drop for State {
    fn drop(&mut self) {
        let Some(Guard { inner, port }) = self.guard.take() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let current = self.current.take().map(|(j, _)| j);
        let pending = self.pending.take();
        let decoders = std::mem::take(&mut self.decoding)
            .into_iter()
            .map(|d| (d.job, d.source))
            .collect();
        let captures = std::mem::take(&mut self.capturing)
            .into_iter()
            .map(|(j, _)| j)
            .collect();
        rt.spawn(async move {
            let source = port.id().clone();
            release(&inner, &source, current, pending, decoders, captures).await;
            inner.leases.idle_if_unheld(port.as_ref()).await;
        });
    }
}

/// Stop what a run of the watch on `source` started: the identify run `current` (and the
/// decoder a proof it landed left, never adopted), a proved decoder being placed,
/// `decoders` and the capture children `captures`. Gated on `source` (the watch's lease
/// is held meanwhile, so there is nothing to idle, and a `replace` holding the gate never
/// waits on it), plainly on another source.
async fn release(
    inner: &Arc<Inner>,
    source: &SourceId,
    current: Option<JobId>,
    pending: Option<JobId>,
    decoders: Vec<(JobId, SourceId)>,
    captures: Vec<JobId>,
) {
    for id in captures {
        let _ = inner.stop_gated(&id).await;
        inner.captured.lock().unwrap().remove(&id);
    }
    if let Some(id) = current {
        let _ = inner.stop_gated(&id).await;
        let verdict = inner.verdicts.lock().unwrap().remove(&id);
        if let Some(job) = verdict
            .as_ref()
            .and_then(|v| v["verdict"]["job"].as_str())
            .map(|s| JobId(s.into()))
        {
            let _ = inner.stop_gated(&job).await;
        }
    }
    if let Some(job) = pending {
        let _ = inner.stop_gated(&job).await;
    }
    for (job, on) in decoders {
        let _ = match on == *source {
            true => inner.stop_gated(&job).await,
            false => inner.stop(&job).await,
        };
    }
}

impl State {
    fn context(&self) -> serde_json::Value {
        serde_json::json!({"watch": {
            "signals": self.signals,
            "decoding": self.decoding,
            "current": self.current.as_ref().map(|(j, _)| j.0.clone()),
        }})
    }
}

/// One `watch` event's fields beyond `state` and `summary`.
#[derive(Default)]
struct Event<'a> {
    signal: Option<&'a Signal>,
    job: Option<&'a JobId>,
    mode: Option<&'a str>,
    source: Option<&'a SourceId>,
    placed: Option<Placed>,
    features: Option<serde_json::Value>,
    /// The register's allocations for an `unknown` (the identify verdict's).
    allocations: Option<serde_json::Value>,
    /// The origins of an `unknown` (the identify verdict's).
    origins: Option<serde_json::Value>,
    /// The captures a `captured` lists.
    captures: Option<&'a [String]>,
}

impl Inner {
    /// Persist the loop's state as `job_context.watch`.
    fn watch_save(&self, r: &Run, w: &State) {
        let _ = self.context_merge(&r.id, w.context());
    }

    /// Publish one `watch` event on the job's event stream.
    fn watch_event(&self, r: &Run, state: &str, summary: String, e: Event) {
        let t = now();
        let payload = serde_json::json!({
            "protocol": "watch",
            "state": state,
            "signal": e.signal,
            "job": e.job.map(|j| j.0.clone()),
            "mode": e.mode,
            "source": e.source.map(|s| s.0.clone()),
            "placed": e.placed.map(Placed::as_str),
            "features": e.features,
            "allocations": e.allocations,
            "origins": e.origins,
            "captures": e.captures,
            "summary": summary,
            "t_ns": t.0,
        });
        let prov = Provenance {
            span: Span {
                start_ns: t,
                end_ns: t,
            },
            ..r.prov.clone()
        };
        self.bus
            .publish(Frame::new(r.out.clone(), prov, Payload::Event(payload)));
        #[cfg(feature = "test-fixtures")]
        test_fault_at(state);
    }

    /// Whether `id` has a running record in this node.
    fn runs(&self, id: &JobId) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|e| e.record.state == JobState::Running && e.cancel.is_some())
    }

    /// Wait up to [`ADOPT_WAIT`] for `id` to run; `None` when cancelled first.
    async fn wait_running(&self, r: &Run, id: &JobId) -> Option<bool> {
        let deadline = tokio::time::Instant::now() + ADOPT_WAIT;
        loop {
            if self.runs(id) {
                return Some(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Some(false);
            }
            tokio::select! {
                _ = r.cancel.cancelled() => return None,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
    }

    /// Clear `id`'s owner: a decoder the watch keeps is an ordinary job, kept in the
    /// history when it stops.
    fn disown(&self, id: &JobId) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(e) = jobs.get_mut(id)
            && e.record.owner.is_some()
        {
            e.record.owner = None;
            let _ = super::persist::persist(&self.jobs_dir, &e.record);
        }
    }

    /// Stop a job the watch started: gated on the watch's own source (the watch's lease
    /// is held meanwhile, so there is nothing to idle, and a `replace` holding the gate
    /// never waits on it), plainly on another source.
    async fn stop_started(&self, r: &Run, job: &JobId, source: &SourceId) {
        let _ = match *source == r.prov.source {
            true => self.stop_gated(job).await,
            false => self.stop(job).await,
        };
    }

    /// The band an identify run on `s` takes and its queue score: the best-scoring ready
    /// row (`score_rows` at the signal's centre and width) that fits the signal (its
    /// measured width within half to 1.5 times the row's band, the scoring's width term)
    /// and the watch's band, else the signal's own width at score 0.
    fn identify_band(&self, r: &Run, s: &Signal) -> (u64, f32) {
        score_rows(
            &self.modes,
            s.center_hz,
            s.bandwidth_hz,
            "",
            &|_| 0.0,
            &|_| 0.0,
        )
        .into_iter()
        .filter(|x| x.mode.state == ModeState::Ready)
        .find(|x| {
            let band = &x.mode.row.band;
            let bw = band.bandwidth_hz as f64;
            (bw * 0.5..=bw * 1.5).contains(&(s.bandwidth_hz as f64))
                && band.bandwidth_hz <= r.tuning.bandwidth_hz.0
        })
        .map_or((s.bandwidth_hz, 0.0), |x| {
            (x.mode.row.band.bandwidth_hz, x.score)
        })
    }

    /// Whether a running job other than the watch's own (itself, its identify run, its
    /// decoders) holds a band on the watch's source that overlaps `s`.
    fn held_elsewhere(&self, r: &Run, w: &State, s: &Signal) -> bool {
        self.leases
            .holders(&r.prov.source)
            .into_iter()
            .filter(|(j, _)| *j != r.id)
            .filter(|(j, _)| w.current.as_ref().is_none_or(|(c, _)| c != j))
            .filter(|(j, _)| !w.decoding.iter().any(|d| d.job == *j))
            .filter(|(j, _)| !self.owned_by(j, &r.id))
            .any(|(_, t)| s.overlaps(t.center_hz.0, t.bandwidth_hz.0))
    }

    /// Whether `job`'s record names `owner` as its owner.
    fn owned_by(&self, job: &JobId, owner: &JobId) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .get(job)
            .is_some_and(|e| e.record.owner.as_ref() == Some(owner))
    }

    /// Start an owned `identify` run on `t`'s centre and band inside the watch's
    /// capture, recorded as `current` first. The launch runs in its own task, so a stop
    /// of the watch never drops it half way (as identify's own children). `Ok(None)`:
    /// cancelled.
    async fn start_identify(
        self: &Arc<Self>,
        r: &Run,
        w: &mut State,
        t: &Tracked,
    ) -> Result<Option<JobId>, String> {
        let tuning = Tuning {
            center_hz: Hz(t.signal.center_hz),
            bandwidth_hz: Hz(t.band_hz),
        };
        let graph = JobKind::Identify
            .expand(&r.prov.source, tuning, Hz(0), None, None, None, &self.modes)
            .map_err(|e| e.to_string())?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: JobKind::Identify.name().to_string(),
            graph,
            source: r.prov.source.clone(),
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner: Some(r.id.clone()),
        };
        let id = rec.id.clone();
        w.current = Some((id.clone(), t.signal.center_hz));
        self.watch_save(r, w);
        let mut launch = self.launch_task(rec);
        tokio::select! {
            res = &mut launch => match res {
                Ok(Ok(())) => Ok(Some(id)),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("launch task: {e}")),
            },
            _ = r.cancel.cancelled() => {
                if self.gated(&r.id) {
                    let inner = self.clone();
                    tokio::spawn(async move {
                        if matches!(launch.await, Ok(Ok(()))) {
                            let _ = inner.stop(&id).await;
                        }
                    });
                } else if matches!(launch.await, Ok(Ok(()))) {
                    let _ = self.stop_gated(&id).await;
                }
                w.current = None;
                Ok(None)
            }
        }
    }

    /// Start `mode` on `source` at `tuning` as an ordinary job: tuned there unless a job
    /// holds the source (the elastic jobs there follow), expanded for the capture.
    async fn start_on(
        self: &Arc<Self>,
        source: &SourceId,
        mode: &str,
        tuning: Tuning,
        sondehub: Option<&serde_json::Value>,
    ) -> Result<(JobId, SinkOutcome), String> {
        let port = self
            .ports
            .read()
            .unwrap()
            .get(source)
            .cloned()
            .ok_or_else(|| format!("unknown source {}", source.0))?;
        let kind = JobKind::from_name(mode).ok_or_else(|| format!("{mode} is not a job kind"))?;
        let (granted, tuned) = self
            .leases
            .tune_if_unheld_tuned(port.as_ref(), tuning)
            .await
            .map_err(|e| e.to_string())?;
        let bin_rate = self.bank_bin_rate(source, granted);
        let graph = kind.expand(
            source,
            tuning,
            granted.bandwidth_hz,
            bin_rate,
            None,
            None,
            &self.modes,
        );
        let started = match graph {
            Ok(mut graph) => {
                let sink = match sondehub.map(|p| self.append_sink(&mut graph, p)) {
                    None | Some(Ok(false)) => SinkOutcome::Off,
                    Some(Ok(true)) => SinkOutcome::On,
                    Some(Err(why)) => SinkOutcome::Refused(why),
                };
                // A graph with the sink is launched as stored: a re-expansion would drop it.
                let with_sink = matches!(sink, SinkOutcome::On);
                let rec = JobRecord {
                    format: JOB_RECORD_FORMAT,
                    id: JobId::new(),
                    kind: kind.name().to_string(),
                    graph,
                    source: source.clone(),
                    tuning,
                    state: JobState::Running,
                    context: serde_json::json!({}),
                    created_ns: now(),
                    input: None,
                    capture_rate_hz: Some(granted.bandwidth_hz.0),
                    elastic: false,
                    operator_graph: with_sink,
                    owner: None,
                };
                let id = rec.id.clone();
                // In its own task, as identify's children: `launch`'s future runs this.
                match self.launch_task(rec).await {
                    Ok(Ok(())) => Ok((id, sink)),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(e) => Err(format!("launch task: {e}")),
                }
            }
            Err(e) => Err(e.to_string()),
        };
        if tuned {
            let except = started.as_ref().ok().map(|(id, _)| id.clone());
            let _ = self.relaunch_elastic(source, except.as_ref()).await;
        }
        if started.is_err() {
            self.leases.idle_if_unheld(port.as_ref()).await;
        }
        started
    }

    /// Place a decoder identify proved (`proved`, running `mode` inside the watch's
    /// capture) by the watch's policy: `capture` keeps it there; `free` starts `mode` on
    /// the signal's centre and the row's width on a free source other than the watch's
    /// (`prefer` first, then `source list` order), trying the next on a refusal (a
    /// `conflict`: not free after all), then stops `proved`; with none it stays in the
    /// capture. The decoder's job, source, placement and, when it fell back, why, with
    /// what came of giving it the sink.
    async fn place_decoder(
        self: &Arc<Self>,
        r: &Run,
        mode: &str,
        signal: &Signal,
        proved: &JobId,
    ) -> Placement {
        let kept = |why: Option<String>, sink: SinkOutcome| Placement {
            job: proved.clone(),
            source: r.prov.source.clone(),
            placed: Placed::Capture,
            why,
            sink,
        };
        if !r.seed.policy.free {
            let sink = self
                .sink_into(r, proved, r.seed.policy.sondehub.as_ref())
                .await;
            return kept(None, sink);
        }
        let width = self
            .modes
            .get(mode)
            .map_or(signal.bandwidth_hz, |m| m.row.band.bandwidth_hz);
        let ports: Vec<Arc<dyn Port>> = {
            let mut v: Vec<_> = self.ports.read().unwrap().values().cloned().collect();
            v.sort_by(|a, b| a.id().0.cmp(&b.id().0));
            v
        };
        let mut free: Vec<SourceId> = self
            .leases
            .free_sources(&ports, signal.center_hz, width)
            .into_iter()
            .filter(|s| *s != r.prov.source)
            .collect();
        free.sort_by_key(|s| {
            r.seed
                .policy
                .prefer
                .iter()
                .position(|p| p == s)
                .unwrap_or(usize::MAX)
        });
        let tuning = Tuning {
            center_hz: Hz(signal.center_hz),
            bandwidth_hz: Hz(width),
        };
        let mut refused = Vec::new();
        for source in free {
            match self
                .start_on(&source, mode, tuning, r.seed.policy.sondehub.as_ref())
                .await
            {
                Ok((job, sink)) => {
                    self.stop_started(r, proved, &r.prov.source).await;
                    return Placement {
                        job,
                        source,
                        placed: Placed::Free,
                        why: None,
                        sink,
                    };
                }
                Err(e) => refused.push(format!("{}: {e}", source.0)),
            }
        }
        let why = match refused.is_empty() {
            true => "no free source".to_string(),
            false => format!("no free source took it ({})", refused.join("; ")),
        };
        let sink = self
            .sink_into(r, proved, r.seed.policy.sondehub.as_ref())
            .await;
        kept(Some(why), sink)
    }

    /// Append a `sink_sondehub` node with params `sondehub` to `graph`, fed by the event
    /// output of its last event-producing node. `Ok(false)` when there is nothing to do
    /// (the graph has a sink already, or no node with an event output); `Err` with the
    /// reason, the graph left as it was, when the graph with the sink would not validate
    /// (the start and put of the watch check `sondehub` first, so this is the last line of
    /// defence: the decoder runs without the feed and the caller says so).
    ///
    /// A sink whose params give no whole position (`lat`, `lon`, `alt_m`) needs the node's
    /// station for the rest: without one it is refused here, so the decoder runs without
    /// it and the `decoding` summary says why, rather than the stage idling unseen.
    fn append_sink(&self, graph: &mut Graph, sondehub: &serde_json::Value) -> Result<bool, String> {
        if graph.nodes.iter().any(|n| n.kind == SONDEHUB_STAGE) {
            return Ok(false);
        }
        let has = |k: &str| sondehub.get(k).is_some_and(serde_json::Value::is_number);
        if !(has("lat") && has("lon") && has("alt_m")) && self.station.lock().unwrap().is_none() {
            return Err(SINK_NO_STATION.into());
        }
        let from = graph.nodes.iter().rev().find(|n| {
            self.registry
                .spec(&n.kind)
                .is_some_and(|s| s.outputs.contains(&StreamKind::Event))
                && !graph
                    .edges
                    .iter()
                    .any(|e| e.from == n.id && e.kind == StreamKind::Event)
        });
        let Some(from) = from.map(|n| n.id.clone()) else {
            return Ok(false);
        };
        let to = NodeId("sondehub".into());
        let mut with = graph.clone();
        with.nodes.push(GraphNode {
            id: to.clone(),
            kind: SONDEHUB_STAGE.into(),
            params: sondehub.clone(),
        });
        with.edges.push(Edge {
            from,
            to,
            kind: StreamKind::Event,
        });
        graph::validate(&with, &self.registry).map_err(|e| e.to_string())?;
        *graph = with;
        Ok(true)
    }

    /// Give the running decoder `job` the sink (`graph_put`'s restart under the same id).
    /// The restart is cancellable like identify's child launches: when the watch is
    /// stopped meanwhile (a `replace` holds the gate of the radio and waits for the watch),
    /// the launch is finished and the decoder stopped by a task. A launch that fails puts
    /// the graph without the sink back ([`SinkOutcome::Refused`]); when that fails too the
    /// decoder is [`SinkOutcome::Lost`].
    async fn sink_into(
        self: &Arc<Self>,
        r: &Run,
        job: &JobId,
        sondehub: Option<&serde_json::Value>,
    ) -> SinkOutcome {
        let Some(sondehub) = sondehub else {
            return SinkOutcome::Off;
        };
        let Some(original) = self
            .jobs
            .lock()
            .unwrap()
            .get(job)
            .map(|e| e.record.graph.clone())
        else {
            return SinkOutcome::Off;
        };
        let mut graph = original.clone();
        match self.append_sink(&mut graph, sondehub) {
            Ok(false) => return SinkOutcome::Off,
            Err(why) => return SinkOutcome::Refused(why),
            Ok(true) => {}
        }
        // Stopping an owned job forgets its record: it is the watch's ordinary decoder
        // from here on (the caller disowns it too).
        self.disown(job);
        let error = match self.relaunch(r, job, graph, true).await {
            Ok(true) => return SinkOutcome::On,
            Ok(false) => return SinkOutcome::Cancelled,
            Err(e) => format!("sink restart failed ({e})"),
        };
        // The stop happened and the sink graph would not launch: the decoder as it was.
        match self.relaunch(r, job, original, false).await {
            Ok(true) => SinkOutcome::Refused(error),
            Ok(false) => SinkOutcome::Cancelled,
            Err(e) => SinkOutcome::Lost(format!("{error}; relaunch without it failed ({e})")),
        }
    }

    /// Store `graph` as `job`'s (stopping it first when `sink`, the restart that adds it)
    /// and launch it, cancellable. `Ok(false)` when the watch was cancelled meanwhile (a
    /// task then finishes the launch and stops the job).
    async fn relaunch(
        self: &Arc<Self>,
        r: &Run,
        job: &JobId,
        graph: Graph,
        sink: bool,
    ) -> Result<bool, String> {
        let rec = self
            .store_graph(job, graph, None, sink.then_some(true))
            .await
            .map_err(|e| e.to_string())?;
        #[cfg(feature = "test-fixtures")]
        if sink_launch_fails() {
            return Err("test: sink launch refused".into());
        }
        let mut launch = self.launch_task(rec);
        tokio::select! {
            res = &mut launch => match res {
                Ok(Ok(())) => Ok(true),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("launch task: {e}")),
            },
            _ = r.cancel.cancelled() => {
                let (inner, job) = (self.clone(), job.clone());
                if self.gated(&r.id) {
                    // The gate of the radio is not ours: finish and stop it from a task.
                    tokio::spawn(async move {
                        if matches!(launch.await, Ok(Ok(()))) {
                            let _ = inner.stop(&job).await;
                        }
                    });
                } else if matches!(launch.await, Ok(Ok(()))) {
                    let _ = self.stop_gated(&job).await;
                }
                Ok(false)
            }
        }
    }

    /// Subscribe to `job`'s event outputs, for its silence.
    fn decoder_subs(&self, job: &JobId) -> Vec<Receiver<Frame>> {
        let jobs = self.jobs.lock().unwrap();
        jobs.get(job)
            .map(|e| e.outputs.clone())
            .unwrap_or_default()
            .iter()
            .filter(|(k, _)| *k == StreamKind::Event)
            .filter_map(|(_, s)| self.bus.subscribe(s))
            .collect()
    }

    /// Whether the watch's run is ending for a `graph_put` restart (set before its stop),
    /// after which the next run re-adopts its decoders.
    fn restarting(&self, id: &JobId) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|e| e.restarting)
    }
}

/// The `watch` job's controller, one of the job's supervised tasks beside its `fft` and
/// `detect` stages: (1) stop a stale identify a run before a restart left and re-adopt
/// the persisted decoders that still run; (2) follow `detect`'s `signal` events, queueing
/// each new one that no other job's band overlaps; (3) identify the queued signals one at
/// a time, best score first, and place each proof; (4) retry unknown signals, stop lost
/// decoders. See the module docs for the exits.
pub(super) async fn watch_loop(inner: Arc<Inner>, mut r: Run) -> Result<(), StageError> {
    let mut w = State {
        signals: Vec::new(),
        decoding: Vec::new(),
        current: None,
        subs: HashMap::new(),
        pending: None,
        capturing: Vec::new(),
        captured: Vec::new(),
        guard: Some(Guard {
            inner: inner.clone(),
            port: r.port.clone(),
        }),
    };
    let result = run(&inner, &mut r, &mut w).await;
    w.guard = None;
    // At the node's shutdown everything is left as it is: the records resume, the next
    // run re-adopts the decoders and stops the identify run as stale. A `graph_put`
    // restart keeps the decoders for the next run. A fault of the watch's port leaves
    // the decoders to fail on their own (those in the capture fail with the same fault),
    // for the node's recovery of the source to relaunch and the next run to re-adopt;
    // stopping them would mark them `Stopped`, which recovery never relaunches. Any other
    // end (`job_stop`, a `replace`, a failure of the loop or of a stage) tears down all
    // the run started.
    let shutdown = inner.shutdown.is_cancelled();
    let port_faulted = r.port.fault().is_some();
    let teardown = !shutdown && !inner.restarting(&r.id) && !port_faulted;
    let (current, pending, captures) = match shutdown {
        true => (None, None, Vec::new()),
        false => (
            w.current.take().map(|(j, _)| j),
            w.pending.take(),
            std::mem::take(&mut w.capturing)
                .into_iter()
                .map(|(j, _)| j)
                .collect(),
        ),
    };
    let decoders = match teardown {
        true => std::mem::take(&mut w.decoding)
            .into_iter()
            .map(|d| (d.job, d.source))
            .collect(),
        false => Vec::new(),
    };
    release(&inner, &r.prov.source, current, pending, decoders, captures).await;
    if teardown {
        w.signals.clear();
    }
    inner.watch_save(&r, &w);
    result
}

/// The loop proper; returns on cancellation or a fault.
async fn run(inner: &Arc<Inner>, r: &mut Run, w: &mut State) -> Result<(), StageError> {
    let cancelled = || Err(StageError::Cancelled);
    if let Some(stale) = r.seed.stale.take() {
        match inner.wait_running(r, &stale).await {
            None => return cancelled(),
            Some(true) => {
                let _ = inner.stop_gated(&stale).await;
            }
            Some(false) => {}
        }
    }
    let gone_ns = r.seed.policy.gone_ms.saturating_mul(1_000_000);
    for mut d in std::mem::take(&mut r.seed.adopt) {
        match inner.wait_running(r, &d.job).await {
            None => return cancelled(),
            Some(false) => continue,
            Some(true) => {}
        }
        let t = now().0;
        d.last_event_ns = t;
        d.confirm_by_ns = Some(t.saturating_add(gone_ns));
        w.subs.insert(d.job.clone(), inner.decoder_subs(&d.job));
        w.decoding.push(d);
    }
    inner.watch_save(r, w);
    loop {
        tokio::select! {
            _ = r.cancel.cancelled() => return cancelled(),
            _ = tokio::time::sleep(POLL) => {}
        }
        if test_fail_now() {
            return Err(StageError::Failed("watch: test failure".into()));
        }
        if let Some(f) = r.port.fault() {
            return Err(StageError::Failed(source_fault_reason(r.port.id(), &f)));
        }
        let mut changed = signals(inner, r, w);
        changed |= verdict(inner, r, w).await;
        if r.cancel.is_cancelled() {
            return cancelled();
        }
        changed |= decoders(inner, r, w).await;
        changed |= capture::ended(inner, r, w);
        if w.current.is_none() {
            sweep(inner, r, w).await;
            match next(inner, r, w).await {
                Next::Idle => {}
                Next::Started => changed = true,
                Next::Cancelled => return cancelled(),
            }
        }
        if changed {
            inner.watch_save(r, w);
        }
    }
}

/// Take `detect`'s announcements since the last tick into the state; whether it changed.
fn signals(inner: &Arc<Inner>, r: &mut Run, w: &mut State) -> bool {
    let mut changed = false;
    loop {
        let v = match r.rx.try_recv() {
            Ok(f) => match f.payload {
                Payload::Event(v) if v["protocol"] == "signal" => v,
                _ => continue,
            },
            Err(TryRecvError::Lagged(_)) => continue,
            Err(_) => return changed,
        };
        let Some(s) = Signal::of(&v) else { continue };
        let half = r.tuning.bandwidth_hz.0 / 2;
        let inside = s.center_hz.abs_diff(r.tuning.center_hz.0) <= half;
        if !inside {
            continue;
        }
        changed = true;
        let new = v["state"] == "new";
        let t = now().0;
        // A decoder the watch keeps is on it.
        if let Some(d) = w
            .decoding
            .iter_mut()
            .find(|d| d.signal.overlaps(s.center_hz, s.bandwidth_hz))
        {
            match new {
                true => {
                    d.signal = s.clone();
                    d.gone_since_ns = None;
                    d.confirm_by_ns = None;
                }
                false => d.gone_since_ns = Some(t),
            }
        }
        let tracked = nearest(&w.signals, s.center_hz, s.bandwidth_hz);
        match (new, tracked) {
            (true, Some(i)) => {
                w.signals[i].signal = s;
                w.signals[i].present = true;
            }
            (true, None) => {
                let decoded = w
                    .decoding
                    .iter()
                    .any(|d| d.signal.overlaps(s.center_hz, s.bandwidth_hz));
                if decoded || inner.held_elsewhere(r, w, &s) {
                    continue;
                }
                let (band_hz, score) = inner.identify_band(r, &s);
                w.signals.push(Tracked {
                    signal: s,
                    present: true,
                    tries: 0,
                    next_retry_ns: None,
                    handled: false,
                    band_hz,
                    score,
                    features: None,
                    allocations: None,
                    captured: false,
                    captures: Vec::new(),
                });
                capture::seen_before(w);
            }
            (false, Some(i)) => {
                let on = w
                    .current
                    .as_ref()
                    .is_some_and(|(_, c)| w.signals[i].signal.overlaps(*c, 1));
                let decoded = w.decoding.iter().any(|d| {
                    d.signal.overlaps(
                        w.signals[i].signal.center_hz,
                        w.signals[i].signal.bandwidth_hz,
                    )
                });
                let capturing = w.capturing.iter().any(|(_, c)| {
                    c.overlaps(
                        w.signals[i].signal.center_hz,
                        w.signals[i].signal.bandwidth_hz,
                    )
                });
                match on || decoded || capturing {
                    true => w.signals[i].present = false,
                    false => {
                        w.signals.remove(i);
                    }
                }
            }
            (false, None) => {}
        }
    }
}

/// When the identify run in progress has ended, act on its verdict: place a proof's
/// decoder and publish `decoding`; publish `unknown` and schedule a retry otherwise.
/// Whether the state changed.
async fn verdict(inner: &Arc<Inner>, r: &Run, w: &mut State) -> bool {
    let Some((id, center)) = w.current.clone() else {
        return false;
    };
    if inner.runs(&id) {
        return false;
    }
    let verdict = inner.verdicts.lock().unwrap().remove(&id);
    w.current = None;
    let Some(i) = nearest(&w.signals, center, 1) else {
        // Its signal is gone from the list: a proof's decoder is not kept.
        if let Some(job) = verdict
            .as_ref()
            .and_then(|v| v["verdict"]["job"].as_str())
            .map(|s| JobId(s.into()))
        {
            inner.stop_started(r, &job, &r.prov.source).await;
        }
        return true;
    };
    let v = verdict.unwrap_or(serde_json::Value::Null);
    let proved = v["verdict"]["mode"]
        .as_str()
        .zip(v["verdict"]["job"].as_str())
        .map(|(m, j)| (m.to_string(), JobId(j.into())));
    if let Some((mode, job)) = proved
        && inner.runs(&job)
    {
        let signal = w.signals[i].signal.clone();
        // Neither `current` nor kept while it is placed: the guard stops it on a panic.
        w.pending = Some(job.clone());
        let Placement {
            job,
            source,
            placed,
            why,
            sink,
        } = inner.place_decoder(r, &mode, &signal, &job).await;
        inner.disown(&job);
        w.signals[i].handled = true;
        let sink_error = match sink {
            // Stopping: the loop's exit path stops what is pending.
            SinkOutcome::Cancelled => return true,
            SinkOutcome::Lost(why) => {
                // Not listed as decoding: it is gone (left alone until its signal returns).
                w.pending = None;
                inner.stop_started(r, &job, &source).await;
                let summary = format!("lost {mode} at {}: {why}", signal.mhz());
                let e = Event {
                    signal: Some(&signal),
                    job: Some(&job),
                    mode: Some(&mode),
                    source: Some(&source),
                    placed: Some(placed),
                    ..Event::default()
                };
                inner.watch_event(r, "lost", summary, e);
                return true;
            }
            SinkOutcome::Refused(why) => Some(why),
            SinkOutcome::Off | SinkOutcome::On => None,
        };
        let t = now().0;
        // A signal that went while identify ran sends no further `gone`: count its
        // silence from now, so the decoder is still dropped after SILENCE_MS.
        let gone_since_ns = (!w.signals[i].present).then_some(t);
        w.subs.insert(job.clone(), inner.decoder_subs(&job));
        w.decoding.push(Decoding {
            mode: mode.clone(),
            job: job.clone(),
            source: source.clone(),
            signal: signal.clone(),
            since_ns: t,
            placed,
            sink_error: sink_error.clone(),
            last_event_ns: t,
            gone_since_ns,
            confirm_by_ns: None,
        });
        w.pending = None;
        let mut summary = format!(
            "decoding {mode} at {} on {} ({})",
            signal.mhz(),
            source.0,
            placed.as_str()
        );
        if let Some(why) = why {
            summary.push_str(&format!(": {why}"));
        }
        if let Some(why) = sink_error {
            summary.push_str(&format!("; sondehub sink refused: {why}"));
        }
        let e = Event {
            signal: Some(&signal),
            job: Some(&job),
            mode: Some(&mode),
            source: Some(&source),
            placed: Some(placed),
            ..Event::default()
        };
        inner.watch_event(r, "decoding", summary, e);
        return true;
    }
    let tracked = &mut w.signals[i];
    tracked.tries += 1;
    match tracked.tries > RETRY_MAX {
        true => {
            tracked.handled = true;
            tracked.next_retry_ns = None;
        }
        false => {
            tracked.next_retry_ns = Some(now().0.saturating_add(retry_ms() * 1_000_000));
        }
    }
    let label = v["verdict"]["label"].as_str().unwrap_or("unknown");
    let features = v["verdict"]["features"].clone();
    let allocations = v["verdict"]["allocations"].clone();
    let mut summary = format!(
        "unknown at {}: {label}{}{}",
        tracked.signal.mhz(),
        measured(&features),
        register_says(&allocations),
    );
    summary.push_str(&format!(
        " (identify {} of {})",
        tracked.tries,
        RETRY_MAX + 1
    ));
    if v.is_null() {
        summary.push_str("; identify ended without a verdict");
    }
    tracked.features = Some(features.clone());
    tracked.allocations = Some(allocations.clone());
    let origins = v["verdict"]["origins"].clone();
    let signal = tracked.signal.clone();
    let e = Event {
        signal: Some(&signal),
        job: Some(&id),
        features: Some(features),
        allocations: Some(allocations),
        origins: Some(origins),
        ..Event::default()
    };
    inner.watch_event(r, "unknown", summary, e);
    capture::on_unknown(inner, r, w, i).await;
    true
}

/// `<n>` kHz from `hz`: whole kHz bare, else to one decimal.
fn khz(hz: u64) -> String {
    match hz % 1_000 {
        0 => format!("{}", hz / 1_000),
        _ => format!("{:.1}", hz as f64 / 1e3),
    }
}

/// ` <bw> kHz[, <baud> Bd][, duty <d>]` from a verdict's layer-1 `features`, or nothing
/// without a record.
pub(super) fn measured(features: &serde_json::Value) -> String {
    let Some(bw) = features["bandwidth_hz"].as_u64() else {
        return String::new();
    };
    let mut out = format!(" {} kHz", khz(bw));
    if let Some(baud) = features["symbol_rate_baud"].as_u64() {
        out.push_str(&format!(", {baud} Bd"));
    }
    if let Some(duty) = features["duty"].as_f64() {
        out.push_str(&format!(", duty {duty:.2}"));
    }
    out
}

/// A designator's decoded modulation as one word: `FM`, `AM`, `PM`, else its letter.
fn modulation_word(e: &serde_json::Value) -> String {
    let name = e["modulation_name"].as_str().unwrap_or_default();
    let words = [("Frequency", "FM"), ("Amplitude", "AM"), ("Phase", "PM")];
    match words.iter().find(|(start, _)| name.starts_with(start)) {
        Some((_, w)) => (*w).to_string(),
        None => e["modulation"].as_str().unwrap_or("?").to_string(),
    }
}

/// A designator's decoded information type as one word: `data`, `telephony`,
/// `telegraphy`, else its letter.
fn info_word(e: &serde_json::Value) -> String {
    let info = e["info"].as_str().unwrap_or("?");
    match info {
        "D" => "data",
        "E" => "telephony",
        "A" | "B" => "telegraphy",
        other => other,
    }
    .to_string()
}

/// `; register says <EMISSION> (<bw> kHz <mod> <info>) <licensee>, <site>, <d> km` for
/// the nearest `tx` assignment of `allocations` (an identify verdict's, nearest
/// first): without a decoded designator `; register says <licensee>, <site>, <d> km`;
/// nothing when there is no transmitter.
pub(super) fn register_says(allocations: &serde_json::Value) -> String {
    let nearest = allocations
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["role"] == "tx");
    let Some(a) = nearest else {
        return String::new();
    };
    let mut where_: Vec<String> = ["licensee", "site"]
        .iter()
        .filter_map(|k| a[*k].as_str().map(str::to_string))
        .collect();
    if let Some(d) = a["distance_km"].as_f64() {
        where_.push(format!("{d:.0} km"));
    }
    let mut out = String::from("; register says");
    let e = &a["emission_decoded"];
    if let (Some(code), Some(bw)) = (a["emission"].as_str(), e["bandwidth_hz"].as_u64()) {
        out.push_str(&format!(
            " {code} ({} kHz {} {})",
            khz(bw),
            modulation_word(e),
            info_word(e)
        ));
    }
    if !where_.is_empty() {
        out.push(' ');
        out.push_str(&where_.join(", "));
    }
    out
}

/// Note the decoders' events; drop those whose job no longer runs; stop (`lost`) those
/// whose signal has gone and that stayed silent for [`SILENCE_MS`], and re-adopted ones
/// whose signal did not reappear in time. Whether the state changed.
async fn decoders(inner: &Arc<Inner>, r: &Run, w: &mut State) -> bool {
    let t = now().0;
    for d in &mut w.decoding {
        for rx in w.subs.get_mut(&d.job).into_iter().flatten() {
            loop {
                match rx.try_recv() {
                    Ok(f) if matches!(f.payload, Payload::Event(_)) => d.last_event_ns = t,
                    Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        }
    }
    let silence_ns = silence_ms().saturating_mul(1_000_000);
    let mut changed = false;
    let mut kept = Vec::with_capacity(w.decoding.len());
    for d in std::mem::take(&mut w.decoding) {
        // A decoder that died of the watch's port fault stays listed: the loop is about
        // to fail on the same fault, and the next run re-adopts it once recovered.
        if !inner.runs(&d.job) && r.port.fault().is_some() {
            kept.push(d);
            continue;
        }
        if !inner.runs(&d.job) {
            w.subs.remove(&d.job);
            changed = true;
            continue;
        }
        let unconfirmed = d.confirm_by_ns.is_some_and(|by| t >= by);
        let silent = d
            .gone_since_ns
            .is_some_and(|_| t.saturating_sub(d.last_event_ns.max(d.since_ns)) >= silence_ns);
        if !(unconfirmed || silent) {
            kept.push(d);
            continue;
        }
        inner.stop_started(r, &d.job, &d.source).await;
        w.subs.remove(&d.job);
        w.signals
            .retain(|x| !x.signal.overlaps(d.signal.center_hz, d.signal.bandwidth_hz));
        let why = match unconfirmed {
            true => "not seen again",
            false => "gone and silent",
        };
        let summary = format!("lost {} at {}: {why}", d.mode, d.signal.mhz());
        let e = Event {
            signal: Some(&d.signal),
            job: Some(&d.job),
            mode: Some(&d.mode),
            source: Some(&d.source),
            placed: Some(d.placed),
            ..Event::default()
        };
        inner.watch_event(r, "lost", summary, e);
        changed = true;
    }
    w.decoding = kept;
    changed
}

/// With no identify in progress, stop any running job this watch owns that it does not
/// keep (a stale identify run or its child resumed after a restart).
async fn sweep(inner: &Arc<Inner>, r: &Run, w: &State) {
    let strays: Vec<JobId> = inner
        .jobs
        .lock()
        .unwrap()
        .values()
        .filter(|e| e.record.owner.as_ref() == Some(&r.id))
        .filter(|e| e.record.state == JobState::Running && e.cancel.is_some())
        .filter(|e| !w.decoding.iter().any(|d| d.job == e.record.id))
        .filter(|e| !w.capturing.iter().any(|(j, _)| *j == e.record.id))
        .map(|e| e.record.id.clone())
        .collect();
    for id in strays {
        let _ = inner.stop_gated(&id).await;
        // A stale capture child's end is nobody's to report.
        inner.captured.lock().unwrap().remove(&id);
    }
}

enum Next {
    Idle,
    Started,
    Cancelled,
}

/// Start an identify run on the best-scoring queued signal that is due, unless another
/// job's band now overlaps it (it is then dropped). Publishes `identifying`.
async fn next(inner: &Arc<Inner>, r: &Run, w: &mut State) -> Next {
    let t = now().0;
    loop {
        let due = w
            .signals
            .iter()
            .enumerate()
            .filter(|(_, x)| x.present && !x.handled)
            .filter(|(_, x)| x.next_retry_ns.is_none_or(|n| t >= n))
            .max_by(|a, b| a.1.score.total_cmp(&b.1.score).then(b.0.cmp(&a.0)))
            .map(|(i, _)| i);
        let Some(i) = due else {
            return Next::Idle;
        };
        if inner.held_elsewhere(r, w, &w.signals[i].signal) {
            w.signals.remove(i);
            continue;
        }
        let tracked = w.signals[i].clone();
        match inner.start_identify(r, w, &tracked).await {
            Ok(None) => return Next::Cancelled,
            Ok(Some(id)) => {
                let summary = format!("identifying {}", tracked.signal.mhz());
                let e = Event {
                    signal: Some(&tracked.signal),
                    job: Some(&id),
                    ..Event::default()
                };
                inner.watch_event(r, "identifying", summary, e);
                return Next::Started;
            }
            Err(e) => {
                // Not startable now (a conflict, a fault): counted as an unknown try.
                w.current = None;
                let tracked = &mut w.signals[i];
                tracked.tries += 1;
                match tracked.tries > RETRY_MAX {
                    true => tracked.handled = true,
                    false => tracked.next_retry_ns = Some(t.saturating_add(retry_ms() * 1_000_000)),
                }
                let signal = tracked.signal.clone();
                // No identify ran, so no register answer: `allocations` stays null.
                let summary = format!("unknown at {}: identify did not start: {e}", signal.mhz());
                let ev = Event {
                    signal: Some(&signal),
                    ..Event::default()
                };
                inner.watch_event(r, "unknown", summary, ev);
                return Next::Started;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(center_hz: u64, bandwidth_hz: u64) -> Tracked {
        Tracked {
            signal: Signal {
                center_hz,
                bandwidth_hz,
                snr_db: 20.0,
                first_ns: 0,
                last_ns: 0,
            },
            present: true,
            tries: 0,
            next_retry_ns: None,
            handled: false,
            band_hz: 12_500,
            score: 0.0,
            features: None,
            allocations: None,
            captured: false,
            captures: Vec::new(),
        }
    }

    #[test]
    fn a_verdict_goes_to_the_nearest_overlapping_track() {
        // Seen live 2026-11-05: a 22.5 kHz track at 404.0254 MHz also covers the 7 kHz
        // track at 404.0285 MHz, and took every one of its verdicts ("identify 9 of 4").
        let signals = vec![tracked(404_025_377, 22_500), tracked(404_028_540, 7_031)];
        assert_eq!(nearest(&signals, 404_028_540, 1), Some(1));
        assert_eq!(nearest(&signals, 404_025_400, 1), Some(0));
        assert_eq!(nearest(&signals, 404_100_000, 1), None);
    }
}
