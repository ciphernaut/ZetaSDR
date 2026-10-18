//! The node: owns ports, leases, rings and the stream bus, and runs graphs as
//! persistent jobs that resume after restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast::error::RecvError;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::classify::{Classifier, ClassifierSet};
use crate::graph::{self, Graph, GraphError};
use crate::job::{JobRecord, JobState};
use crate::knowledge::KnowledgeProvider;
use crate::lease::{LeaseError, LeaseManager};
use crate::ops::{Op, OpError, OpResult};
use crate::port::{IqReceiver, Port, Tuning};
use crate::ring::IqRing;
use crate::stage::{Backend, StageError, StageRegistry};
use crate::stream::{Frame, Payload, StreamBus};
use crate::types::{
    Capabilities, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};

const BUS_CAPACITY: usize = 256;

pub struct NodeConfig {
    pub state_dir: PathBuf,
    pub ring_seconds: f64,
    pub backends: Vec<Backend>,
    /// Relaunch `Running` jobs on `open` (a long-lived node). When false, records
    /// are loaded as persisted and nothing is launched, leased or rewritten.
    pub resume: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("unknown source {}", .0.0)]
    UnknownSource(SourceId),
    #[error("unknown job {}", .0.0)]
    UnknownJob(JobId),
    #[error(transparent)]
    Graph(#[from] GraphError),
    #[error(transparent)]
    Lease(#[from] LeaseError),
    #[error("io: {0}")]
    Io(String),
}

struct JobEntry {
    record: JobRecord,
    cancel: Option<CancellationToken>,
    /// The supervisor; it releases the lease and unregisters the job's streams.
    done: Option<JoinHandle<()>>,
}

struct Inner {
    state_dir: PathBuf,
    jobs_dir: PathBuf,
    ring_seconds: f64,
    backends: Vec<Backend>,
    registry: StageRegistry,
    leases: LeaseManager,
    bus: Arc<StreamBus>,
    ports: RwLock<HashMap<SourceId, Arc<dyn Port>>>,
    rings: Mutex<HashMap<SourceId, Arc<RwLock<IqRing>>>>,
    jobs: Mutex<HashMap<JobId, JobEntry>>,
    knowledge: RwLock<Vec<Arc<dyn KnowledgeProvider>>>,
    classifiers: RwLock<ClassifierSet>,
    /// Failures to persist a job's natural end; memory and disk disagree for these.
    persist_errors: Mutex<Vec<String>>,
    /// Cancelled when the `Node` is dropped; every job token is a child of it.
    shutdown: CancellationToken,
}

/// Dropping the node cancels its tasks without touching persisted state, so
/// `Running` jobs resume on the next `open`.
pub struct Node {
    inner: Arc<Inner>,
    load_errors: Vec<String>,
}

impl Drop for Node {
    fn drop(&mut self) {
        self.inner.shutdown.cancel();
    }
}

fn now() -> Timestamp {
    Timestamp(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64),
    )
}

fn io(e: impl std::fmt::Display) -> NodeError {
    NodeError::Io(e.to_string())
}

/// Write `<id>.json` atomically: temp file, then rename.
fn persist(dir: &Path, rec: &JobRecord) -> Result<(), NodeError> {
    let json = serde_json::to_vec_pretty(rec).map_err(io)?;
    let path = dir.join(format!("{}.json", rec.id.0));
    let tmp = dir.join(format!("{}.json.tmp", rec.id.0));
    std::fs::write(&tmp, json).map_err(io)?;
    std::fs::rename(&tmp, &path).map_err(io)
}

/// Load every `*.json` record; unreadable or unparsable files are reported by name.
fn load(dir: &Path) -> Result<(Vec<JobRecord>, Vec<String>), NodeError> {
    let (mut records, mut errors) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<JobRecord>(&s).ok());
        match parsed {
            Some(r) => records.push(r),
            None => errors.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            ),
        }
    }
    Ok((records, errors))
}

impl Node {
    /// Open the node on `cfg.state_dir`, attach `ports`, and, when
    /// `cfg.resume`, resume every `Running` job and every job that `Failed`
    /// because its source was absent and is now attached (same id, context
    /// and creation time): one whose source is absent or cannot be resumed is
    /// kept as `Failed`. Without `cfg.resume` every record
    /// is loaded as persisted, untouched. `open` itself fails only on
    /// state-directory I/O.
    pub async fn open(
        cfg: NodeConfig,
        registry: StageRegistry,
        ports: Vec<Arc<dyn Port>>,
    ) -> Result<Self, NodeError> {
        let jobs_dir = cfg.state_dir.join("jobs");
        std::fs::create_dir_all(&jobs_dir).map_err(io)?;
        let (records, load_errors) = load(&jobs_dir)?;
        let resume = cfg.resume;
        let node = Node {
            inner: Arc::new(Inner {
                state_dir: cfg.state_dir.clone(),
                jobs_dir,
                ring_seconds: cfg.ring_seconds,
                backends: cfg.backends,
                registry,
                leases: LeaseManager::new(),
                bus: Arc::new(StreamBus::new(BUS_CAPACITY)),
                ports: RwLock::new(HashMap::new()),
                rings: Mutex::new(HashMap::new()),
                jobs: Mutex::new(HashMap::new()),
                knowledge: RwLock::new(Vec::new()),
                classifiers: RwLock::new(ClassifierSet::default()),
                persist_errors: Mutex::new(Vec::new()),
                shutdown: CancellationToken::new(),
            }),
            load_errors,
        };
        for port in ports {
            node.add_port(port);
        }
        for rec in records {
            if !resume || !node.resumable(&rec) {
                node.inner.jobs.lock().unwrap().insert(
                    rec.id.clone(),
                    JobEntry {
                        record: rec,
                        cancel: None,
                        done: None,
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
                        record: failed,
                        cancel: None,
                        done: None,
                    },
                );
            }
        }
        Ok(node)
    }

    /// Whether `open` relaunches `rec`: it is `Running`, or it `Failed` only
    /// because its source was absent and that source is now attached.
    fn resumable(&self, rec: &JobRecord) -> bool {
        match &rec.state {
            JobState::Running => true,
            JobState::Failed { reason } => {
                reason.starts_with("source ")
                    && reason.ends_with(" not present")
                    && self.port(&rec.source).is_some()
            }
            _ => false,
        }
    }

    /// Files in `state_dir/jobs` that could not be read as a `JobRecord`.
    pub fn load_errors(&self) -> &[String] {
        &self.load_errors
    }

    /// Attach a port and start feeding its blocks into that source's ring.
    /// Does not resume jobs.
    pub fn add_port(&self, port: Arc<dyn Port>) {
        let inner = &self.inner;
        let source = port.id().clone();
        let rx = port.subscribe();
        inner.ports.write().unwrap().insert(source.clone(), port);
        tokio::spawn(feed_ring(inner.clone(), source, rx));
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

    /// Apply one operation; see [`crate::ops`].
    pub async fn apply(&self, op: Op) -> Result<OpResult, OpError> {
        crate::ops::apply(self, op).await
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
            id: JobId::new(),
            kind: kind.to_string(),
            graph,
            source,
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
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
            }
            if let Some(c) = entry.cancel.take() {
                c.cancel();
            }
            entry.done.take()
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

impl Inner {
    /// Validate, lease, record as `Running`, persist, then spawn the job's tasks.
    /// On error nothing is recorded and no lease is held.
    async fn launch(self: &Arc<Self>, mut rec: JobRecord) -> Result<(), NodeError> {
        let port = self.ports.read().unwrap().get(&rec.source).cloned();
        let port = port.ok_or_else(|| NodeError::UnknownSource(rec.source.clone()))?;
        graph::validate(&rec.graph, &self.registry)?;
        let mut stages = Vec::with_capacity(rec.graph.nodes.len());
        for n in &rec.graph.nodes {
            let stage = self
                .registry
                .select(&n.kind, &self.backends)
                .ok_or_else(|| GraphError::UnknownStage {
                    node: n.id.clone(),
                    kind: n.kind.clone(),
                })?;
            stages.push(stage);
        }
        self.leases
            .ensure(port.as_ref(), rec.id.clone(), rec.tuning)
            .await?;

        let id = rec.id.clone();
        let token = self.shutdown.child_token();
        rec.state = JobState::Running;
        if let Err(e) = persist(&self.jobs_dir, &rec) {
            self.leases.release(&id);
            return Err(e);
        }
        let t = now();
        let prov = Provenance {
            source: rec.source.clone(),
            job: Some(id.clone()),
            channel: None,
            span: Span {
                start_ns: t,
                end_ns: t,
            },
        };
        let graph = rec.graph.clone();
        self.jobs.lock().unwrap().insert(
            id.clone(),
            JobEntry {
                record: rec,
                cancel: Some(token.clone()),
                done: None,
            },
        );

        // One stream per edge; a node with no outgoing edge gets one job output
        // stream of its stage's first output kind.
        let edge_streams: Vec<_> = graph
            .edges
            .iter()
            .map(|e| self.bus.register(e.kind, prov.clone()))
            .collect();
        let roots = graph
            .nodes
            .iter()
            .any(|n| !graph.edges.iter().any(|e| e.to == n.id));
        let iq = roots.then(|| self.bus.register(StreamKind::Iq, prov.clone()));
        let mut streams: Vec<StreamId> = edge_streams.iter().cloned().chain(iq.clone()).collect();
        let mut set = JoinSet::new();
        for (n, stage) in graph.nodes.iter().zip(stages) {
            let mut inputs = Vec::new();
            let mut outputs = Vec::new();
            for (e, sid) in graph.edges.iter().zip(&edge_streams) {
                if e.to == n.id {
                    inputs.push(self.bus.subscribe(sid).expect("registered"));
                }
                if e.from == n.id {
                    outputs.push(sid.clone());
                }
            }
            if inputs.is_empty() {
                let sid = iq.as_ref().expect("root nodes have the IQ stream");
                inputs.push(self.bus.subscribe(sid).expect("registered"));
            }
            if outputs.is_empty()
                && let Some(&k) = stage.spec().outputs.first()
            {
                let sid = self.bus.register(k, prov.clone());
                streams.push(sid.clone());
                outputs.push(sid);
            }
            let (params, bus, cancel) = (n.params.clone(), self.bus.clone(), token.clone());
            set.spawn(async move { stage.run(params, inputs, outputs, bus, cancel).await });
        }
        if let Some(sid) = iq {
            tokio::spawn(forward_iq(
                self.bus.clone(),
                prov,
                sid,
                port.subscribe(),
                token.clone(),
            ));
        }
        let h = tokio::spawn(supervise(self.clone(), id.clone(), set, token, streams));
        if let Some(e) = self.jobs.lock().unwrap().get_mut(&id) {
            e.done = Some(h);
        }
        Ok(())
    }

    /// Record how a job ended on its own, unless it was stopped meanwhile.
    /// A failed write keeps the new state in memory and is reported through
    /// `persist_errors`; the job will be resumed on the next `open`.
    fn finish(&self, id: &JobId, state: JobState) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(entry) = jobs.get_mut(id)
            && entry.record.state == JobState::Running
        {
            entry.record.state = state;
            entry.cancel = None;
            if let Err(e) = persist(&self.jobs_dir, &entry.record) {
                self.persist_errors
                    .lock()
                    .unwrap()
                    .push(format!("job {}: {e}", id.0));
            }
        }
    }
}

/// Await a job's stages. The first to end for any reason other than
/// cancellation stops the rest; `Failed` wins over a clean end. Then release
/// the lease and unregister the job's streams.
async fn supervise(
    inner: Arc<Inner>,
    id: JobId,
    mut set: JoinSet<Result<(), StageError>>,
    token: CancellationToken,
    streams: Vec<StreamId>,
) {
    let mut failure = None;
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Err(StageError::Cancelled)) => {}
            Ok(Ok(())) => token.cancel(),
            Ok(Err(StageError::Failed(m))) => {
                failure.get_or_insert(m);
                token.cancel();
            }
            Err(e) => {
                failure.get_or_insert(format!("stage task: {e}"));
                token.cancel();
            }
        }
    }
    // After node shutdown the record must stay `Running` so the job resumes.
    if !inner.shutdown.is_cancelled() {
        inner.finish(
            &id,
            failure.map_or(JobState::Stopped, |reason| JobState::Failed { reason }),
        );
    }
    // The only place a started job's lease is released.
    inner.leases.release(&id);
    for s in &streams {
        inner.bus.unregister(s);
    }
}

/// Republish a port's blocks as `Payload::Iq` frames on a job's IQ stream.
async fn forward_iq(
    bus: Arc<StreamBus>,
    prov: Provenance,
    stream: StreamId,
    mut rx: IqReceiver,
    cancel: CancellationToken,
) {
    loop {
        let block = tokio::select! {
            _ = cancel.cancelled() => return,
            r = rx.recv() => match r {
                Ok(b) => b,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => {
                    // The port has ended: close the job's IQ stream so its stages end too.
                    bus.unregister(&stream);
                    return;
                }
            },
        };
        let rate = block.sample_rate_hz.0.max(1) as u128;
        let end = block.t0.0 + (block.samples.len() as u128 * 1_000_000_000 / rate) as u64;
        let provenance = Provenance {
            span: Span {
                start_ns: block.t0,
                end_ns: Timestamp(end),
            },
            ..prov.clone()
        };
        bus.publish(Frame {
            stream: stream.clone(),
            kind: StreamKind::Iq,
            provenance,
            payload: Payload::Iq(block),
        });
    }
}

/// Push a port's blocks into its ring, creating the ring on the first block.
async fn feed_ring(inner: Arc<Inner>, source: SourceId, mut rx: IqReceiver) {
    loop {
        let block = tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            r = rx.recv() => match r {
                Ok(b) => b,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            },
        };
        let ring = inner
            .rings
            .lock()
            .unwrap()
            .entry(source.clone())
            .or_insert_with(|| {
                Arc::new(RwLock::new(IqRing::new(
                    inner.ring_seconds,
                    block.sample_rate_hz,
                )))
            })
            .clone();
        ring.write().unwrap().push(&block);
    }
}
