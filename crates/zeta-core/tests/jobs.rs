//! Node job runtime: persistence, stop, resume and the per-source ring.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;

use zeta_core::graph::{Graph, GraphError, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig, NodeError};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, Timestamp};
use zeta_source_file::{FileOpts, FileSource, fixtures};

fn source(dir: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    source_with(dir, true)
}

fn source_with(dir: &Path, repeat: bool) -> (Arc<FileSource>, Arc<dyn Port>) {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r
}

fn graph(kind: &str) -> Graph {
    Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("p".into()),
            kind: kind.into(),
            params: serde_json::json!({"kind": "iq"}),
        }],
        edges: vec![],
    }
}

fn tuning() -> Tuning {
    Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(48_000),
    }
}

fn on_disk(dir: &Path, id: &JobId) -> JobRecord {
    let p = dir
        .join("state")
        .join("jobs")
        .join(format!("{}.json", id.0));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

async fn start_job(node: &Node) -> JobId {
    node.job_start(
        "monitor",
        SourceId("file0".into()),
        tuning(),
        graph("passthrough"),
        None,
        false,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn start_writes_record_and_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Running);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let list = node.job_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].state, JobState::Running);
}

#[tokio::test]
async fn stop_releases_lease_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    assert_eq!(node.lease_holders(&SourceId("file0".into())).len(), 1);
    node.job_stop(&id).await.unwrap();
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Stopped);
    assert!(node.lease_holders(&SourceId("file0".into())).is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Stopped);
    assert_eq!(node.job_list()[0].state, JobState::Stopped);
}

#[tokio::test]
async fn restart_resumes_running_job_with_same_id() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    node.job_context_merge(&id, serde_json::json!({"note": "kept"}))
        .unwrap();
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let list = node.job_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].state, JobState::Running);
    assert_eq!(node.job_context(&id).unwrap()["note"], "kept");
    assert_eq!(node.lease_holders(&SourceId("file0".into())).len(), 1);
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Running);
}

#[tokio::test]
async fn open_without_resume_leaves_records_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let path = dir
        .path()
        .join("state")
        .join("jobs")
        .join(format!("{}.json", id.0));
    let before = std::fs::read(&path).unwrap();
    let node = Node::open(
        NodeConfig {
            source_positions: Default::default(),
            sets_dir: None,
            burst_policy: Default::default(),
            resume: false,
            ..cfg(dir.path())
        },
        registry(),
        vec![],
    )
    .await
    .unwrap();
    let list = node.job_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].state, JobState::Running);
    assert!(node.lease_holders(&SourceId("file0".into())).is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before, "record rewritten");
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Running);
}

#[tokio::test]
async fn restart_with_missing_source_marks_failed_not_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    drop(node);
    let node = Node::open(cfg(dir.path()), registry(), vec![])
        .await
        .unwrap();
    let list = node.job_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    match &list[0].state {
        JobState::Failed { reason } => assert!(reason.contains("file0"), "{reason}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(matches!(
        on_disk(dir.path(), &id).state,
        JobState::Failed { .. }
    ));
}

#[tokio::test]
async fn unknown_stage_rejected_before_start() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    match node
        .job_start(
            "monitor",
            SourceId("file0".into()),
            tuning(),
            graph("nope"),
            None,
            false,
        )
        .await
    {
        Err(NodeError::Graph(GraphError::UnknownStage { kind, .. })) => assert_eq!(kind, "nope"),
        other => panic!("expected UnknownStage, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_dir(dir.path().join("state").join("jobs"))
            .unwrap()
            .count(),
        0
    );
    assert!(node.job_list().is_empty());
    assert!(node.lease_holders(&SourceId("file0".into())).is_empty());
}

#[tokio::test]
async fn ring_fills_from_port() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let ring = node.ring(&SourceId("file0".into())).expect("ring created");
    assert!(ring.read().unwrap().latest().is_some());
}

fn job_streams(node: &Node, id: &JobId) -> usize {
    node.bus()
        .list()
        .iter()
        .filter(|s| s.provenance.job.as_ref() == Some(id))
        .count()
}

#[tokio::test]
async fn stopped_job_streams_are_unregistered() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    assert!(job_streams(&node, &id) > 0);
    node.job_stop(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(job_streams(&node, &id), 0);
}

#[tokio::test]
async fn port_close_ends_job_as_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source_with(dir.path(), false);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    let mut state = JobState::Running;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        state = node.job_list()[0].state.clone();
        if state != JobState::Running {
            break;
        }
    }
    assert_eq!(state, JobState::Stopped);
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Stopped);
    assert!(node.lease_holders(&SourceId("file0".into())).is_empty());
    assert_eq!(job_streams(&node, &id), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn stop_persist_failure_leaves_job_running() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    let jobs = dir.path().join("state").join("jobs");
    std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores directory permissions; the failure cannot be provoked this way.
    if std::fs::write(jobs.join("probe"), b"").is_ok() {
        std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: directory permissions are not enforced (running as root?)");
        return;
    }
    let r = node.job_stop(&id).await;
    std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(r, Err(NodeError::Io(_))), "{r:?}");
    assert_eq!(node.job_list()[0].state, JobState::Running);
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Running);
    assert_eq!(node.lease_holders(&SourceId("file0".into())).len(), 1);
    assert!(job_streams(&node, &id) > 0);
    node.job_stop(&id).await.unwrap();
    assert_eq!(node.job_list()[0].state, JobState::Stopped);
}

#[tokio::test]
async fn failed_missing_source_job_resumes_when_source_returns() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    node.job_context_merge(&id, serde_json::json!({"note": "kept"}))
        .unwrap();
    let created = node.job_list()[0].created_ns;
    drop(node);
    let node = Node::open(cfg(dir.path()), registry(), vec![])
        .await
        .unwrap();
    assert!(matches!(node.job_list()[0].state, JobState::Failed { .. }));
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let list = node.job_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].state, JobState::Running);
    assert_eq!(list[0].created_ns, created);
    assert_eq!(node.job_context(&id).unwrap()["note"], "kept");
    assert_eq!(node.lease_holders(&SourceId("file0".into())).len(), 1);
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Running);
}

#[tokio::test]
async fn duplicate_source_ids_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (_a, pa) = source(dir.path());
    let (_b, pb) = source(dir.path());
    match Node::open(cfg(dir.path()), registry(), vec![pa.clone(), pb.clone()]).await {
        Err(NodeError::DuplicateSource(s)) => assert_eq!(s, SourceId("file0".into())),
        Err(e) => panic!("expected DuplicateSource, got {e:?}"),
        Ok(_) => panic!("expected DuplicateSource, got a node"),
    }
    let node = Node::open(cfg(dir.path()), registry(), vec![pa])
        .await
        .unwrap();
    match node.add_port(pb) {
        Err(NodeError::DuplicateSource(s)) => assert_eq!(s, SourceId("file0".into())),
        other => panic!("expected DuplicateSource, got {other:?}"),
    }
    assert_eq!(node.ports().len(), 1);
}

#[tokio::test]
async fn record_with_mismatched_id_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    node.job_stop(&id).await.unwrap();
    drop(node);
    let jobs = dir.path().join("state").join("jobs");
    let text = std::fs::read_to_string(jobs.join(format!("{}.json", id.0))).unwrap();
    // A valid ULID file name holding another id, and a non-ULID id in its own file.
    let other = JobId::new();
    std::fs::write(jobs.join(format!("{}.json", other.0)), &text).unwrap();
    let bad = text.replace(&id.0, "not-a-ulid");
    std::fs::write(jobs.join("not-a-ulid.json"), bad).unwrap();
    std::fs::write(jobs.join(format!("{}.json.tmp", other.0)), "partial").unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let ids: Vec<JobId> = node.job_list().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![id]);
    let mut errors = node.load_errors().to_vec();
    errors.sort();
    let mut want = vec![format!("{}.json", other.0), "not-a-ulid.json".to_string()];
    want.sort();
    assert_eq!(errors, want);
    assert!(!jobs.join(format!("{}.json.tmp", other.0)).exists());
}

/// Persist one stopped record, then rewrite its file with `edit` applied to the raw JSON.
async fn reopen_with_edited_record(
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> (Node, JobId, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let id = start_job(&node).await;
    node.job_stop(&id).await.unwrap();
    drop(node);
    let path = dir
        .path()
        .join("state")
        .join("jobs")
        .join(format!("{}.json", id.0));
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    edit(v.as_object_mut().unwrap());
    std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let mut c = cfg(dir.path());
    c.resume = false;
    let node = Node::open(c, registry(), vec![port]).await.unwrap();
    (node, id, dir)
}

#[tokio::test]
async fn format_1_record_without_format_field_loads() {
    let (node, id, _dir) = reopen_with_edited_record(|m| {
        assert!(m.remove("format").is_some());
    })
    .await;
    let list = node.job_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].format, 1);
    assert!(node.load_errors().is_empty());
}

#[tokio::test]
async fn newer_format_record_is_reported_not_loaded() {
    let (node, id, _dir) = reopen_with_edited_record(|m| {
        m.insert("format".into(), serde_json::json!(2));
    })
    .await;
    assert!(node.job_list().is_empty());
    assert_eq!(
        node.load_errors(),
        [format!("{}.json: format 2 newer than supported 1", id.0)]
    );
}

/// A port whose blocks the test sends by hand and whose fault the test sets;
/// a successful `tune` clears the fault, dropping the sender closes the stream.
struct FaultyPort {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: Mutex<Option<tokio::sync::broadcast::Sender<IqBlock>>>,
    rx: IqReceiver,
    fault: Mutex<Option<String>>,
    idles: std::sync::atomic::AtomicUsize,
}

impl FaultyPort {
    fn new() -> Arc<Self> {
        let (tx, rx) = tokio::sync::broadcast::channel(16);
        Arc::new(Self {
            id: SourceId("flaky".into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(20_000_000)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: false,
            },
            cur: Mutex::new(None),
            tx: Mutex::new(Some(tx)),
            rx,
            fault: Mutex::new(None),
            idles: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn send(&self, n: u64) {
        let block = IqBlock {
            t0: Timestamp(n * 1_000_000),
            sample_rate_hz: Hz(48_000),
            center_hz: Hz(100_000_000),
            samples: vec![Complex::new(0.0, 0.0); 48].into(),
        };
        let _ = self.tx.lock().unwrap().as_ref().unwrap().send(block);
    }

    fn close(&self) {
        self.tx.lock().unwrap().take();
    }

    fn set_fault(&self, f: &str) {
        *self.fault.lock().unwrap() = Some(f.into());
    }
}

#[async_trait]
impl Port for FaultyPort {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        *self.cur.lock().unwrap() = Some(t);
        *self.fault.lock().unwrap() = None;
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.rx.resubscribe()
    }
    fn fault(&self) -> Option<String> {
        self.fault.lock().unwrap().clone()
    }
    async fn idle(&self) {
        self.idles.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn start_flaky(node: &Node) -> JobId {
    node.job_start(
        "monitor",
        SourceId("flaky".into()),
        tuning(),
        graph("passthrough"),
        None,
        false,
    )
    .await
    .unwrap()
}

async fn wait_not_running(node: &Node, ms: u64) -> JobState {
    let mut state = JobState::Running;
    for _ in 0..ms / 20 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        state = node.job_list()[0].state.clone();
        if state != JobState::Running {
            break;
        }
    }
    state
}

#[tokio::test]
async fn port_fault_fails_running_job_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    let port = FaultyPort::new();
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    let id = start_flaky(&node).await;
    port.send(0);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(node.job_list()[0].state, JobState::Running);
    // No block follows the fault: the job must fail from the periodic check.
    port.set_fault("device gone");
    let state = wait_not_running(&node, 1000).await;
    assert_eq!(
        state,
        JobState::Failed {
            reason: "source flaky: device gone".into()
        }
    );
    assert_eq!(on_disk(dir.path(), &id).state, state);
    assert!(node.lease_holders(&SourceId("flaky".into())).is_empty());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(job_streams(&node, &id), 0);
}

#[tokio::test]
async fn closed_port_without_fault_still_drains_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let port = FaultyPort::new();
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    let id = start_flaky(&node).await;
    // The job output is the job stream nothing in the job subscribes to.
    let bus = node.bus();
    let out = bus
        .list()
        .into_iter()
        .filter(|s| s.provenance.job.as_ref() == Some(&id))
        .find(|s| bus.receiver_count(&s.id) == Some(0))
        .expect("job output stream");
    let mut rx = bus.subscribe(&out.id).unwrap();
    for n in 0..3 {
        port.send(n);
    }
    port.close();
    let mut frames = 0;
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
        frames += 1;
    }
    assert_eq!(frames, 3);
    assert_eq!(wait_not_running(&node, 1000).await, JobState::Stopped);
    assert!(node.lease_holders(&SourceId("flaky".into())).is_empty());
}

#[tokio::test]
async fn idle_fires_only_when_the_last_holder_stops() {
    let dir = tempfile::tempdir().unwrap();
    let flaky = FaultyPort::new();
    let port: Arc<dyn Port> = flaky.clone();
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let idles = || flaky.idles.load(std::sync::atomic::Ordering::SeqCst);
    let a = start_flaky(&node).await;
    let b = start_flaky(&node).await;
    assert_eq!(node.lease_holders(&SourceId("flaky".into())).len(), 2);
    node.job_stop(&a).await.unwrap();
    assert_eq!(idles(), 0, "one holder is left");
    node.job_stop(&b).await.unwrap();
    assert_eq!(idles(), 1, "the last holder left");
    // Starting again and stopping idles once more.
    let c = start_flaky(&node).await;
    node.job_stop(&c).await.unwrap();
    assert_eq!(idles(), 2);
}

/// Stopped jobs are history: only the newest `JOB_HISTORY_MAX` terminal
/// records survive, in memory and on disk; a running job is never pruned.
#[tokio::test]
async fn stopped_jobs_beyond_the_history_cap_are_forgotten() {
    use zeta_core::node::JOB_HISTORY_MAX;
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let keep = start_job(&node).await;
    let mut stopped = Vec::new();
    for _ in 0..JOB_HISTORY_MAX + 3 {
        let id = start_job(&node).await;
        node.job_stop(&id).await.unwrap();
        stopped.push(id);
    }
    let list = node.job_list();
    assert_eq!(list.len(), JOB_HISTORY_MAX + 1, "{list:?}");
    assert!(
        list.iter()
            .any(|r| r.id == keep && r.state == JobState::Running)
    );
    // Replacing the oldest running job's graph stops and relaunches it: the
    // stop must not prune it (history is ordered by when a job ended, not
    // when it started).
    node.job_graph_put(&keep, graph("passthrough"))
        .await
        .unwrap();
    let list = node.job_list();
    assert!(
        list.iter()
            .any(|r| r.id == keep && r.state == JobState::Running),
        "{list:?}"
    );
    // While it was stopped the cap applied once more: one older record went.
    assert_eq!(list.len(), JOB_HISTORY_MAX, "{list:?}");
    let kept: Vec<_> = list
        .iter()
        .filter(|r| r.state == JobState::Stopped)
        .map(|r| r.id.clone())
        .collect();
    assert_eq!(kept, stopped[4..].to_vec(), "the oldest four went");
    let jobs_dir = dir.path().join("state").join("jobs");
    for id in &stopped[..4] {
        assert!(
            !jobs_dir.join(format!("{}.json", id.0)).exists(),
            "{id:?} still on disk"
        );
    }
    assert!(jobs_dir.join(format!("{}.json", stopped[4].0)).exists());
    // Reopening prunes too: write back four stale stopped records, older than
    // everything kept, so the three oldest go and the fourth stays.
    drop(node);
    for (i, id) in stopped[..4].iter().enumerate() {
        let rec = JobRecord {
            id: id.clone(),
            state: JobState::Stopped,
            created_ns: Timestamp(i as u64 + 1),
            ..on_disk(dir.path(), &stopped[4])
        };
        std::fs::write(
            jobs_dir.join(format!("{}.json", id.0)),
            serde_json::to_vec(&rec).unwrap(),
        )
        .unwrap();
    }
    let (_src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let list = node.job_list();
    assert_eq!(
        list.iter().filter(|r| r.state == JobState::Stopped).count(),
        JOB_HISTORY_MAX,
        "{list:?}"
    );
    for id in &stopped[..3] {
        assert!(!jobs_dir.join(format!("{}.json", id.0)).exists(), "{id:?}");
        assert!(!list.iter().any(|r| &r.id == id), "{id:?} listed");
    }
    assert!(jobs_dir.join(format!("{}.json", stopped[3].0)).exists());
    assert!(list.iter().any(|r| r.id == stopped[3]));
}
