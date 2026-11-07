//! The node: owns ports, leases, rings and the stream bus, and runs graphs as
//! persistent jobs that resume after restart.

mod capture;
mod identify;
mod jsonlog;
mod launch;
pub use launch::DRAIN_TIMEOUT_MS;
mod persist;
mod recover;
mod retune;
mod sources;
pub mod watch;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::OwnedMutexGuard;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::bank::{BankFactory, ChannelBank, PFB_MIN_RATE_HZ};
use crate::classify::{Classifier, ClassifierSet};
use crate::graph::{self, Graph, GraphError};
use crate::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use crate::knowledge::KnowledgeProvider;
use crate::lease::{LeaseError, LeaseManager};
use crate::modes::ModeRegistry;
use crate::ops::{
    FM_DEVIATION_DEFAULT_HZ, JobKind, Op, OpError, OpResult, Sideband, WFM_DEVIATION_DEFAULT_HZ,
};
use crate::port::{GainInfo, Port, PortError, Tuning};
use crate::ring::IqRing;
use crate::stage::{Backend, MixInput, StageRegistry};
use crate::stream::{Frame, Payload, StreamBus};
use crate::types::{
    Capabilities, Hz, JobId, PositionFrom, Provenance, SourceId, SourcePosition, Span, StreamId,
    StreamKind, Timestamp,
};

pub use crate::ops::OPS_RESULT_LIMIT_BYTES;

pub(crate) use launch::AfterGate;
use launch::{feed_bank, feed_ring};
use persist::{io, load, now, persist};
pub use sources::{GainSetting, SOURCE_RECORD_FORMAT, SourceRecord, load_source};
use sources::{load_sources, persist_source};

const BUS_CAPACITY: usize = 256;

/// Provenance source of the node's own streams (the `ops` stream).
pub const NODE_SOURCE: &str = "node";

fn node_provenance(span: Span) -> Provenance {
    Provenance {
        source: SourceId(NODE_SOURCE.into()),
        job: None,
        channel: None,
        span,
    }
}

/// Ring history in seconds for a source at `rate_hz` within `budget_bytes`
/// (`Complex<f32>`, 8 bytes per sample): `budget / (rate * 8)` clamped to
/// `1..=max_seconds`.
pub fn ring_seconds_for(budget_bytes: u64, rate_hz: u64, max_seconds: f64) -> f64 {
    let s = budget_bytes as f64 / (rate_hz.max(1) as f64 * 8.0);
    s.max(1.0).min(max_seconds)
}

pub struct NodeConfig {
    pub state_dir: PathBuf,
    /// Longest ring history in seconds; a fast source gets less, see [`ring_seconds_for`].
    pub ring_seconds: f64,
    /// Memory budget for one source's IQ ring (8 bytes per sample).
    pub ring_budget_bytes: u64,
    pub backends: Vec<Backend>,
    /// Relaunch `Running` jobs on `open` (a long-lived node). When false, records
    /// are loaded as persisted and nothing is launched, leased or rewritten.
    pub resume: bool,
    /// How long a `scan` job dwells on each step; normally
    /// [`crate::ops::SCAN_DWELL_MS`], shorter in tests.
    pub scan_dwell_ms: u64,
    /// Makes the channel banks of wide sources; installed before any port is
    /// added, so resumed jobs get a bank as fresh ones do. `None`: no banks
    /// (until [`Node::set_bank_factory`]).
    pub bank_factory: Option<Arc<dyn BankFactory>>,
    /// The operator's mode rows (`--modes-dir`), read after the embedded ones; `None`
    /// reads none (the CLI resolves the default, [`crate::modes::default_operator_dir`]).
    /// A missing directory reads as empty.
    pub modes_dir: Option<PathBuf>,
    /// What the burst store (`state_dir/bursts`) keeps; `BurstPolicy::default()` is
    /// 256 MiB and 7 days.
    pub burst_policy: crate::bursts::BurstPolicy,
    /// Where the operator said sources are (`--source-position`); a source that
    /// reports its own position (`Port::position`) overrides its entry.
    pub source_positions: BTreeMap<SourceId, SourcePosition>,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("unknown source {}", .0.0)]
    UnknownSource(SourceId),
    #[error("unknown job {}", .0.0)]
    UnknownJob(JobId),
    #[error("source {} is already attached", .0.0)]
    DuplicateSource(SourceId),
    #[error(transparent)]
    Graph(#[from] GraphError),
    #[error(transparent)]
    Lease(#[from] LeaseError),
    /// The source's port refused the tune a job's lease needed (a faulted device that
    /// cannot be opened, a band outside its range). Displayed under the source's name,
    /// as [`source_fault_reason`] words it, so a `job_start` against a faulted source
    /// reads the same as a job that failed on it.
    #[error("{}", source_fault_reason(.0, .1))]
    SourcePort(SourceId, PortError),
    #[error("io: {0}")]
    Io(String),
    /// An attached job's input job is not running in this node.
    #[error("input job {} not running", .0.0)]
    InputNotRunning(JobId),
    /// An attached job's input job runs but has no audio stream to read.
    #[error("input job {} has no audio stream", .0.0)]
    NoAudioStream(JobId),
    /// A built-in job's graph could not be re-expanded for its leased capture's rate.
    #[error("{0}")]
    Expand(OpError),
    /// An elastic job's relaunch found it stopped meanwhile; it stays stopped.
    #[error("job {} was stopped", .0.0)]
    Stopped(JobId),
    /// A request the node refuses, with the reason (a `replace` naming a job
    /// that does not hold the source).
    #[error("{0}")]
    Other(String),
}

/// How many stopped or failed job records the node keeps: when a job ends
/// and more than this many are terminal, the oldest beyond the cap are
/// forgotten and their files removed. Running jobs are never counted.
pub const JOB_HISTORY_MAX: usize = 20;

/// How long [`Node::replace_holders`] waits, holding the source's gate, for a
/// stopped job's lease to go.
const REPLACE_RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

struct JobEntry {
    record: JobRecord,
    /// The run this entry is for: each launch of the job (a start, a resume, a
    /// `graph_put` restart, an elastic relaunch) records a new one. Two launches of
    /// one id can overlap (two `graph_put`s at once: the second's stop finds the job
    /// already `Stopped` and returns while the first's old run is still ending), so
    /// a supervisor ends, releases and records only the run it was spawned for and
    /// never a later run that took the id meanwhile. 0 for a record loaded from disk
    /// and not launched.
    run: u64,
    /// When the record turned terminal in this process (its `created_ns` for
    /// one loaded terminal from disk); history is pruned oldest-ended first.
    ended_ns: Timestamp,
    cancel: Option<CancellationToken>,
    /// The supervisor; it releases the lease and unregisters the job's streams.
    done: Option<JoinHandle<()>>,
    /// Closes when run `run`'s supervisor has released the lease and unregistered
    /// the streams (or kept them for a relaunch), before it idles the port. A stop
    /// that finds `done` already taken by another stop waits on this instead, so
    /// every stop returns with the run released and a launch that follows never
    /// meets the old run's supervisor releasing the new lease (both are keyed by
    /// the job id). `None` for a record loaded from disk and not launched.
    ended: Option<tokio::sync::watch::Receiver<()>>,
    /// The job's output streams (those no edge of its graph consumes), in graph order
    /// (each node's in its stage's output order), set at launch; an attached job reads
    /// one of these.
    outputs: Vec<(StreamKind, StreamId)>,
    /// Set at launch for a run nothing in which polls its port's fault: it has no IQ
    /// forwarder and no controller loop, and is not attached. A fault does not end it, so
    /// a recovery does not wait for it (see [`Inner::wait_ended`]).
    blind: bool,
    /// Set while the job is relaunched under its id, by [`Inner::relaunch_elastic`] (an
    /// elastic job on a new capture), by [`Inner::relaunch_in_place`] (a
    /// `job_retune`) or by [`Inner::hold_attached`] (an attached job whose input is
    /// retuned or restarted by `graph_put`): its supervisor then leaves the record
    /// `Running` and the lease held for the relaunch.
    relaunch: bool,
    /// Set by a stop under the source's gate ([`Inner::stop_gated`], a `replace`):
    /// the supervisor then does not idle the port, which would take that gate, and
    /// a launch of the job already past its lease (a `graph_put` restart) is
    /// refused, so the replaced job stays stopped (see [`Inner::record_running`]).
    gated_stop: bool,
    /// Set by the supervisor once the lease is released and the streams are
    /// unregistered, when all that is left is the port's idle.
    released: bool,
    /// Set by a `graph_put` before it stops the run to relaunch it under the same id:
    /// a controller (`watch`) then leaves what it started for the next run to re-adopt
    /// instead of stopping it as a `job_stop` does.
    restarting: bool,
}

impl JobEntry {
    /// The entry of a record that is not running in this process (loaded from disk and
    /// not launched, or whose launch failed), ended at `ended_ns` for the history prune.
    fn loaded(record: JobRecord, ended_ns: Timestamp) -> JobEntry {
        JobEntry {
            ended_ns,
            record,
            run: 0,
            cancel: None,
            done: None,
            ended: None,
            outputs: Vec::new(),
            blind: false,
            relaunch: false,
            gated_stop: false,
            released: false,
            restarting: false,
        }
    }
}

/// A source's bank for the capture band it was made for; `bank` is `None`
/// when that band is narrower than [`PFB_MIN_RATE_HZ`] or the factory declined.
struct BankSlot {
    band: Tuning,
    bank: Option<Arc<dyn ChannelBank>>,
}

/// The node's three JSON-lines logs in the state directory.
#[derive(Clone, Copy)]
enum LogFile {
    Ops,
    Transcripts,
    Events,
}

impl LogFile {
    fn name(self) -> &'static str {
        match self {
            LogFile::Ops => "ops.log",
            LogFile::Transcripts => "transcripts.log",
            LogFile::Events => "events.log",
        }
    }
}

/// The last audio stream of `outputs` (a job's, in graph order): what is read of the job.
fn last_audio(outputs: &[(StreamKind, StreamId)]) -> Option<StreamId> {
    outputs
        .iter()
        .rev()
        .find(|(k, _)| *k == StreamKind::Audio)
        .map(|(_, s)| s.clone())
}

/// Whether a mix may read `rec`: not an attached job (it is a chain over a job's audio),
/// a job a controller owns (a trial), another mix (a `mix` job, or any job whose graph
/// holds a `mix` node, as a `graph_start` of one does) or a job centred on a plan
/// channel that carries no voice (CB ch22 and ch23: data, its noise is no one talking).
fn mixable(rec: &JobRecord) -> bool {
    rec.input.is_none()
        && rec.owner.is_none()
        && crate::plans::no_voice_at(rec.tuning.center_hz.0).is_none()
        && rec.kind != JobKind::Mix.name()
        && !rec
            .graph
            .nodes
            .iter()
            .any(|n| n.kind == JobKind::Mix.name())
}

struct Inner {
    state_dir: PathBuf,
    /// The node's station, loaded at `open` and written through on every change.
    station: Mutex<Option<crate::station::Station>>,
    jobs_dir: PathBuf,
    /// `state_dir/sources`, one [`SourceRecord`] per source.
    sources_dir: PathBuf,
    /// The source records, loaded at `open` (those of absent sources included,
    /// kept untouched) and written through on every change.
    sources: Mutex<HashMap<SourceId, SourceRecord>>,
    ring_seconds: f64,
    ring_budget_bytes: u64,
    scan_dwell_ms: u64,
    /// Where the operator said sources are (`--source-position`).
    source_positions: BTreeMap<SourceId, SourcePosition>,
    backends: Vec<Backend>,
    registry: StageRegistry,
    /// The decoder modes, loaded at `open` against `registry`.
    modes: Arc<ModeRegistry>,
    /// The page themes: the built-in ones until the caller sets its own with `set_themes`.
    themes: RwLock<Arc<crate::themes::ThemeRegistry>>,
    /// The `capture` jobs' windows, opened (and expired) at `open`.
    bursts: Arc<crate::bursts::BurstStore>,
    leases: LeaseManager,
    bus: Arc<StreamBus>,
    ports: RwLock<HashMap<SourceId, Arc<dyn Port>>>,
    rings: Mutex<HashMap<SourceId, Arc<RwLock<IqRing>>>>,
    bank_factory: RwLock<Option<Arc<dyn BankFactory>>>,
    banks: Mutex<HashMap<SourceId, BankSlot>>,
    jobs: Mutex<HashMap<JobId, JobEntry>>,
    knowledge: RwLock<Vec<Arc<dyn KnowledgeProvider>>>,
    classifiers: RwLock<ClassifierSet>,
    /// Failures to persist a job's natural end; memory and disk disagree for these.
    persist_errors: Mutex<Vec<String>>,
    /// Every applied op, one JSON line each; see [`jsonlog`].
    ops_log: jsonlog::JsonLog,
    /// Every `transcript` event of a `transcribe` job, one JSON line each; see
    /// [`Inner::tee_transcripts`].
    transcripts_log: jsonlog::JsonLog,
    /// Every event frame of every source's feed, one JSON line each; see
    /// [`Inner::tee_events`].
    events_log: jsonlog::JsonLog,
    /// Per [`LogFile`]: whether a failed write is already in `persist_errors`.
    log_failed: [std::sync::atomic::AtomicBool; 3],
    /// Running transcript tees (see [`Inner::tee_transcripts`]).
    tees: Arc<std::sync::atomic::AtomicUsize>,
    /// Running tasks of every job's stage set (its stages, its IQ forwarder, a
    /// scan's step loop), so a run left behind by a launch shows (see `StageRunning`).
    stages: Arc<std::sync::atomic::AtomicUsize>,
    /// The last run handed out (see [`JobEntry::run`]).
    runs: std::sync::atomic::AtomicU64,
    /// Held across [`Inner::relaunch_elastic`], so two retunes never relaunch one
    /// elastic job at once.
    relaunching: tokio::sync::Mutex<()>,
    /// Held across [`Node::mix_start`], so two starts of one set make one mix.
    mix_gate: tokio::sync::Mutex<()>,
    /// Cancelled when the `Node` is dropped; every job token is a child of it.
    shutdown: CancellationToken,
    /// The verdicts of owned `identify` runs, by identify id, until their owner (a
    /// `watch`) takes them: an owned record is dropped as soon as it ends, context and
    /// all (see [`Inner::prune_history`]).
    verdicts: Mutex<HashMap<JobId, serde_json::Value>>,
    /// How owned `capture` runs ended (`{capture, failed}`, see [`capture::capture_finished`]),
    /// by capture id, until their watch takes them: as `verdicts`, the record goes at its end.
    captured: Mutex<HashMap<JobId, serde_json::Value>>,
    /// The open fault episode of each source whose port the node has seen faulted and
    /// not yet recovered (see [`recover::Episode`]). Locked after `jobs` when both are
    /// held, never the other way round.
    episodes: Mutex<HashMap<SourceId, recover::Episode>>,
}

impl Inner {
    /// The kind a job recorded as `kind` was expanded from, when the node expands its
    /// graph: a built-in kind, or a `Mode` the registry has a row for. A record of any
    /// other kind (a [`Node::job_start`] caller's own name) keeps its graph as stored.
    pub(crate) fn expandable_kind(&self, kind: &str) -> Option<JobKind> {
        JobKind::from_name(kind)
            .filter(|k| !matches!(k, JobKind::Mode(id) if self.modes.get(&id.0).is_none()))
    }

    /// Append `line` to the log `which`. A failure never fails anything; the
    /// first per log is recorded in `persist_errors` as `<name>: <e>`, later
    /// ones are dropped.
    fn log_line(&self, which: LogFile, line: &serde_json::Value) {
        let log = match which {
            LogFile::Ops => &self.ops_log,
            LogFile::Transcripts => &self.transcripts_log,
            LogFile::Events => &self.events_log,
        };
        if let Err(e) = log.append(line)
            && !self.log_failed[which as usize].swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            self.persist_errors
                .lock()
                .unwrap()
                .push(format!("{}: {e}", which.name()));
        }
    }

    /// Cache a worker port's capabilities in its source record, so a later start
    /// with the device absent can still list the source. A port that runs in the
    /// node (`Port::worker` is `None`) has nothing to cache and writes no record.
    /// A failed write goes to `persist_errors`; the memory record is then left as
    /// it was.
    pub(crate) fn note_capabilities(&self, port: &dyn Port) {
        if port.worker().is_none() {
            return;
        }
        let source = port.id();
        let mut sources = self.sources.lock().unwrap();
        let mut rec = sources
            .get(source)
            .cloned()
            .unwrap_or_else(SourceRecord::new);
        rec.format = SOURCE_RECORD_FORMAT;
        rec.capabilities = Some(port.capabilities().clone());
        match persist_source(&self.sources_dir, source, &rec) {
            Ok(()) => {
                sources.insert(source.clone(), rec);
            }
            Err(e) => self
                .persist_errors
                .lock()
                .unwrap()
                .push(format!("source record {}: {e}", source.0)),
        }
    }

    /// See [`Node::job_stop`].
    async fn stop(&self, id: &JobId) -> Result<(), NodeError> {
        self.stop_with(id, false).await
    }

    /// Store `graph` as job `id`'s (`capture_rate_hz` as in [`Node::job_graph_set`]) and,
    /// when it `running`, restart it under the same id.
    pub(crate) async fn swap_graph(
        self: &Arc<Self>,
        id: &JobId,
        graph: Graph,
        capture_rate_hz: Option<Option<u64>>,
        running: bool,
    ) -> Result<(), NodeError> {
        // Before the stop: the jobs attached to it follow the restart (see
        // `Inner::relaunch_attached`), relaunched once it runs again or failed to.
        let held = match running {
            true => self.hold_attached(id),
            false => Vec::new(),
        };
        let rec = match self
            .store_graph(id, graph, capture_rate_hz, running.then_some(false))
            .await
        {
            Ok(rec) => rec,
            Err(e) => {
                self.relaunch_attached(held).await;
                return Err(e);
            }
        };
        if running {
            let r = self
                .launch_task(rec)
                .await
                .map_err(|e| NodeError::Other(format!("launch task: {e}")));
            self.relaunch_attached(held).await;
            r??;
        }
        Ok(())
    }

    /// The first half of [`Inner::swap_graph`]: stop the job when `stop` is `Some`
    /// (`Some(true)`: gated, see [`Inner::stop_gated`]; a controller's run tells this
    /// restart from a `job_stop` by a flag), persist `graph` as its own and
    /// return the record, for the caller to launch.
    pub(crate) async fn store_graph(
        self: &Arc<Self>,
        id: &JobId,
        graph: Graph,
        capture_rate_hz: Option<Option<u64>>,
        stop: Option<bool>,
    ) -> Result<JobRecord, NodeError> {
        if let Some(gated) = stop {
            if let Some(e) = self.jobs.lock().unwrap().get_mut(id) {
                e.restarting = true;
            }
            if let Err(e) = self.stop_with(id, gated).await {
                if let Some(e) = self.jobs.lock().unwrap().get_mut(id) {
                    e.restarting = false;
                }
                return Err(e);
            }
        }
        let mut jobs = self.jobs.lock().unwrap();
        let entry = jobs
            .get_mut(id)
            .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
        let rec = JobRecord {
            graph,
            capture_rate_hz: capture_rate_hz.flatten(),
            operator_graph: capture_rate_hz.is_none(),
            ..entry.record.clone()
        };
        persist(&self.jobs_dir, &rec)?;
        entry.record = rec.clone();
        // The gated stop was this restart's own, not a `replace`'s: the launch that
        // follows must not be refused as undoing it (see `record_running`).
        if stop == Some(true) {
            entry.gated_stop = false;
        }
        Ok(rec)
    }

    /// [`Inner::stop`] while the caller holds the job's source gate: the port
    /// is not idled (the caller's request is about to tune it, and the idle
    /// would wait on that gate), and a supervisor already past its cleanup and
    /// waiting on the gate to idle is not awaited. Either way the job's lease is
    /// released and its streams unregistered when this returns, unless another
    /// stop took the supervisor first (see [`Node::replace_holders`]).
    async fn stop_gated(&self, id: &JobId) -> Result<(), NodeError> {
        self.stop_with(id, true).await
    }

    async fn stop_with(&self, id: &JobId, gated: bool) -> Result<(), NodeError> {
        let done = {
            let mut jobs = self.jobs.lock().unwrap();
            let entry = jobs
                .get_mut(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            if !matches!(
                entry.record.state,
                JobState::Failed { .. } | JobState::Stopped
            ) {
                let rec = JobRecord {
                    state: JobState::Stopped,
                    ..entry.record.clone()
                };
                persist(&self.jobs_dir, &rec)?;
                entry.record = rec;
                entry.ended_ns = now();
            }
            // Set before the cancel, under the jobs lock the supervisor reads it under.
            entry.gated_stop |= gated;
            if let Some(c) = entry.cancel.take() {
                c.cancel();
            }
            let done = entry.done.take();
            // A supervisor that already released may be waiting on the gate to idle.
            let done = done.filter(|_| !(gated && entry.released));
            let ended = entry.ended.clone();
            self.prune_history(&mut jobs);
            (done, ended)
        };
        if let Some(h) = done.0 {
            let _ = h.await;
        }
        // Another stop (or a relaunch) took the supervisor: wait for the run to
        // release all the same. Closed already once it has, so a gated stop never
        // waits here on a supervisor that waits on the gate to idle.
        if let Some(mut ended) = done.1 {
            let _ = ended.changed().await;
        }
        Ok(())
    }

    /// Forget the terminal records that ended earliest beyond
    /// [`JOB_HISTORY_MAX`], removing their files (best effort: a file that
    /// will not go is retried at the next prune). Called with `jobs` locked
    /// after a record turns terminal; that record ended last, so it survives.
    /// A record the heal of its source would relaunch (recoverable in the
    /// source's open fault episode) is never forgotten while the episode lasts:
    /// it still counts toward the cap, and older ones go in its place.
    fn prune_history(&self, jobs: &mut HashMap<JobId, JobEntry>) {
        let jobs_dir = &self.jobs_dir;
        let is_terminal =
            |e: &JobEntry| matches!(e.record.state, JobState::Stopped | JobState::Failed { .. });
        // An owned record (a watch's identify runs and their children) goes as soon as
        // it ends: it never takes one of the kept slots. Once its run has released (or
        // it was loaded, never launched), so its supervisor still finds the entry and
        // honours a gated stop (no idle under a gate the stopper holds).
        let owned: Vec<JobId> = jobs
            .values()
            .filter(|e| e.record.owner.is_some() && is_terminal(e))
            .filter(|e| e.cancel.is_none() && (e.released || e.run == 0))
            .map(|e| e.record.id.clone())
            .collect();
        for id in owned {
            jobs.remove(&id);
            let _ = std::fs::remove_file(jobs_dir.join(format!("{}.json", id.0)));
        }
        let terminal: Vec<&JobEntry> = jobs
            .values()
            .filter(|e| is_terminal(e) && e.record.owner.is_none())
            .collect();
        if terminal.len() <= JOB_HISTORY_MAX {
            return;
        }
        let over = terminal.len() - JOB_HISTORY_MAX;
        let mut candidates: Vec<(Timestamp, JobId)> = terminal
            .into_iter()
            .filter(|e| !self.keep_from_prune(jobs, e))
            .map(|e| (e.ended_ns, e.record.id.clone()))
            .collect();
        candidates.sort_by(|a, b| a.0.0.cmp(&b.0.0).then_with(|| a.1.0.cmp(&b.1.0)));
        for (_, id) in candidates.iter().take(over) {
            jobs.remove(id);
            let _ = std::fs::remove_file(jobs_dir.join(format!("{}.json", id.0)));
        }
    }

    /// See [`Node::job_context_merge`].
    fn context_merge(&self, id: &JobId, patch: serde_json::Value) -> Result<(), NodeError> {
        let mut jobs = self.jobs.lock().unwrap();
        let entry = jobs
            .get_mut(id)
            .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
        let ctx = &mut entry.record.context;
        match patch {
            serde_json::Value::Object(p) => {
                if !ctx.is_object() {
                    *ctx = serde_json::json!({});
                }
                let obj = ctx.as_object_mut().expect("context is an object");
                for (k, v) in p {
                    if v.is_null() {
                        obj.remove(&k);
                    } else {
                        obj.insert(k, v);
                    }
                }
            }
            other => *ctx = other,
        }
        persist(&self.jobs_dir, &entry.record)
    }

    /// The audio stream an attached job reads from `job`: the last audio output stream
    /// in its graph order (an audio edge between two of its stages does not count).
    /// `InputNotRunning` unless `job` runs in this node (a record loaded without resume
    /// does not), `NoAudioStream` when it outputs no audio.
    fn audio_stream(&self, job: &JobId) -> Result<StreamId, NodeError> {
        let jobs = self.jobs.lock().unwrap();
        let entry = jobs
            .get(job)
            .filter(|e| e.record.state == JobState::Running && e.cancel.is_some())
            .ok_or_else(|| NodeError::InputNotRunning(job.clone()))?;
        last_audio(&entry.outputs).ok_or_else(|| NodeError::NoAudioStream(job.clone()))
    }

    /// What a `mix` stage of the job `except` may read: the audio output of every job
    /// with a live run that is [`mixable`]. A run that ended is not listed, so the stage
    /// never holds a closed stream with no replacement.
    pub(super) fn mix_inputs(&self, except: &JobId) -> Vec<MixInput> {
        let jobs = self.jobs.lock().unwrap();
        jobs.values()
            .filter(|e| {
                e.record.state == JobState::Running
                    && e.cancel.is_some()
                    && mixable(&e.record)
                    && &e.record.id != except
            })
            .filter_map(|e| {
                Some(MixInput {
                    job: e.record.id.clone(),
                    stream: last_audio(&e.outputs)?,
                    source: e.record.source.clone(),
                    center_hz: e.record.tuning.center_hz.0,
                })
            })
            .collect()
    }

    /// The bin rate of `source`'s bank for the capture `band` (made now if
    /// the band is new), or `None` when that band gets no bank.
    fn bank_bin_rate(&self, source: &SourceId, band: Tuning) -> Option<u64> {
        self.sync_bank(source, band).map(|b| 2 * b.bin_hz())
    }

    /// `source`'s bank for the capture `band`: the kept one when it was made
    /// for that band, else a new one from the factory (none below
    /// [`PFB_MIN_RATE_HZ`]) replacing it. Without a factory nothing is kept.
    /// While a scan holds the source (a time-shared lease) the band changes
    /// every step and nothing can subscribe, so no bank is made or replaced:
    /// the first block after the scan makes the one for the band it left.
    fn sync_bank(&self, source: &SourceId, band: Tuning) -> Option<Arc<dyn ChannelBank>> {
        let mut banks = self.banks.lock().unwrap();
        if let Some(slot) = banks.get(source)
            && slot.band == band
        {
            return slot.bank.clone();
        }
        if self.leases.time_shared(source) {
            return None;
        }
        let factory = self.bank_factory.read().unwrap().clone()?;
        let bank = (band.bandwidth_hz.0 >= PFB_MIN_RATE_HZ)
            .then(|| factory.make(band.bandwidth_hz, band.center_hz))
            .flatten();
        banks.insert(
            source.clone(),
            BankSlot {
                band,
                bank: bank.clone(),
            },
        );
        bank
    }
}

/// The capture a source's resumable rigid records shared: the widest one's band
/// when it contains the others (the capture it tuned), else the band bounding them
/// all when the port can capture it (no wider than `max_bandwidth_hz`, its centre in
/// the tune range), else an elastic record's band that contains them all. `None`:
/// no rigid record, or no such band (each launch then tunes as before).
fn resume_capture(
    caps: &Capabilities,
    rigid: &[Tuning],
    elastic: impl IntoIterator<Item = Tuning>,
) -> Option<Tuning> {
    let edges = |t: &Tuning| {
        let half = t.bandwidth_hz.0 / 2;
        (
            t.center_hz.0.saturating_sub(half),
            t.center_hz.0.saturating_add(half),
        )
    };
    let contains = |outer: &Tuning| {
        let (ol, oh) = edges(outer);
        rigid.iter().all(|t| {
            let (l, h) = edges(t);
            l >= ol && h <= oh
        })
    };
    let widest = rigid.iter().max_by_key(|t| t.bandwidth_hz.0)?;
    if contains(widest) {
        return Some(*widest);
    }
    let lo = rigid.iter().map(|t| edges(t).0).min()?;
    let hi = rigid.iter().map(|t| edges(t).1).max()?;
    let bounding = Tuning {
        center_hz: Hz(lo + (hi - lo) / 2),
        bandwidth_hz: Hz(hi - lo),
    };
    let tunable =
        (caps.tune_min_hz.value().0..=caps.tune_max_hz.value().0).contains(&bounding.center_hz.0);
    if tunable && bounding.bandwidth_hz.0 <= caps.max_bandwidth_hz.value().0 {
        return Some(bounding);
    }
    elastic.into_iter().find(|t| contains(t))
}

/// A job's failure caused by its source's port, `source <id>: <error>`: the reason every
/// fault-caused failure carries (a stage that saw the fault, a launch whose tune the port
/// refused), so the node's recovery of the source can find the jobs it should relaunch.
pub(crate) fn source_fault_reason(source: &SourceId, e: &dyn std::fmt::Display) -> String {
    format!("source {}: {e}", source.0)
}

/// A port's refusal of a gain as the node reports it: an `Unsupported` text
/// verbatim (it names the source and the range), an I/O failure as `Io`.
fn gain_error(e: PortError) -> NodeError {
    match e {
        PortError::Unsupported(m) => NodeError::Other(m),
        PortError::Io(m) => NodeError::Io(m),
        e => NodeError::Other(e.to_string()),
    }
}

/// Dropping the node cancels its tasks without touching persisted state, so
/// `Running` jobs resume on the next `open`.
pub struct Node {
    inner: Arc<Inner>,
    load_errors: Vec<String>,
    /// How many of `load_errors` (the first) are job-record files.
    job_load_errors: usize,
    /// The `ops` event stream: one frame per [`Node::apply`].
    ops_stream: StreamId,
}

impl Drop for Node {
    fn drop(&mut self) {
        self.inner.shutdown.cancel();
    }
}

/// The lease [`Node::hold_band`] took, under an id that is no job: released when
/// this is dropped, whether the caller lets it go on its normal path or its future
/// is dropped between hold and release (a timed-out or disconnected request), so
/// the lease never outlives the request. The release takes no gate and tunes
/// nothing (`LeaseManager::release`).
pub(crate) struct BandHold<'a> {
    node: &'a Node,
    id: JobId,
}

impl Drop for BandHold<'_> {
    fn drop(&mut self) {
        self.node.inner.leases.release(&self.id);
    }
}

impl Node {
    /// Open the node on `cfg.state_dir`, attach `ports`, and, when
    /// `cfg.resume`, resume every `Running` job and every job that `Failed`
    /// because its source was absent and is now attached (same id, context
    /// and creation time): one whose source is absent or cannot be resumed is
    /// kept as `Failed`. A source's capture is restored first (the band its
    /// rigid jobs shared, see [`resume_capture`]), then its jobs resume, a scan
    /// first of all, then widest band first (ties oldest first); one that
    /// still does not fit fails with the `conflict` reason. Elastic jobs
    /// resume after every rigid one and join the capture they left. Attached jobs
    /// resume after every job on the radio, so their input runs first; one
    /// whose input does not run is kept as `Failed` with `input job <id> not
    /// running`, and one that failed that way resumes once its input runs.
    /// Without `cfg.resume` every record is loaded as persisted, untouched.
    /// Either way, before any of that, each attached source pinned in its record
    /// (`state_dir/sources`) is held at its pin (see [`Node::source_pin`]), so a
    /// one-shot node honours the pins too.
    /// `open` itself fails only on
    /// state-directory I/O and on two ports sharing a source id
    /// (`DuplicateSource`).
    pub async fn open(
        cfg: NodeConfig,
        registry: StageRegistry,
        ports: Vec<Arc<dyn Port>>,
    ) -> Result<Self, NodeError> {
        Self::open_with_modes(cfg, registry, ports, None).await
    }

    /// [`Node::open`] with the mode registry `modes` already loaded (and its program rows
    /// probed) by the caller; `None` loads it from `cfg.modes_dir` against `registry`, as
    /// `open` does.
    pub async fn open_with_modes(
        cfg: NodeConfig,
        registry: StageRegistry,
        ports: Vec<Arc<dyn Port>>,
        modes: Option<Arc<ModeRegistry>>,
    ) -> Result<Self, NodeError> {
        let jobs_dir = cfg.state_dir.join("jobs");
        std::fs::create_dir_all(&jobs_dir).map_err(io)?;
        let (records, mut load_errors) = load(&jobs_dir)?;
        let job_load_errors = load_errors.len();
        let (station, station_error) = crate::station::load(&cfg.state_dir);
        load_errors.extend(station_error);
        let sources_dir = cfg.state_dir.join("sources");
        let (source_records, source_errors) = load_sources(&sources_dir);
        load_errors.extend(source_errors);
        let resume = cfg.resume;
        let bursts =
            crate::bursts::BurstStore::open(&cfg.state_dir, cfg.burst_policy).map_err(io)?;
        if cfg.burst_policy.expire_at_open {
            bursts.expire();
        }
        let modes = modes
            .unwrap_or_else(|| Arc::new(ModeRegistry::load(&registry, cfg.modes_dir.as_deref())));
        let bus = Arc::new(StreamBus::new(BUS_CAPACITY));
        // Registered before jobs resume; their internal applies bypass
        // `Node::apply` and are never logged.
        let t = now();
        let ops_stream = bus.register(
            StreamKind::Event,
            node_provenance(Span {
                start_ns: t,
                end_ns: t,
            }),
        );
        let mut node = Node {
            inner: Arc::new(Inner {
                state_dir: cfg.state_dir.clone(),
                station: Mutex::new(station),
                jobs_dir,
                sources_dir,
                sources: Mutex::new(source_records),
                ring_seconds: cfg.ring_seconds,
                ring_budget_bytes: cfg.ring_budget_bytes,
                scan_dwell_ms: cfg.scan_dwell_ms,
                source_positions: cfg.source_positions,
                backends: cfg.backends,
                registry,
                modes,
                themes: RwLock::new(Arc::new(crate::themes::ThemeRegistry::load(None))),
                bursts,
                leases: LeaseManager::new(),
                bus,
                ports: RwLock::new(HashMap::new()),
                rings: Mutex::new(HashMap::new()),
                bank_factory: RwLock::new(cfg.bank_factory),
                banks: Mutex::new(HashMap::new()),
                jobs: Mutex::new(HashMap::new()),
                knowledge: RwLock::new(Vec::new()),
                classifiers: RwLock::new(ClassifierSet::default()),
                persist_errors: Mutex::new(Vec::new()),
                ops_log: jsonlog::JsonLog::new(cfg.state_dir.join("ops.log")),
                transcripts_log: jsonlog::JsonLog::new(cfg.state_dir.join("transcripts.log")),
                events_log: jsonlog::JsonLog::with_max_bytes(
                    cfg.state_dir.join("events.log"),
                    jsonlog::EVENTS_MAX_BYTES,
                ),
                log_failed: Default::default(),
                tees: Arc::default(),
                stages: Arc::default(),
                runs: std::sync::atomic::AtomicU64::new(0),
                relaunching: tokio::sync::Mutex::new(()),
                mix_gate: tokio::sync::Mutex::new(()),
                shutdown: CancellationToken::new(),
                verdicts: Mutex::new(HashMap::new()),
                captured: Mutex::new(HashMap::new()),
                episodes: Mutex::new(HashMap::new()),
            }),
            load_errors,
            job_load_errors,
            ops_stream,
        };
        for port in ports {
            node.add_port(port)?;
        }
        // Gains before anything tunes, then pins: each pinned source is held at
        // its band before any record resumes, so the records fit inside it or
        // conflict as they would live.
        node.restore_gains().await;
        node.restore_pins().await;
        // The records in resume order (see `Inner::resume_records`); without `resume`
        // none is launched and every one is loaded as persisted.
        let resumed = node
            .inner
            .resume_records(records, None, |inner, rec| resume && inner.resumable(rec))
            .await;
        if let Some(e) = resumed.error {
            return Err(e);
        }
        {
            let mut jobs = node.inner.jobs.lock().unwrap();
            for rec in resumed.skipped {
                let created = rec.created_ns;
                jobs.insert(rec.id.clone(), JobEntry::loaded(rec, created));
            }
        }
        for (holder, port) in resumed.holds {
            node.inner.leases.release(&holder);
            node.inner.leases.idle_if_unheld(port.as_ref()).await;
        }
        {
            let mut jobs = node.inner.jobs.lock().unwrap();
            node.inner.prune_history(&mut jobs);
        }
        Ok(node)
    }

    /// Set each attached source whose record has a gain to it (see
    /// [`Node::source_set_gain`]); a refusal is an entry `source <id>: gain:
    /// <reason>` in `load_errors`, the record left on disk as it was.
    async fn restore_gains(&mut self) {
        let mut gains: Vec<(SourceId, Option<f64>)> = self
            .inner
            .sources
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(id, r)| r.gain.as_ref().map(|g| (id.clone(), g.db)))
            .collect();
        gains.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        for (id, db) in gains {
            let Some(port) = self.port(&id) else {
                continue;
            };
            if let Err(e) = port.set_gain(db).await {
                self.load_errors
                    .push(format!("source {}: gain: {}", id.0, gain_error(e)));
            }
        }
    }

    /// Lease each attached source whose record has a pin at that band (see
    /// [`Node::source_pin`]). A pin that cannot be taken (its tune fails) is an
    /// entry `source <id>: pin <center_hz>/<bandwidth_hz> Hz: <reason>` in
    /// `load_errors`; the record stays on disk as it was.
    async fn restore_pins(&mut self) {
        let mut pins: Vec<(SourceId, Tuning)> = self
            .inner
            .sources
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(id, r)| r.pin.map(|t| (id.clone(), t)))
            .collect();
        pins.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        for (id, t) in pins {
            let Some(port) = self.port(&id) else {
                continue;
            };
            if let Err(e) = self.inner.leases.pin(port.as_ref(), t).await {
                self.load_errors.push(format!(
                    "source {}: pin {}/{} Hz: {e}",
                    id.0, t.center_hz.0, t.bandwidth_hz.0
                ));
            }
        }
    }

    /// Files in `state_dir/jobs` that could not be read as a `JobRecord`, or
    /// whose record id is not a ULID matching the file name; files in
    /// `state_dir/sources` that could not be read as a [`SourceRecord`]; and the
    /// pins `open` could not restore.
    pub fn load_errors(&self) -> &[String] {
        &self.load_errors
    }

    /// [`Node::load_errors`] split into the job-record files that could not be read
    /// (by file name, or `<file>: format ...`) and the rest (source records, pins
    /// and gains, each entry saying what it is about).
    pub fn load_errors_by_origin(&self) -> (&[String], &[String]) {
        self.load_errors.split_at(self.job_load_errors)
    }

    /// Attach a port and start feeding its blocks into that source's ring and
    /// channel bank (made now for the port's current band, if it has one, and
    /// remade whenever its blocks show a new band). A second port with an
    /// attached source's id is `DuplicateSource`. Does not resume jobs. A port that
    /// reports heals ([`Port::healed`], a supervised worker) is watched from now on:
    /// each heal recovers the source (see [`Inner::recover_source`]).
    pub fn add_port(&self, port: Arc<dyn Port>) -> Result<(), NodeError> {
        let inner = &self.inner;
        let source = port.id().clone();
        {
            let mut ports = inner.ports.write().unwrap();
            if ports.contains_key(&source) {
                return Err(NodeError::DuplicateSource(source));
            }
            ports.insert(source.clone(), port.clone());
        }
        inner.bus.register_feed(&source);
        inner.tee_events(&source);
        inner.note_capabilities(port.as_ref());
        if let Some(band) = port.current() {
            inner.sync_bank(&source, band);
        }
        tokio::spawn(feed_ring(inner.clone(), source.clone(), port.subscribe()));
        tokio::spawn(feed_bank(inner.clone(), source.clone(), port.subscribe()));
        if let Some(rx) = port.healed() {
            tokio::spawn(recover::watch_healed(inner.clone(), source, rx));
        }
        Ok(())
    }

    /// Use `factory` for the channel banks of wide sources (rate at least
    /// [`PFB_MIN_RATE_HZ`]); existing banks are dropped and remade with it
    /// for every port that has a current band.
    pub fn set_bank_factory(&self, factory: Arc<dyn BankFactory>) {
        *self.inner.bank_factory.write().unwrap() = Some(factory);
        self.inner.banks.lock().unwrap().clear();
        let ports: Vec<_> = self.inner.ports.read().unwrap().values().cloned().collect();
        for p in ports {
            if let Some(band) = p.current() {
                self.inner.sync_bank(p.id(), band);
            }
        }
    }

    /// Use `themes` as the page themes (`themes_list`, the served theme files).
    pub fn set_themes(&self, themes: Arc<crate::themes::ThemeRegistry>) {
        *self.inner.themes.write().unwrap() = themes;
    }

    /// The page themes (the built-in ones unless `set_themes` replaced them).
    pub fn themes(&self) -> Arc<crate::themes::ThemeRegistry> {
        self.inner.themes.read().unwrap().clone()
    }

    /// The channel bank the node keeps for `source`, if any.
    pub fn bank(&self, source: &SourceId) -> Option<Arc<dyn ChannelBank>> {
        self.inner
            .banks
            .lock()
            .unwrap()
            .get(source)
            .and_then(|s| s.bank.clone())
    }

    /// See [`Inner::bank_bin_rate`].
    pub(crate) fn bank_bin_rate(&self, source: &SourceId, band: Tuning) -> Option<u64> {
        self.inner.bank_bin_rate(source, band)
    }

    /// Register a knowledge provider; `KnowledgeLookup` queries all of them.
    pub fn add_knowledge(&self, p: Arc<dyn KnowledgeProvider>) {
        self.inner.knowledge.write().unwrap().push(p);
    }

    /// Register a classifier; `Classify` runs all of them in layer order.
    pub fn add_classifier(&self, c: Arc<dyn Classifier>) {
        self.inner.classifiers.write().unwrap().register(c);
    }

    /// The decoder modes loaded at `open` (see [`crate::modes`]).
    pub fn modes(&self) -> Arc<ModeRegistry> {
        self.inner.modes.clone()
    }

    /// The burst store: the windows `capture` jobs wrote.
    pub fn bursts(&self) -> Arc<crate::bursts::BurstStore> {
        self.inner.bursts.clone()
    }

    pub(crate) fn knowledge(&self) -> Vec<Arc<dyn KnowledgeProvider>> {
        self.inner.knowledge.read().unwrap().clone()
    }

    pub(crate) fn classifiers(&self) -> ClassifierSet {
        self.inner.classifiers.read().unwrap().clone()
    }

    pub(crate) fn port(&self, id: &SourceId) -> Option<Arc<dyn Port>> {
        self.inner.port(id)
    }

    /// The fault `id`'s port reports, if any; reading one opens the source's fault
    /// episode (see [`recover::Episode`]), so the jobs that fail of it from now on are
    /// relaunched when the port heals.
    pub(crate) fn source_fault(&self, id: &SourceId) -> Option<String> {
        let fault = self.port(id).and_then(|p| p.fault());
        if fault.is_some() {
            self.inner.note_fault(id);
        }
        fault
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.inner.state_dir
    }

    /// [`crate::stage::StageRegistry::readiness`] of the stage kind `kind` on this node.
    pub fn stage_readiness(&self, kind: &str) -> Option<crate::stage::Readiness> {
        self.inner.registry.readiness(kind)
    }

    /// [`crate::ops::schema`] plus `stages`: every registered stage kind with
    /// its stream kinds and params schema (see [`StageRegistry::describe`]), with
    /// `$defs.JobKind.enum` (in the op, result and error schemas) extended by this
    /// node's mode rows, its operator rows included.
    pub fn schema(&self) -> serde_json::Value {
        let mut v = crate::ops::schema();
        v["stages"] = self.inner.registry.describe();
        let ids = self.inner.modes.ids();
        for part in ["op", "result", "error"] {
            let Some(kinds) = v[part]["$defs"]
                .get_mut("JobKind")
                .and_then(|k| k.get_mut("enum"))
                .and_then(serde_json::Value::as_array_mut)
            else {
                continue;
            };
            for id in ids.iter().filter(|id| crate::modes::is_mode_id(id)) {
                if !kinds.iter().any(|k| k == id.as_str()) {
                    kinds.push(id.as_str().into());
                }
            }
        }
        v
    }

    /// Apply one operation; see [`crate::ops`]. Once it completes, success
    /// or failure, one [`crate::ops::ops_payload`] event is published on
    /// [`Node::ops_stream`] (so a tail of that stream never sees its own record).
    /// A cancelled or dropped `apply` future logs nothing. The record's
    /// `transport` is `"local"`; see [`Node::apply_from`].
    pub async fn apply(&self, op: Op) -> Result<OpResult, OpError> {
        self.apply_from(op, "local").await
    }

    /// [`Node::apply`] with the record's `transport` set to `transport`: who
    /// asked (`"local"`, `"http"`, `"ui"`, `"agent"`), so a reader of the
    /// `ops` stream can tell its own calls from others'.
    pub async fn apply_from(&self, op: Op, transport: &'static str) -> Result<OpResult, OpError> {
        let logged = op.clone();
        let start_ns = now();
        let started = std::time::Instant::now();
        let outcome = crate::ops::apply(self, op).await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let payload = crate::ops::ops_payload(&logged, &outcome, elapsed_ms, transport);
        let mut line = payload.clone();
        self.inner.bus.publish(Frame::new(
            self.ops_stream.clone(),
            node_provenance(Span {
                start_ns,
                end_ns: now(),
            }),
            Payload::Event(payload),
        ));
        line["t_ns"] = start_ns.0.into();
        // A successful read is logged without its result: who asked, and when.
        if outcome.is_ok()
            && logged.read_only()
            && let Some(m) = line.as_object_mut()
        {
            m.remove("result");
        }
        self.inner.log_line(LogFile::Ops, &line);
        outcome
    }

    /// The `ops` event stream: kind `Event`, provenance source
    /// [`NODE_SOURCE`], one frame per [`Node::apply`].
    pub fn ops_stream(&self) -> StreamId {
        self.ops_stream.clone()
    }

    pub fn ports(&self) -> Vec<(SourceId, Capabilities)> {
        let ports = self.inner.ports.read().unwrap();
        ports
            .values()
            .map(|p| (p.id().clone(), p.capabilities().clone()))
            .collect()
    }

    /// Start a job of `kind` on `graph`, leasing `tuning` on `source`. `capture_rate_hz`
    /// is the source rate `graph` was expanded for (recorded, so a resume at another
    /// rate re-expands it), `None` for a graph that does not depend on it. An
    /// `elastic` job (a built-in kind) takes an elastic lease: it joins the source's
    /// capture, its `tuning` becomes that capture, and it is relaunched under the same
    /// id on every retune (see [`crate::lease::LeaseManager::ensure`]).
    pub async fn job_start(
        &self,
        kind: &str,
        source: SourceId,
        tuning: Tuning,
        graph: Graph,
        capture_rate_hz: Option<Hz>,
        elastic: bool,
    ) -> Result<JobId, NodeError> {
        self.check_prefer(&graph)?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.to_string(),
            graph,
            source,
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: capture_rate_hz.map(|r| r.0),
            elastic,
            operator_graph: false,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// Start a job of `kind` on `graph` attached to the job `input`: recorded with
    /// `input`, the input's `source` and `tuning`, and launched without a lease (see
    /// [`crate::stream_in`]); the launch points every `stream_in` node at the input's
    /// audio stream. `UnknownJob` for an unknown input; `InputNotRunning` and
    /// `NoAudioStream` as [`Node::job_audio_stream`]; on error nothing is recorded.
    pub async fn job_attach(
        &self,
        kind: &str,
        input: &JobId,
        graph: Graph,
    ) -> Result<JobId, NodeError> {
        let (source, tuning) = {
            let jobs = self.inner.jobs.lock().unwrap();
            let e = jobs
                .get(input)
                .ok_or_else(|| NodeError::UnknownJob(input.clone()))?;
            (e.record.source.clone(), e.record.tuning)
        };
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.to_string(),
            graph,
            source,
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: Some(input.clone()),
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// Start a mix of `sources` (`None` for every source): one leaseless job on the
    /// placeholder source [`NODE_SOURCE`], tuning 0/0. With one already recorded running
    /// on the same set (a mute plays no part; `None` matches only `None`) its id is the
    /// answer. `UnknownSource` for a source not attached, `Other` for an empty list.
    pub async fn mix_start(&self, sources: Option<Vec<SourceId>>) -> Result<JobId, NodeError> {
        if let Some(list) = &sources {
            if list.is_empty() {
                return Err(NodeError::Other(
                    "sources: empty: pass null for every source".into(),
                ));
            }
            if let Some(s) = list.iter().find(|s| self.inner.port(s).is_none()) {
                return Err(NodeError::UnknownSource(s.clone()));
            }
        }
        let graph = crate::ops::mix_graph(sources.as_deref());
        let wanted = crate::ops::mix_graph_sources(&graph);
        let _gate = self.inner.mix_gate.lock().await;
        let running = self.job_list().into_iter().find(|r| {
            r.kind == JobKind::Mix.name()
                && r.state == JobState::Running
                && crate::ops::mix_graph_sources(&r.graph) == wanted
        });
        if let Some(r) = running {
            return Ok(r.id);
        }
        graph::validate(&graph, &self.inner.registry)?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: JobKind::Mix.name().to_string(),
            graph,
            source: SourceId(NODE_SOURCE.into()),
            tuning: Tuning {
                center_hz: Hz(0),
                bandwidth_hz: Hz(0),
            },
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// Start a job from `graph`, whose first node takes no input (`capture_in`,
    /// `stream_in`, `mix`): a leaseless [`GRAPH_KIND`](crate::ops::GRAPH_KIND) record on
    /// [`NODE_SOURCE`], its band the first `capture_in`'s first capture's (0/0 without
    /// one), launched as stored (`operator_graph`). `Graph` when it does not validate;
    /// `Other` for a first node that takes input or a node with no edge into it whose
    /// stage takes input (nothing would feed it: there is no radio); `Expand(Unknown)`
    /// naming `captures[<i>]` for a capture not in the burst store.
    pub async fn graph_start(&self, graph: Graph) -> Result<JobId, NodeError> {
        graph::validate(&graph, &self.inner.registry)?;
        let registry = &self.inner.registry;
        let takes_input = |n: &crate::graph::Node| {
            registry
                .spec(&n.kind)
                .and_then(|s| s.inputs.first().copied())
        };
        let kind_name = |k: StreamKind| {
            serde_json::to_value(k)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default()
        };
        let unfed = |n: &crate::graph::Node, k: StreamKind| {
            NodeError::Other(format!(
                "graph_start: node {} ({}) has an unfed {} input",
                n.id.0,
                n.kind,
                kind_name(k)
            ))
        };
        let first = graph
            .nodes
            .first()
            .ok_or_else(|| NodeError::Other("graph_start: the graph has no nodes".into()))?;
        if let Some(k) = takes_input(first) {
            // A lone stage that takes input is a node nothing feeds; one in a chain is a
            // root that would wait for a radio.
            if !graph
                .edges
                .iter()
                .any(|e| e.from == first.id || e.to == first.id)
            {
                return Err(unfed(first, k));
            }
            return Err(NodeError::Other(format!(
                "graph_start: node {} ({}) takes input; the first node must take none \
                 (capture_in, stream_in, mix)",
                first.id.0, first.kind
            )));
        }
        for n in &graph.nodes {
            if let Some(k) = takes_input(n)
                && !graph.edges.iter().any(|e| e.to == n.id)
            {
                return Err(unfed(n, k));
            }
        }
        let store = self.bursts();
        let mut tuning = Tuning {
            center_hz: Hz(0),
            bandwidth_hz: Hz(0),
        };
        let mut placed = false;
        for n in graph
            .nodes
            .iter()
            .filter(|n| n.kind == crate::ops::CAPTURE_IN_STAGE)
        {
            let ids = n.params.get("captures").and_then(|v| v.as_array());
            for (i, id) in ids.into_iter().flatten().enumerate() {
                let id = id.as_str().unwrap_or_default();
                let Some(rec) = store.get(id) else {
                    return Err(NodeError::Expand(OpError::Unknown {
                        field: format!("captures[{i}]"),
                        value: id.to_string(),
                        known: store.list().into_iter().map(|r| r.id).collect(),
                    }));
                };
                if !placed {
                    tuning = Tuning {
                        center_hz: rec.center_hz,
                        bandwidth_hz: rec.bandwidth_hz,
                    };
                    placed = true;
                }
            }
        }
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: crate::ops::GRAPH_KIND.to_string(),
            graph,
            source: SourceId(NODE_SOURCE.into()),
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: true,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// Start the bits chain ([`crate::bits::bits_graph`]) over the set of the burst
    /// capture `capture`, in time order, each `None` filled from the capture's feature
    /// record or, for `modulation`, the layer-1 label the classifiers give its samples: a
    /// leaseless [`BITS_KIND`](crate::ops::BITS_KIND) record on [`NODE_SOURCE`], its band
    /// the capture's, `context.bits` the plan, launched as stored as `graph_start`
    /// launches. `Expand(Unknown)` naming `capture` for a capture not in the store;
    /// `Other` for what [`crate::bits::bits_plan`] refuses.
    pub async fn bits_start(
        &self,
        capture: &str,
        modulation: Option<crate::bits::Modulation>,
        symbol_rate_baud: Option<crate::types::Baud>,
        order: Option<u8>,
    ) -> Result<JobId, NodeError> {
        let store = self.bursts();
        let Some(named) = store.get(capture) else {
            return Err(NodeError::Expand(OpError::Unknown {
                field: "capture".into(),
                value: capture.to_string(),
                known: store.list().into_iter().map(|r| r.id).collect(),
            }));
        };
        let mut set: Vec<_> = store
            .list()
            .into_iter()
            .filter(|r| r.set == named.set)
            .collect();
        set.sort_by_key(|r| (r.start_ns, r.id.clone()));
        // A record that does not read as one (an older store's `{}`) measured nothing.
        let features: crate::classify::FeatureRecord = serde_json::from_value(
            named.features.clone(),
        )
        .unwrap_or(crate::classify::FeatureRecord {
            bandwidth_hz: named.bandwidth_hz,
            center_hz: named.center_hz,
            symbol_rate_baud: None,
            snr_db: 0.0,
            burst: false,
            duty: None,
            bursts: None,
            burst_ms: None,
            period_ms: None,
            order: None,
            tone_spacing_hz: None,
            symbol_rate_line: None,
        });
        // The layer-1 label names the modulation when the request does not, and a
        // `noise` label keeps the chain off the measured centre; with the modulation
        // given, a capture that does not classify is only unlabelled.
        let classified = crate::ops::classify_capture(&self.classifiers(), &named).await;
        let classified = match (modulation, classified) {
            (_, Ok(c)) => Some(c),
            (Some(_), Err(_)) => None,
            (None, Err(e)) => return Err(NodeError::Other(e)),
        };
        let label =
            classified.and_then(|c| c.labels.into_iter().find(|l| l.layer == 1).map(|l| l.name));
        let plan = crate::bits::bits_plan(
            &set,
            &features,
            label.as_deref(),
            modulation,
            symbol_rate_baud,
            order,
        )
        .map_err(NodeError::Other)?;
        let graph = crate::bits::bits_graph(&plan);
        graph::validate(&graph, &self.inner.registry)?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: crate::ops::BITS_KIND.to_string(),
            graph,
            source: SourceId(NODE_SOURCE.into()),
            tuning: Tuning {
                center_hz: named.center_hz,
                bandwidth_hz: named.bandwidth_hz,
            },
            state: JobState::Running,
            context: serde_json::json!({ "bits": plan }),
            created_ns: now(),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: true,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// The audio stream a job attached to `job` would read: the last audio output stream
    /// (not an edge between its stages) in `job`'s graph order. `InputNotRunning` unless
    /// `job` runs in this node (a one-shot node runs only what it started),
    /// `NoAudioStream` when it outputs no audio.
    pub fn job_audio_stream(&self, job: &JobId) -> Result<StreamId, NodeError> {
        self.inner.audio_stream(job)
    }

    /// The output streams (those no edge of its graph consumes) of `job`'s current run,
    /// in graph order; empty for a job that does not run here. A `watch`'s one event
    /// output carries `detect`'s `signal` events and the watch's own.
    pub fn job_outputs(&self, job: &JobId) -> Vec<(StreamKind, StreamId)> {
        self.inner
            .jobs
            .lock()
            .unwrap()
            .get(job)
            .filter(|e| e.cancel.is_some())
            .map(|e| e.outputs.clone())
            .unwrap_or_default()
    }

    /// Persist the job as `Stopped`, then cancel its stages and wait for its
    /// supervisor to release the lease and unregister its streams. If the
    /// record cannot be written the job is left running and `Io` is returned.
    /// A `Failed` job keeps its state and reason.
    pub async fn job_stop(&self, id: &JobId) -> Result<(), NodeError> {
        self.inner.stop(id).await
    }

    /// Hold `source`'s gate until the guard drops when `replace` names jobs, so
    /// stopping them and the request that follows happen under one hold (see
    /// [`Node::replace_holders`]); `None` (each step takes the gate itself, as
    /// without replace) when it names none. See [`LeaseManager::gate_guard`].
    pub(crate) async fn gate_for_replace(
        &self,
        source: &SourceId,
        replace: &[JobId],
    ) -> Option<OwnedMutexGuard<()>> {
        match replace.is_empty() {
            true => None,
            false => Some(self.inner.leases.gate_guard(source).await),
        }
    }

    /// Hold `source`'s gate until the guard drops; see [`LeaseManager::gate_guard`].
    pub(crate) async fn gate_guard(&self, source: &SourceId) -> OwnedMutexGuard<()> {
        self.inner.leases.gate_guard(source).await
    }

    /// The first running, rigid, non-attached job on `source` that a start of
    /// `kind` at `tuning` with `deviation_hz` and `sideband` would repeat: the
    /// same kind string and tuning, and the same start params once both sides
    /// are read as the kind's defaults (`fm_demod` 5000 Hz, `wfm_demod` 75000 Hz,
    /// `ssb_demod` usb; other kinds have none). A job another job owns (an
    /// `identify` or `watch` trial child) is never named. A kind the node does not
    /// expand has no such job.
    pub(crate) fn duplicate_of(
        &self,
        source: &SourceId,
        kind: &str,
        tuning: Tuning,
        deviation_hz: Option<u64>,
        sideband: Option<Sideband>,
    ) -> Option<JobId> {
        let k = self.expandable_kind(kind)?;
        let norm = |(d, s): (Option<u64>, Option<Sideband>)| match k {
            JobKind::FmDemod => (Some(d.unwrap_or(FM_DEVIATION_DEFAULT_HZ)), None),
            JobKind::WfmDemod => (Some(d.unwrap_or(WFM_DEVIATION_DEFAULT_HZ)), None),
            JobKind::SsbDemod => (None, Some(s.unwrap_or(Sideband::Usb))),
            _ => (None, None),
        };
        let want = norm((deviation_hz, sideband));
        self.job_list()
            .into_iter()
            .find(|r| {
                r.state == JobState::Running
                    && !r.elastic
                    && r.input.is_none()
                    && r.owner.is_none()
                    && &r.source == source
                    && r.kind == kind
                    && r.tuning == tuning
                    && norm(k.start_params(&r.graph)) == want
            })
            .map(|r| r.id)
    }

    /// Stop the jobs `replace` names, under `source`'s gate (`guard`), so the
    /// request that follows under the same guard finds the radio as they left
    /// it. Every id is checked first, and nothing is stopped unless all pass:
    /// `UnknownJob`, or `Other("job <id> does not hold source <src>")` for a job
    /// without a rigid lease on `source` (an elastic job never holds one). Each is
    /// then stopped as by [`Node::job_stop`] (persisted `Stopped`, streams
    /// unregistered) but without idling the port, and its lease is gone when this
    /// returns. A stop that cannot be persisted is `Io`, that job left running.
    pub(crate) async fn replace_holders(
        &self,
        source: &SourceId,
        replace: &[JobId],
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(), NodeError> {
        let leases = &self.inner.leases;
        leases.assert_gate(source, guard);
        let holds = |id: &JobId| leases.holders(source).iter().any(|(j, _)| j == id);
        for id in replace {
            if !self.inner.jobs.lock().unwrap().contains_key(id) {
                return Err(NodeError::UnknownJob(id.clone()));
            }
            if !holds(id) {
                return Err(NodeError::Other(format!(
                    "job {} does not hold source {}",
                    id.0, source.0
                )));
            }
        }
        let mut seen = Vec::new();
        for id in replace {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            self.inner.stop_gated(id).await?;
            // Another stop may have taken the supervisor, or a restart (`graph_put`)
            // may hold the new lease on its way to the launch `record_running`
            // refuses: either release is near (nothing before it takes the gate),
            // and the request must not see the lease. Bounded, so a stage that
            // never ends cannot keep the source's gate.
            let deadline = tokio::time::Instant::now() + REPLACE_RELEASE_TIMEOUT;
            while holds(id) {
                if tokio::time::Instant::now() >= deadline {
                    return Err(NodeError::Other(format!(
                        "job {} did not release source {}",
                        id.0, source.0
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        }
        Ok(())
    }

    /// A watch's `detect` node names only this node's sources in `prefer`:
    /// `bad params on node <id>: prefer[<i>]: unknown source <id>` otherwise. Checked
    /// at `job_start` and `graph_put` only, so a resume on a node started without one of
    /// them still runs (the watch then skips it).
    fn check_prefer(&self, graph: &Graph) -> Result<(), NodeError> {
        let ports = self.inner.ports.read().unwrap();
        for n in graph
            .nodes
            .iter()
            .filter(|n| n.kind == crate::ops::DETECT_STAGE)
        {
            let Some(prefer) = n.params.get("prefer").and_then(|p| p.as_array()) else {
                continue;
            };
            for (i, id) in prefer.iter().enumerate() {
                let Some(id) = id.as_str() else { continue };
                if !ports.contains_key(&SourceId(id.into())) {
                    return Err(GraphError::BadParams {
                        node: n.id.clone(),
                        reason: format!("prefer[{i}]: unknown source {id}"),
                    }
                    .into());
                }
            }
        }
        Ok(())
    }

    /// [`Node::job_start`], with `guard` under `source`'s gate: the lease is taken
    /// without letting the gate go. What must wait for the gate to be released
    /// (relaunching the elastic jobs a tune moved, stopping those a scan
    /// displaces) is returned, for [`Node::after_gate`] once the guard is dropped.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn job_start_gated(
        &self,
        kind: &str,
        source: SourceId,
        tuning: Tuning,
        graph: Graph,
        capture_rate_hz: Option<Hz>,
        elastic: bool,
        guard: Option<&OwnedMutexGuard<()>>,
    ) -> (Result<JobId, NodeError>, AfterGate) {
        if let Err(e) = self.check_prefer(&graph) {
            return (Err(e), AfterGate::Nothing);
        }
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.to_string(),
            graph,
            source,
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: capture_rate_hz.map(|r| r.0),
            elastic,
            operator_graph: false,
            owner: None,
        };
        let id = rec.id.clone();
        let (r, after) = self.inner.launch_gated(rec, guard).await;
        (r.map(|()| id), after)
    }

    /// Run what a gated call left for after the gate (see [`AfterGate`]). Call it
    /// only once the guard is dropped.
    pub(crate) async fn after_gate(&self, after: AfterGate) {
        self.inner.after_gate(after).await;
    }

    /// The kind a job recorded as `kind` was expanded from, when the node expands its
    /// graph; see [`Inner::expandable_kind`].
    pub(crate) fn expandable_kind(&self, kind: &str) -> Option<JobKind> {
        self.inner.expandable_kind(kind)
    }

    /// Idle `port` if nobody holds it; see [`LeaseManager::idle_if_unheld`]. Not
    /// under the source's gate (it takes it).
    pub(crate) async fn idle_if_unheld(&self, port: &dyn Port) {
        self.inner.leases.idle_if_unheld(port).await;
    }

    /// Replace a job's graph, keeping its id. The graph is validated first. A
    /// `Running` job is stopped and relaunched on the new graph; any other job
    /// only has its record rewritten. If the relaunch fails (say, a lease
    /// conflict after the stop) the job is left `Stopped` with the new graph.
    /// The record's `capture_rate_hz` is cleared and `operator_graph` set: the node
    /// cannot know what an operator's graph intends, so a resume launches it as stored.
    /// Except when the job is of a built-in kind the node expanded and `graph` is its
    /// graph with only the params [`JobKind::reexpand`] carries changed (a demodulator's
    /// `squelch`, `squelch_db`, `deviation_hz`, `sideband`): the record then keeps its
    /// rate and stays the node's to re-expand at a later capture.
    pub async fn job_graph_put(&self, id: &JobId, graph: Graph) -> Result<(), NodeError> {
        self.job_graph_set(id, graph, None).await
    }

    /// Whether `graph` is what re-expanding the job `rec` would give, carrying `graph`'s
    /// params: so only carried params differ from the node's graph. Tried at the rate the
    /// record was expanded for (else the source's kept capture) with the source's kept
    /// bank, and without a bank. Makes no bank.
    fn only_carried_params_differ(&self, rec: &JobRecord, graph: &Graph) -> bool {
        let Some(kind) = self.inner.expandable_kind(&rec.kind) else {
            return false;
        };
        if rec.operator_graph || rec.input.is_some() {
            return false;
        }
        let kept = self
            .inner
            .banks
            .lock()
            .unwrap()
            .get(&rec.source)
            .map(|slot| (slot.band, slot.bank.as_ref().map(|b| 2 * b.bin_hz())));
        let Some(rate) = rec
            .capture_rate_hz
            .map(Hz)
            .or(kept.map(|(band, _)| band.bandwidth_hz))
        else {
            return false;
        };
        let bin_rates = [kept.and_then(|(_, b)| b), None];
        bin_rates.into_iter().any(|bin_rate| {
            kind.reexpand(
                &rec.source,
                rec.tuning,
                rate,
                bin_rate,
                graph,
                &self.inner.modes,
            )
            .is_ok_and(|g| graph::same_pipeline(&g, graph))
        })
    }

    /// [`Node::job_graph_put`] recording `capture_rate_hz` as the rate `graph` was
    /// expanded for (the node's own re-expansion of a built-in kind).
    pub(crate) async fn job_graph_set(
        &self,
        id: &JobId,
        graph: Graph,
        capture_rate_hz: Option<Hz>,
    ) -> Result<(), NodeError> {
        let mut graph = graph;
        crate::ops::fill_squelch_params(&mut graph);
        graph::validate(&graph, &self.inner.registry)?;
        self.check_prefer(&graph)?;
        let current = {
            let jobs = self.inner.jobs.lock().unwrap();
            let entry = jobs
                .get(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            entry.record.clone()
        };
        let running = current.state == JobState::Running;
        // An operator's put of the node's graph with only carried params changed keeps
        // the record's rate (and so stays re-expandable).
        let capture_rate_hz = match capture_rate_hz {
            None if self.only_carried_params_differ(&current, &graph) => {
                Some(current.capture_rate_hz)
            }
            None => None,
            Some(r) => Some(Some(r.0)),
        };
        self.inner
            .swap_graph(id, graph, capture_rate_hz, running)
            .await
    }

    /// Every job record, oldest first.
    pub fn job_list(&self) -> Vec<JobRecord> {
        let mut v: Vec<JobRecord> = self
            .inner
            .jobs
            .lock()
            .unwrap()
            .values()
            .map(|e| e.record.clone())
            .collect();
        v.sort_by(|a, b| {
            a.created_ns
                .0
                .cmp(&b.created_ns.0)
                .then_with(|| a.id.0.cmp(&b.id.0))
        });
        v
    }

    pub fn job_context(&self, id: &JobId) -> Option<serde_json::Value> {
        self.inner
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.record.context.clone())
    }

    /// Shallow JSON merge patch: each top-level key of an object `patch` is set,
    /// or removed when its value is `null`; a non-object `patch` replaces the context.
    pub fn job_context_merge(&self, id: &JobId, patch: serde_json::Value) -> Result<(), NodeError> {
        self.inner.context_merge(id, patch)
    }

    /// The source's IQ history; `None` until its first block arrives.
    pub fn ring(&self, source: &SourceId) -> Option<Arc<RwLock<IqRing>>> {
        self.inner.rings.lock().unwrap().get(source).cloned()
    }

    pub fn bus(&self) -> Arc<StreamBus> {
        self.inner.bus.clone()
    }

    /// Natural job ends that could not be written to disk, one message each.
    pub fn persist_errors(&self) -> Vec<String> {
        self.inner.persist_errors.lock().unwrap().clone()
    }

    /// Transcript tees running (see `Inner::tee_transcripts`), for tests.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn debug_tee_count(&self) -> usize {
        self.inner.tees.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Tasks running for jobs' stages (each stage, IQ forwarder and scan loop);
    /// 0 when no job runs. `GET /health` reports it as `stages_running`.
    pub fn debug_stage_count(&self) -> usize {
        self.inner.stages.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// When `source`'s open fault episode began (see [`recover::Episode`]), if one is
    /// open, for tests of what opens and closes it.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn debug_fault_episode(&self, source: &SourceId) -> Option<u64> {
        self.inner.episode_since(source)
    }

    /// Hold `source`'s gate, for tests of what waits on it.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn debug_gate(&self, source: &SourceId) -> OwnedMutexGuard<()> {
        self.inner.leases.gate_guard(source).await
    }

    /// Tune `source` to `t` unless a job holds it, under the source's gate the
    /// caller holds (`guard`) or taking it; see [`LeaseManager::tune_if_unheld`].
    /// Also whether it tuned the port: the caller then relaunches the elastic jobs
    /// there ([`Node::relaunch_elastic`]) once the gate is let go.
    pub(crate) async fn tune_if_unheld(
        &self,
        port: &dyn Port,
        t: Tuning,
        guard: Option<&OwnedMutexGuard<()>>,
    ) -> Result<(Tuning, bool), LeaseError> {
        let tuned = match guard {
            Some(g) => self.inner.leases.tune_if_unheld_locked(port, t, g).await,
            None => self.inner.leases.tune_if_unheld_tuned(port, t).await,
        };
        // A tune the faulted port refused: the node has seen the fault.
        if let Err(LeaseError::Port(_)) = &tuned {
            self.inner.note_fault_if_faulted(port.id());
        }
        tuned
    }

    /// Lease `want` on `port` as a rigid request under the source's gate the caller
    /// holds, for a holder that is no job (a fresh id, returned): granted, tuned or a
    /// conflict exactly as a rigid `job_start` of `want` would be (see
    /// [`LeaseManager::ensure_locked`]), so the jobs leased inside it afterwards join its
    /// capture untuned. The lease lives as long as the returned [`BandHold`]: drop it
    /// before the gate goes (it is released however the caller's future ends, a
    /// dropped one included); meanwhile `source_list` may list it among the holders.
    /// Also whether it tuned the port: the caller then relaunches the elastic jobs
    /// ([`Node::after_gate`]).
    pub(crate) async fn hold_band(
        &self,
        port: &dyn Port,
        want: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(BandHold<'_>, Tuning, bool), LeaseError> {
        let holder = JobId::new();
        let held = self
            .inner
            .leases
            .ensure_locked(port, holder.clone(), want, false, guard)
            .await;
        // A tune the faulted port refused: the node has seen the fault.
        if let Err(LeaseError::Port(_)) = &held {
            self.inner.note_fault_if_faulted(port.id());
        }
        held.map(|(t, tuned)| {
            let hold = BandHold {
                node: self,
                id: holder,
            };
            (hold, t, tuned)
        })
    }

    /// Relaunch every running elastic job on `source` on its current capture; see
    /// [`Inner::relaunch_elastic`].
    pub(crate) async fn relaunch_elastic(&self, source: &SourceId) {
        let _ = self.inner.relaunch_elastic(source, None).await;
    }

    /// Where source `id` is: what its port reports itself (`from: status`), else the
    /// operator's `--source-position` entry (`from: configured`), else `None`.
    pub fn source_position(&self, id: &SourceId) -> Option<SourcePosition> {
        self.port(id).and_then(|p| p.position()).or_else(|| {
            self.inner.source_positions.get(id).map(|p| SourcePosition {
                from: PositionFrom::Configured,
                ..*p
            })
        })
    }

    /// The node's station, when one is set.
    pub fn station(&self) -> Option<crate::station::Station> {
        self.inner.station.lock().unwrap().clone()
    }

    /// Store `s` as the station: written to `station.json` first, then kept in memory;
    /// a failed write leaves the old one. Jobs started afterwards see it.
    pub async fn set_station(
        &self,
        s: crate::station::Station,
    ) -> Result<crate::station::Station, NodeError> {
        crate::station::persist(&self.inner.state_dir, &s)?;
        *self.inner.station.lock().unwrap() = Some(s.clone());
        Ok(s)
    }

    /// Forget the station and remove `station.json`.
    pub async fn clear_station(&self) -> Result<(), NodeError> {
        crate::station::remove(&self.inner.state_dir)?;
        *self.inner.station.lock().unwrap() = None;
        Ok(())
    }

    /// Hold `source` at `want` without a job: a rigid lease under
    /// [`crate::lease::pin_holder`] that is never listed among the holders. Under
    /// one hold of the source's gate the `replace` jobs are stopped (see
    /// [`Node::replace_holders`]), any existing pin is set aside and the band is
    /// leased as a rigid request; a conflict keeps the old pin. The pin is then
    /// persisted in `sources/<id>.json`; a write that fails releases it and is
    /// `Io`. A tune relaunches the elastic jobs there once the gate is let go.
    /// The result is the capture, as `source_ensure`'s.
    pub async fn source_pin(
        &self,
        source: &SourceId,
        want: Tuning,
        replace: &[JobId],
    ) -> Result<Tuning, NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        let guard = self.inner.leases.gate_guard(source).await;
        let leased = match self.replace_holders(source, replace, &guard).await {
            Ok(()) => self
                .inner
                .leases
                .pin_locked(port.as_ref(), want, &guard)
                .await
                .map_err(NodeError::Lease),
            Err(e) => Err(e),
        };
        let (result, tuned) = match leased {
            Ok((got, tuned)) => match self.set_source_record(source, |r| r.pin = Some(want)) {
                Ok(()) => (Ok(got), tuned),
                Err(e) => {
                    self.inner.leases.unpin(source);
                    (Err(e), tuned)
                }
            },
            Err(e) => (Err(e), false),
        };
        drop(guard);
        // Outside the gate: the relaunched jobs' leases and the idle take it.
        if tuned {
            self.relaunch_elastic(source).await;
        }
        if let Err(e) = &result {
            // The `replace` jobs were stopped without idling the port (as for
            // `job_start`), and a pin released on a failed write held it: idle it
            // if nobody holds it now. A refused id stopped nothing.
            let stopped =
                !replace.is_empty() && !matches!(e, NodeError::UnknownJob(_) | NodeError::Other(_));
            if stopped || matches!(e, NodeError::Io(_)) {
                self.idle_if_unheld(port.as_ref()).await;
            }
        }
        result
    }

    /// Release `source`'s pin and persist `pin: null` under the source's gate, then
    /// idle the port if nothing else holds it. `Other("source <id> is not pinned")` when it has none; a
    /// write that fails is `Io`, the pin released all the same.
    pub async fn source_unpin(&self, source: &SourceId) -> Result<(), NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        // The release and the write under one hold of the gate, as `source_pin`'s
        // lease and write, so a racing pin cannot leave disk and memory disagreeing.
        let guard = self.inner.leases.gate_guard(source).await;
        if self.inner.leases.unpin(source).is_none() {
            return Err(NodeError::Other(format!(
                "source {} is not pinned",
                source.0
            )));
        }
        let written = self.set_source_record(source, |r| r.pin = None);
        drop(guard);
        // Outside the gate: the idle takes it.
        self.idle_if_unheld(port.as_ref()).await;
        written
    }

    /// Set `source`'s overall receive gain (`None`: automatic gain) through
    /// [`Port::set_gain`] and persist it in `sources/<id>.json`, to be re-applied
    /// when the node opens. A refusal is `Other` with the port's text; a write
    /// that fails is `Io`, the gain applied all the same.
    pub async fn source_set_gain(
        &self,
        source: &SourceId,
        db: Option<f64>,
    ) -> Result<GainInfo, NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        let got = port.set_gain(db).await.map_err(gain_error)?;
        self.set_source_record(source, |r| r.gain = Some(GainSetting { db }))?;
        Ok(got)
    }

    /// The band `source` is pinned at, if it is.
    pub fn pinned(&self, source: &SourceId) -> Option<Tuning> {
        self.inner.leases.pinned(source)
    }

    /// Change `source`'s record with `change` and write it; on a failed write the
    /// record in memory is left as it was.
    fn set_source_record(
        &self,
        source: &SourceId,
        change: impl FnOnce(&mut SourceRecord),
    ) -> Result<(), NodeError> {
        let mut sources = self.inner.sources.lock().unwrap();
        let mut rec = sources
            .get(source)
            .cloned()
            .unwrap_or_else(SourceRecord::new);
        rec.format = SOURCE_RECORD_FORMAT;
        change(&mut rec);
        persist_source(&self.inner.sources_dir, source, &rec)?;
        sources.insert(source.clone(), rec);
        Ok(())
    }

    /// Jobs currently holding a rigid lease on `source`, with the band each asked for.
    pub fn lease_holders(&self, source: &SourceId) -> Vec<(JobId, Tuning)> {
        self.inner.leases.holders(source)
    }

    /// Jobs currently following `source`'s capture on an elastic lease.
    pub fn lease_elastic(&self, source: &SourceId) -> Vec<JobId> {
        self.inner.leases.elastic_holders(source)
    }

    /// `source_free`: the sources, in id order, that could take the band now
    /// ([`LeaseManager::free_sources`]), and every other source that covers it with
    /// its holders, elastic jobs, pin and fault. Sources that cannot cover the band
    /// are in neither list.
    pub fn source_free(
        &self,
        center_hz: u64,
        bandwidth_hz: u64,
    ) -> (Vec<SourceId>, Vec<crate::ops::BusySource>) {
        let mut ports: Vec<Arc<dyn Port>> =
            self.inner.ports.read().unwrap().values().cloned().collect();
        ports.sort_by(|a, b| a.id().0.cmp(&b.id().0));
        let free = self
            .inner
            .leases
            .free_sources(&ports, center_hz, bandwidth_hz);
        let busy = ports
            .iter()
            .filter(|p| crate::lease::covers(p.capabilities(), center_hz, bandwidth_hz))
            .filter(|p| !free.contains(p.id()))
            .map(|p| crate::ops::BusySource {
                id: p.id().clone(),
                holders: self
                    .lease_holders(p.id())
                    .into_iter()
                    .map(|(j, _)| j)
                    .collect(),
                elastic: self.lease_elastic(p.id()),
                pin: self.pinned(p.id()),
                fault: p.fault(),
            })
            .collect();
        (free, busy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, input: Option<JobId>, owner: Option<JobId>) -> JobRecord {
        JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.into(),
            graph: if kind == "mix" {
                crate::ops::mix_graph(None)
            } else {
                crate::graph::Graph {
                    id: crate::graph::GraphId("g".into()),
                    nodes: vec![],
                    edges: vec![],
                }
            },
            source: SourceId("a".into()),
            tuning: Tuning {
                center_hz: Hz(476_625_000),
                bandwidth_hz: Hz(12_500),
            },
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: Timestamp(0),
            input,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner,
        }
    }

    #[test]
    fn mixable_refuses_attached_owned_and_mix_records() {
        assert!(mixable(&rec("fm_demod", None, None)));
        assert!(!mixable(&rec("transcribe", Some(JobId::new()), None)));
        assert!(!mixable(&rec("fm_demod", None, Some(JobId::new()))));
        assert!(!mixable(&rec("mix", None, None)));
        let mut graph_mix = rec("graph", None, None);
        graph_mix.graph = crate::ops::mix_graph(None);
        assert!(!mixable(&graph_mix));
    }

    #[test]
    fn mixable_refuses_a_job_on_a_no_voice_plan_channel() {
        let mut ch22 = rec("fm_demod", None, None);
        ch22.tuning.center_hz = Hz(476_950_000);
        assert!(!mixable(&ch22));
        let mut ch20 = rec("fm_demod", None, None);
        ch20.tuning.center_hz = Hz(476_900_000);
        assert!(mixable(&ch20));
    }
}
