//! The node: owns ports, leases, rings and the stream bus, and runs graphs as
//! persistent jobs that resume after restart.

mod launch;
mod persist;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::bank::{BankFactory, ChannelBank, PFB_MIN_RATE_HZ};
use crate::classify::{Classifier, ClassifierSet};
use crate::graph::{self, Graph, GraphError};
use crate::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use crate::knowledge::KnowledgeProvider;
use crate::lease::{LeaseError, LeaseManager};
use crate::ops::{Op, OpError, OpResult};
use crate::port::{Port, Tuning};
use crate::ring::IqRing;
use crate::stage::{Backend, StageRegistry};
use crate::stream::{Frame, Payload, StreamBus};
use crate::types::{
    Capabilities, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};

pub use crate::ops::OPS_RESULT_LIMIT_BYTES;

use launch::{feed_bank, feed_ring};
use persist::{io, load, now, persist};

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
    #[error("io: {0}")]
    Io(String),
    /// An attached job's input job is not running in this node.
    #[error("input job {} not running", .0.0)]
    InputNotRunning(JobId),
    /// An attached job's input job runs but has no audio stream to read.
    #[error("input job {} has no audio stream", .0.0)]
    NoAudioStream(JobId),
}

/// How many stopped or failed job records the node keeps: when a job ends
/// and more than this many are terminal, the oldest beyond the cap are
/// forgotten and their files removed. Running jobs are never counted.
pub const JOB_HISTORY_MAX: usize = 20;

struct JobEntry {
    record: JobRecord,
    /// When the record turned terminal in this process (its `created_ns` for
    /// one loaded terminal from disk); history is pruned oldest-ended first.
    ended_ns: Timestamp,
    cancel: Option<CancellationToken>,
    /// The supervisor; it releases the lease and unregisters the job's streams.
    done: Option<JoinHandle<()>>,
    /// The job's output streams (those no edge of its graph consumes), in graph order
    /// (each node's in its stage's output order), set at launch; an attached job reads
    /// one of these.
    outputs: Vec<(StreamKind, StreamId)>,
}

/// A source's bank for the capture band it was made for; `bank` is `None`
/// when that band is narrower than [`PFB_MIN_RATE_HZ`] or the factory declined.
struct BankSlot {
    band: Tuning,
    bank: Option<Arc<dyn ChannelBank>>,
}

struct Inner {
    state_dir: PathBuf,
    jobs_dir: PathBuf,
    ring_seconds: f64,
    ring_budget_bytes: u64,
    scan_dwell_ms: u64,
    backends: Vec<Backend>,
    registry: StageRegistry,
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
    /// Cancelled when the `Node` is dropped; every job token is a child of it.
    shutdown: CancellationToken,
}

impl Inner {
    /// Forget the terminal records that ended earliest beyond
    /// [`JOB_HISTORY_MAX`], removing their files (best effort: a file that
    /// will not go is retried at the next prune). Called with `jobs` locked
    /// after a record turns terminal; that record ended last, so it survives.
    fn prune_history(jobs: &mut HashMap<JobId, JobEntry>, jobs_dir: &Path) {
        let mut terminal: Vec<(Timestamp, JobId)> = jobs
            .values()
            .filter(|e| matches!(e.record.state, JobState::Stopped | JobState::Failed { .. }))
            .map(|e| (e.ended_ns, e.record.id.clone()))
            .collect();
        if terminal.len() <= JOB_HISTORY_MAX {
            return;
        }
        terminal.sort_by(|a, b| a.0.0.cmp(&b.0.0).then_with(|| a.1.0.cmp(&b.1.0)));
        for (_, id) in terminal.iter().take(terminal.len() - JOB_HISTORY_MAX) {
            jobs.remove(id);
            let _ = std::fs::remove_file(jobs_dir.join(format!("{}.json", id.0)));
        }
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
        entry
            .outputs
            .iter()
            .rev()
            .find(|(k, _)| *k == StreamKind::Audio)
            .map(|(_, s)| s.clone())
            .ok_or_else(|| NodeError::NoAudioStream(job.clone()))
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

/// Dropping the node cancels its tasks without touching persisted state, so
/// `Running` jobs resume on the next `open`.
pub struct Node {
    inner: Arc<Inner>,
    load_errors: Vec<String>,
    /// The `ops` event stream: one frame per [`Node::apply`].
    ops_stream: StreamId,
}

impl Drop for Node {
    fn drop(&mut self) {
        self.inner.shutdown.cancel();
    }
}

impl Node {
    /// Open the node on `cfg.state_dir`, attach `ports`, and, when
    /// `cfg.resume`, resume every `Running` job and every job that `Failed`
    /// because its source was absent and is now attached (same id, context
    /// and creation time): one whose source is absent or cannot be resumed is
    /// kept as `Failed`. Attached jobs resume after every job on the radio, so
    /// their input runs first; one whose input does not run is kept as
    /// `Failed` with `input job <id> not running`, and one that failed that way
    /// resumes once its input runs. Without `cfg.resume` every record
    /// is loaded as persisted, untouched. `open` itself fails only on
    /// state-directory I/O and on two ports sharing a source id
    /// (`DuplicateSource`).
    pub async fn open(
        cfg: NodeConfig,
        registry: StageRegistry,
        ports: Vec<Arc<dyn Port>>,
    ) -> Result<Self, NodeError> {
        let jobs_dir = cfg.state_dir.join("jobs");
        std::fs::create_dir_all(&jobs_dir).map_err(io)?;
        let (mut records, load_errors) = load(&jobs_dir)?;
        // Jobs on the radio first: an attached job resumes on its input's streams.
        records.sort_by_key(|r| r.input.is_some());
        let resume = cfg.resume;
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
        let node = Node {
            inner: Arc::new(Inner {
                state_dir: cfg.state_dir.clone(),
                jobs_dir,
                ring_seconds: cfg.ring_seconds,
                ring_budget_bytes: cfg.ring_budget_bytes,
                scan_dwell_ms: cfg.scan_dwell_ms,
                backends: cfg.backends,
                registry,
                leases: LeaseManager::new(),
                bus,
                ports: RwLock::new(HashMap::new()),
                rings: Mutex::new(HashMap::new()),
                bank_factory: RwLock::new(None),
                banks: Mutex::new(HashMap::new()),
                jobs: Mutex::new(HashMap::new()),
                knowledge: RwLock::new(Vec::new()),
                classifiers: RwLock::new(ClassifierSet::default()),
                persist_errors: Mutex::new(Vec::new()),
                shutdown: CancellationToken::new(),
            }),
            load_errors,
            ops_stream,
        };
        for port in ports {
            node.add_port(port)?;
        }
        for rec in records {
            if !resume || !node.resumable(&rec) {
                node.inner.jobs.lock().unwrap().insert(
                    rec.id.clone(),
                    JobEntry {
                        ended_ns: rec.created_ns,
                        record: rec,
                        cancel: None,
                        done: None,
                        outputs: Vec::new(),
                    },
                );
                continue;
            }
            let mut failed = rec.clone();
            if let Err(e) = node.inner.launch(rec).await {
                let reason = match e {
                    NodeError::UnknownSource(s) => format!("source {} not present", s.0),
                    e => e.to_string(),
                };
                failed.state = JobState::Failed { reason };
                persist(&node.inner.jobs_dir, &failed)?;
                node.inner.jobs.lock().unwrap().insert(
                    failed.id.clone(),
                    JobEntry {
                        ended_ns: now(),
                        record: failed,
                        cancel: None,
                        done: None,
                        outputs: Vec::new(),
                    },
                );
            }
        }
        {
            let mut jobs = node.inner.jobs.lock().unwrap();
            Inner::prune_history(&mut jobs, &node.inner.jobs_dir);
        }
        Ok(node)
    }

    /// Whether `open` relaunches `rec`: it is `Running`, or it `Failed` only
    /// because its source was absent and that source is now attached, or (an
    /// attached job) only because its input job was not running and it now runs.
    fn resumable(&self, rec: &JobRecord) -> bool {
        match (&rec.state, &rec.input) {
            (JobState::Running, _) => true,
            (JobState::Failed { reason }, Some(input)) => {
                *reason == NodeError::InputNotRunning(input.clone()).to_string()
                    && !matches!(
                        self.inner.audio_stream(input),
                        Err(NodeError::InputNotRunning(_))
                    )
            }
            (JobState::Failed { reason }, None) => {
                reason.starts_with("source ")
                    && reason.ends_with(" not present")
                    && self.port(&rec.source).is_some()
            }
            _ => false,
        }
    }

    /// Files in `state_dir/jobs` that could not be read as a `JobRecord`, or
    /// whose record id is not a ULID matching the file name.
    pub fn load_errors(&self) -> &[String] {
        &self.load_errors
    }

    /// Attach a port and start feeding its blocks into that source's ring and
    /// channel bank (made now for the port's current band, if it has one, and
    /// remade whenever its blocks show a new band). A second port with an
    /// attached source's id is `DuplicateSource`. Does not resume jobs.
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
        if let Some(band) = port.current() {
            inner.sync_bank(&source, band);
        }
        tokio::spawn(feed_ring(inner.clone(), source.clone(), port.subscribe()));
        tokio::spawn(feed_bank(inner.clone(), source, port.subscribe()));
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

    /// The channel bank the node keeps for `source`, if any.
    pub fn bank(&self, source: &SourceId) -> Option<Arc<dyn ChannelBank>> {
        self.inner
            .banks
            .lock()
            .unwrap()
            .get(source)
            .and_then(|s| s.bank.clone())
    }

    /// The bin rate of `source`'s bank for the capture `band` (made now if
    /// the band is new), or `None` when that band gets no bank.
    pub(crate) fn bank_bin_rate(&self, source: &SourceId, band: Tuning) -> Option<u64> {
        self.inner.sync_bank(source, band).map(|b| 2 * b.bin_hz())
    }

    /// Register a knowledge provider; `KnowledgeLookup` queries all of them.
    pub fn add_knowledge(&self, p: Arc<dyn KnowledgeProvider>) {
        self.inner.knowledge.write().unwrap().push(p);
    }

    /// Register a classifier; `Classify` runs all of them in layer order.
    pub fn add_classifier(&self, c: Arc<dyn Classifier>) {
        self.inner.classifiers.write().unwrap().register(c);
    }

    pub(crate) fn knowledge(&self) -> Vec<Arc<dyn KnowledgeProvider>> {
        self.inner.knowledge.read().unwrap().clone()
    }

    pub(crate) fn classifiers(&self) -> ClassifierSet {
        self.inner.classifiers.read().unwrap().clone()
    }

    pub(crate) fn port(&self, id: &SourceId) -> Option<Arc<dyn Port>> {
        self.inner.ports.read().unwrap().get(id).cloned()
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.inner.state_dir
    }

    /// [`crate::ops::schema`] plus `stages`: every registered stage kind with
    /// its stream kinds and params schema (see [`StageRegistry::describe`]).
    pub fn schema(&self) -> serde_json::Value {
        let mut v = crate::ops::schema();
        v["stages"] = self.inner.registry.describe();
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
        self.inner.bus.publish(Frame::new(
            self.ops_stream.clone(),
            node_provenance(Span {
                start_ns,
                end_ns: now(),
            }),
            Payload::Event(payload),
        ));
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

    pub async fn job_start(
        &self,
        kind: &str,
        source: SourceId,
        tuning: Tuning,
        graph: Graph,
    ) -> Result<JobId, NodeError> {
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

    /// Persist the job as `Stopped`, then cancel its stages and wait for its
    /// supervisor to release the lease and unregister its streams. If the
    /// record cannot be written the job is left running and `Io` is returned.
    /// A `Failed` job keeps its state and reason.
    pub async fn job_stop(&self, id: &JobId) -> Result<(), NodeError> {
        let done = {
            let mut jobs = self.inner.jobs.lock().unwrap();
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
                persist(&self.inner.jobs_dir, &rec)?;
                entry.record = rec;
                entry.ended_ns = now();
            }
            if let Some(c) = entry.cancel.take() {
                c.cancel();
            }
            let done = entry.done.take();
            Inner::prune_history(&mut jobs, &self.inner.jobs_dir);
            done
        };
        if let Some(h) = done {
            let _ = h.await;
        }
        Ok(())
    }

    /// Replace a job's graph, keeping its id. The graph is validated first. A
    /// `Running` job is stopped and relaunched on the new graph; any other job
    /// only has its record rewritten. If the relaunch fails (say, a lease
    /// conflict after the stop) the job is left `Stopped` with the new graph.
    pub async fn job_graph_put(&self, id: &JobId, graph: Graph) -> Result<(), NodeError> {
        graph::validate(&graph, &self.inner.registry)?;
        let running = {
            let jobs = self.inner.jobs.lock().unwrap();
            let entry = jobs
                .get(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            entry.record.state == JobState::Running
        };
        if running {
            self.job_stop(id).await?;
        }
        let rec = {
            let mut jobs = self.inner.jobs.lock().unwrap();
            let entry = jobs
                .get_mut(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            let rec = JobRecord {
                graph,
                ..entry.record.clone()
            };
            persist(&self.inner.jobs_dir, &rec)?;
            entry.record = rec.clone();
            rec
        };
        if running {
            self.inner.launch(rec).await?;
        }
        Ok(())
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
        let mut jobs = self.inner.jobs.lock().unwrap();
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
        persist(&self.inner.jobs_dir, &entry.record)
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

    /// Tune `source` to `t` unless a job holds it; see [`LeaseManager::tune_if_unheld`].
    pub(crate) async fn tune_if_unheld(
        &self,
        port: &dyn Port,
        t: Tuning,
    ) -> Result<Tuning, LeaseError> {
        self.inner.leases.tune_if_unheld(port, t).await
    }

    /// Jobs currently holding a lease on `source`, with the band each asked for.
    pub fn lease_holders(&self, source: &SourceId) -> Vec<(JobId, Tuning)> {
        self.inner.leases.holders(source)
    }
}
