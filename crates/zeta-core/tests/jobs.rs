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
        owns_state_dir: true,
        bank_factory: None,
        modes_dir: None,
        handlers_dir: None,
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
    start_job_at(node, tuning()).await
}

/// A `monitor` job on `band` (inside the capture: [`tuning`] or narrower).
async fn start_job_at(node: &Node, band: Tuning) -> JobId {
    node.job_start(
        "monitor",
        SourceId("file0".into()),
        band,
        graph("passthrough"),
        None,
        false,
    )
    .await
    .unwrap()
}

/// The `i`th of a run of distinct bands inside the capture: [`tuning`] `i` Hz narrower,
/// so no two of them are one job re-run.
fn nth_band(i: usize) -> Tuning {
    Tuning {
        bandwidth_hz: Hz(tuning().bandwidth_hz.0 - i as u64),
        ..tuning()
    }
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
            owns_state_dir: true,
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
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
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
/// records survive, in memory and on disk; a running job is never pruned. (Each on a
/// band of its own, so none replaces another as its re-run.)
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
    for i in 0..JOB_HISTORY_MAX + 3 {
        let id = start_job_at(&node, nth_band(i + 1)).await;
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
            tuning: nth_band(i + 1),
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

/// The node keeps the 50 newest terminal records.
#[test]
fn the_history_cap_is_50() {
    assert_eq!(zeta_core::node::JOB_HISTORY_MAX, 50);
}

/// A stopped job's re-run (the same kind on the same source and band) takes its
/// predecessor's history slot once it stops: the older record and its file go.
#[tokio::test]
async fn a_rerun_of_a_stopped_job_replaces_its_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let first = start_job(&node).await;
    node.job_stop(&first).await.unwrap();
    let again = start_job(&node).await;
    // Running, the re-run takes no slot: its predecessor is still kept, through a
    // `graph_put` restart of the re-run too (briefly `stopped` inside it).
    assert_eq!(node.job_list().len(), 2);
    node.job_graph_put(&again, graph("passthrough"))
        .await
        .unwrap();
    assert_eq!(node.job_list().len(), 2, "{:?}", node.job_list());
    node.job_stop(&again).await.unwrap();
    let list = node.job_list();
    let ids: Vec<_> = list.iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids, vec![again.clone()], "{list:?}");
    let jobs_dir = dir.path().join("state").join("jobs");
    assert!(!jobs_dir.join(format!("{}.json", first.0)).exists());
    assert!(jobs_dir.join(format!("{}.json", again.0)).exists());
    // Another kind, or another band, is no re-run.
    let narrower = start_job_at(&node, nth_band(1)).await;
    node.job_stop(&narrower).await.unwrap();
    let other = node
        .job_start(
            "other",
            SourceId("file0".into()),
            tuning(),
            graph("passthrough"),
            None,
            false,
        )
        .await
        .unwrap();
    node.job_stop(&other).await.unwrap();
    let mut ids: Vec<_> = node.job_list().into_iter().map(|r| r.id).collect();
    ids.sort();
    let mut want = vec![again, narrower, other];
    want.sort();
    assert_eq!(ids, want);
}

/// A stranger's older stopped record survives any number of re-runs of another job:
/// each re-run replaces its own predecessor, never the oldest record.
#[tokio::test]
async fn a_strangers_record_survives_reruns_of_another_job() {
    use zeta_core::node::JOB_HISTORY_MAX;
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let stranger = start_job_at(&node, nth_band(1)).await;
    node.job_stop(&stranger).await.unwrap();
    // A test's job, run and stopped more times than the history holds.
    let mut last = None;
    for _ in 0..JOB_HISTORY_MAX + 5 {
        let id = start_job(&node).await;
        node.job_stop(&id).await.unwrap();
        last = Some(id);
    }
    let mut ids: Vec<_> = node.job_list().into_iter().map(|r| r.id).collect();
    ids.sort();
    let mut want = vec![stranger, last.unwrap()];
    want.sort();
    assert_eq!(ids, want, "the stranger and the last re-run");
}

// Plan 42: a job runs the handler it names or the one the policy picks.

mod common;
use common::handlers::{self as h, ALT, MODE, OWN};
use zeta_core::modes::policy::Passed;
use zeta_core::ops::{JobKind, Op, OpError, OpResult};
use zeta_core::types::StreamKind;

/// A node on [`h::Radio`] with the `alt` handler file (packets beside its events when
/// `packets`) and the `external_decode` stand-in.
async fn handler_node(dir: &Path, packets: bool) -> Node {
    let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
    Node::open(h::cfg(dir, packets), h::stages(None), ports)
        .await
        .unwrap()
}

fn listed(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

/// The params of the job's `external_decode` node.
fn decode_params(rec: &JobRecord) -> serde_json::Value {
    rec.graph
        .nodes
        .iter()
        .find(|n| n.kind == "external_decode")
        .expect("an external_decode node")
        .params
        .clone()
}

/// A `job_start` of `fm_demod` at 100 MHz with `handler` and `needs`.
fn fm_op(handler: Option<&str>, needs: &[StreamKind]) -> zeta_core::ops::Op {
    h::start_kind_op(JobKind::FmDemod, 100_000_000, 12_500, handler, needs)
}

#[tokio::test]
async fn job_start_takes_the_policys_handler_and_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let chosen = h::start(&node, None, &[]).await;
    let rec = listed(&node, &chosen);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(OWN), false)
    );
    assert_eq!(
        decode_params(&rec),
        serde_json::json!({"mode": MODE, "bandwidth_hz": 2_400_000})
    );
    // `job_list` gives it, and the record on disk keeps it.
    let wire = serde_json::to_value(&rec).unwrap();
    assert_eq!(wire["handler"], OWN);
    assert_eq!(wire["handler_named"], false);
    assert_eq!(on_disk(dir.path(), &chosen).handler.as_deref(), Some(OWN));
    node.job_stop(&chosen).await.unwrap();

    let named = h::start(&node, Some(ALT), &[]).await;
    let rec = listed(&node, &named);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(ALT), true)
    );
    assert_eq!(
        decode_params(&rec),
        serde_json::json!({"mode": MODE, "handler": ALT, "bandwidth_hz": 2_400_000})
    );
    node.job_stop(&named).await.unwrap();
    // A kind of no mode row records none.
    let Ok(OpResult::Job(fm)) = node.apply(fm_op(None, &[])).await else {
        panic!("fm_demod start")
    };
    let rec = listed(&node, &fm);
    assert_eq!((rec.handler, rec.handler_named), (None, false));
    assert_eq!(
        serde_json::to_value(listed(&node, &fm)).unwrap()["handler"],
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn a_named_handler_that_is_unavailable_is_refused_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    h::alt_goes_missing(&node);
    let r = h::try_start(&node, Some(ALT), &[]).await;
    assert_eq!(
        r,
        Err(OpError::Other {
            message: format!("{MODE}: handler {ALT} is unavailable: {}", h::ALT_MISSING),
        })
    );
    assert!(node.job_list().is_empty(), "nothing started");
    // The policy passes it over.
    let id = h::start(&node, None, &[]).await;
    assert_eq!(listed(&node, &id).handler.as_deref(), Some(OWN));
}

/// Set in the re-run of [`a_node_opened_without_the_cli_probes_its_handler_files`] whose
/// environment pins `onair-rx` missing.
const NO_ONAIR_CHILD: &str = "ZETA_JOBS_NO_ONAIR_CHILD";

/// A node opened directly (no `zeta` crate probe before it) looks up its handler files'
/// programs itself: the embedded OnAir with no `onair-rx` is unavailable, with the
/// lookup's reason, and a start naming it is refused with that reason rather than
/// failing at launch. The lookup reads the process's environment, so the test re-runs
/// itself in a child whose `ZETA_HANDLER_ONAIR_RX` names no program (no `set_var` in a
/// threaded test binary): whatever the developer's shell or `PATH` has, it is missing.
#[tokio::test]
async fn a_node_opened_without_the_cli_probes_its_handler_files() {
    if std::env::var_os(NO_ONAIR_CHILD).is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "a_node_opened_without_the_cli_probes_its_handler_files",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(NO_ONAIR_CHILD, "1")
            .env("ZETA_HANDLER_ONAIR_RX", "/nonexistent/zeta-test/onair-rx")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("1 passed"), "{text}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
    let node = Node::open(common::cfg(dir.path(), true), h::stages(None), ports)
        .await
        .unwrap();
    let modes = node.modes();
    let onair = modes.handler(MODE, "onair").expect("the embedded onair");
    assert_eq!(onair.state, zeta_core::modes::ModeState::Unavailable);
    assert_eq!(onair.program, None);
    let reason = onair.reason.clone().unwrap_or_default();
    assert_eq!(
        reason,
        "onair-rx: missing (https://github.com/ciphernaut/onair-rx) — modes: acars_decode, \
         adsb_decode, ais_decode, aprs_decode, ble_decode, dfm_decode, \
         dmr_decode, dvbs_ip_decode, dvbt_decode, flex_decode, gsmr_decode, \
         hfdl_decode, lorawan_decode, lte_decode, m17_decode, m20_decode, \
         p25_decode, pocsag_decode, rds_decode, rs41_decode, tetra_decode, \
         thread_decode, vdl2_decode, wifi_decode, wisun_decode, zigbee_decode, \
         zwave_decode"
    );
    let r = h::try_start(&node, Some("onair"), &[]).await;
    assert_eq!(
        r,
        Err(OpError::Other {
            message: format!("{MODE}: handler onair is unavailable: {reason}"),
        })
    );
    assert!(node.job_list().is_empty(), "nothing started");
}

#[tokio::test]
async fn an_unknown_handler_names_the_known_ones() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let r = h::try_start(&node, Some("nope"), &[]).await;
    assert_eq!(
        r,
        Err(OpError::Unknown {
            field: "handler".into(),
            value: "nope".into(),
            known: vec![OWN.into(), ALT.into()],
        })
    );
    assert!(node.job_list().is_empty(), "nothing started");
}

#[tokio::test]
async fn needs_packet_with_no_such_handler_is_no_handler() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let r = h::try_start(&node, None, &[StreamKind::Packet]).await;
    let passed = |id: &str| Passed {
        id: id.into(),
        why: "outputs no packet".into(),
    };
    assert_eq!(
        r,
        Err(OpError::NoHandler {
            mode: MODE.into(),
            passed: vec![passed(OWN), passed(ALT)],
        })
    );
    // `needs` takes the kinds a decoder outputs only.
    let r = h::try_start(&node, None, &[StreamKind::Iq]).await;
    assert_eq!(
        r,
        Err(OpError::Unknown {
            field: "needs".into(),
            value: "iq".into(),
            known: vec!["event".into(), "packet".into(), "audio".into()],
        })
    );
    assert!(node.job_list().is_empty(), "nothing started");

    // With alt giving packets, `needs` picks it over the row's own.
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), true).await;
    let id = h::start(&node, None, &[StreamKind::Packet]).await;
    let rec = listed(&node, &id);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(ALT), false)
    );
}

#[tokio::test]
async fn handler_and_needs_are_refused_on_a_kind_that_is_no_mode_row() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let refused = Err(OpError::Other {
        message: "fm_demod has no handlers".into(),
    });
    assert_eq!(node.apply(fm_op(Some("x"), &[])).await, refused);
    assert_eq!(node.apply(fm_op(None, &[StreamKind::Event])).await, refused);
    assert!(node.job_list().is_empty(), "nothing started");
    assert!(matches!(
        node.apply(fm_op(None, &[])).await,
        Ok(OpResult::Job(_))
    ));
}

#[tokio::test]
async fn the_same_mode_on_the_same_band_with_another_handler_is_not_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let own = h::start(&node, None, &[]).await;
    let alt = h::start(&node, Some(ALT), &[]).await;
    assert_ne!(own, alt);
    // The same handler again is the first's repeat, as before handlers.
    let again = |job: &JobId| {
        Err(OpError::Duplicate {
            message: format!(
                "an identical {MODE} already runs on {} at 1090000000/2400000 Hz as job {}",
                h::RADIO,
                job.0
            ),
            job: job.clone(),
        })
    };
    assert_eq!(h::try_start(&node, None, &[]).await, again(&own));
    assert_eq!(h::try_start(&node, Some(OWN), &[]).await, again(&own));
    assert_eq!(h::try_start(&node, Some(ALT), &[]).await, again(&alt));
    assert_eq!(node.job_list().len(), 2);
}

// Plan 43: Send to starts a sibling beside a running job, and both carry its group.

/// [`handler_node`], with its radio.
async fn handler_node_with_radio(dir: &Path, packets: bool) -> (Node, Arc<h::Radio>) {
    let radio = h::Radio::new();
    let ports: Vec<Arc<dyn Port>> = vec![radio.clone()];
    let node = Node::open(h::cfg(dir, packets), h::stages(None), ports)
        .await
        .unwrap();
    (node, radio)
}

/// The id of a job a start answered.
fn started(r: Result<OpResult, OpError>) -> JobId {
    match r {
        Ok(OpResult::Job(id)) => id,
        r => panic!("a job: {r:?}"),
    }
}

/// Wait until every job of `ids` is listed `Running`.
async fn until_running(node: &Node, ids: &[&JobId]) {
    common::until("the jobs run", Duration::from_secs(10), || {
        ids.iter()
            .all(|id| listed(node, id).state == JobState::Running)
    })
    .await;
}

#[tokio::test]
async fn a_sibling_starts_beside_a_running_job_and_both_carry_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = handler_node_with_radio(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    until_running(&node, &[&first]).await;
    let tunes = radio.tunes();
    let sib = started(h::start_beside(&node, &first, Some(ALT), &[]).await);
    until_running(&node, &[&first, &sib]).await;
    assert_eq!(listed(&node, &first).group, Some(first.clone()));
    let rec = listed(&node, &sib);
    assert_eq!(rec.group, Some(first.clone()));
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(ALT), true)
    );
    assert_eq!(rec.tuning, listed(&node, &first).tuning);
    assert_eq!(radio.tunes(), tunes, "the sibling's start tunes nothing");
    assert_eq!(on_disk(dir.path(), &first).group, Some(first.clone()));
    assert_eq!(on_disk(dir.path(), &sib).group, Some(first.clone()));
    let wire = serde_json::to_value(listed(&node, &sib)).unwrap();
    assert_eq!(wire["group"], first.0.as_str());
}

#[tokio::test]
async fn send_to_from_a_sibling_joins_its_group() {
    let dir = tempfile::tempdir().unwrap();
    h::write_handler(dir.path(), "alt2", false);
    let node = handler_node(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    let sib = started(h::start_beside(&node, &first, Some(ALT), &[]).await);
    let third = started(h::start_beside(&node, &sib, Some("alt2"), &[]).await);
    until_running(&node, &[&first, &sib, &third]).await;
    for id in [&first, &sib, &third] {
        assert_eq!(listed(&node, id).group, Some(first.clone()), "{id:?}");
    }
    let members: Vec<JobId> = node
        .group_members(&first)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(members, vec![first, sib, third], "oldest first");
}

#[tokio::test]
async fn beside_refuses_a_handler_the_group_already_runs() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    let refused = |handler: &str, member: &JobId| {
        Err(OpError::Duplicate {
            message: format!(
                "handler {handler} already runs beside job {} as job {}",
                first.0, member.0
            ),
            job: member.clone(),
        })
    };
    assert_eq!(
        h::start_beside(&node, &first, Some(OWN), &[]).await,
        refused(OWN, &first)
    );
    assert_eq!(
        listed(&node, &first).group,
        None,
        "a refusal writes no group"
    );
    let sib = started(h::start_beside(&node, &first, Some(ALT), &[]).await);
    assert_eq!(
        h::start_beside(&node, &first, Some(ALT), &[]).await,
        refused(ALT, &sib)
    );
    // Named from the sibling, `beside job` is the job named.
    assert_eq!(
        h::start_beside(&node, &sib, Some(OWN), &[]).await,
        Err(OpError::Duplicate {
            message: format!(
                "handler {OWN} already runs beside job {} as job {}",
                sib.0, first.0
            ),
            job: first.clone(),
        })
    );
    assert_eq!(node.job_list().len(), 2);
}

/// A job outside the group running the handler on the band is the ordinary duplicate,
/// checked after the group's (design call 28).
#[tokio::test]
async fn beside_a_handler_run_outside_the_group_is_a_duplicate_naming_that_job() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    let stranger = h::start(&node, Some(ALT), &[]).await;
    assert_eq!(
        h::start_beside(&node, &first, Some(ALT), &[]).await,
        Err(OpError::Duplicate {
            message: format!(
                "an identical {MODE} already runs on {} at 1090000000/2400000 Hz as job {}",
                h::RADIO,
                stranger.0
            ),
            job: stranger.clone(),
        })
    );
    assert_eq!(listed(&node, &first).group, None);
    assert_eq!(listed(&node, &stranger).group, None);
}

#[tokio::test]
async fn beside_without_a_handler_names_the_free_ones() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    assert_eq!(
        h::start_beside(&node, &first, None, &[]).await,
        Err(OpError::Other {
            message: format!("beside needs a handler: one of {ALT}"),
        })
    );
    started(h::start_beside(&node, &first, Some(ALT), &[]).await);
    assert_eq!(
        h::start_beside(&node, &first, None, &[]).await,
        Err(OpError::Other {
            message: "beside needs a handler: none is free".into(),
        })
    );
}

#[tokio::test]
async fn beside_refuses_a_band_that_is_not_the_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    let refused = |field: &str, asked: &str, its: &str| {
        Err(OpError::Other {
            message: format!(
                "beside job {}: {field} is {asked}, the job's is {its}",
                first.0
            ),
        })
    };
    let apply = |op: serde_json::Value| node.apply(serde_json::from_value(op).unwrap());
    let mut op = h::beside_op(&node, &first, Some(ALT), &[]);
    op["center_hz"] = 1_090_001_000u64.into();
    assert_eq!(
        apply(op).await,
        refused("center_hz", "1090001000", "1090000000")
    );
    let mut op = h::beside_op(&node, &first, Some(ALT), &[]);
    op["bandwidth_hz"] = 2_000_000u64.into();
    assert_eq!(
        apply(op).await,
        refused("bandwidth_hz", "2000000", "2400000")
    );
    // A `replace` is refused with it: Send to stops nothing.
    let mut op = h::beside_op(&node, &first, Some(ALT), &[]);
    op["replace"] = serde_json::json!([first.0]);
    assert_eq!(
        apply(op).await,
        Err(OpError::Other {
            message: "beside: replace must be empty (Send to stops nothing)".into(),
        })
    );
    // An unknown job is `unknown`.
    let ghost = JobId::new();
    let mut op = h::beside_op(&node, &first, Some(ALT), &[]);
    op["beside"] = ghost.0.clone().into();
    assert!(matches!(
        apply(op).await,
        Err(OpError::Unknown { field, value, .. }) if field == "job" && value == ghost.0
    ));
    assert_eq!(node.job_list().len(), 1, "nothing started");
    assert_eq!(listed(&node, &first).group, None);
}

#[tokio::test]
async fn beside_refuses_a_job_that_cannot_be_sent_from() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let Ok(OpResult::Job(fm)) = node.apply(fm_op(None, &[])).await else {
        panic!("fm_demod start")
    };
    until_running(&node, &[&fm]).await;
    let op = serde_json::json!({
        "op": "job_start", "kind": MODE, "source": h::RADIO,
        "center_hz": 100_000_000u64, "bandwidth_hz": 12_500u64,
        "deviation_hz": null, "sideband": null, "elastic": false, "replace": [],
        "handler": ALT, "needs": [], "beside": fm.0,
    });
    assert_eq!(
        node.apply(serde_json::from_value(op).unwrap()).await,
        Err(OpError::Other {
            message: format!("job {} cannot be sent from: fm_demod has no handlers", fm.0),
        })
    );
}

#[tokio::test]
async fn beside_refuses_a_stopped_job_and_writes_no_group() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    node.job_stop(&first).await.unwrap();
    common::until("the job stops", Duration::from_secs(10), || {
        listed(&node, &first).state == JobState::Stopped
    })
    .await;
    assert_eq!(
        h::start_beside(&node, &first, Some(ALT), &[]).await,
        Err(OpError::Other {
            message: format!("job {} is stopped", first.0),
        })
    );
    assert_eq!(listed(&node, &first).group, None);
    assert_eq!(node.job_list().len(), 1);
}

#[tokio::test]
async fn stopping_the_first_member_leaves_its_sibling_running_in_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    let sib = started(h::start_beside(&node, &first, Some(ALT), &[]).await);
    until_running(&node, &[&first, &sib]).await;
    node.job_stop(&first).await.unwrap();
    common::until("the first job stops", Duration::from_secs(10), || {
        listed(&node, &first).state == JobState::Stopped
    })
    .await;
    let rec = listed(&node, &sib);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.group, Some(first.clone()));
    let members: Vec<JobId> = node
        .group_members(&first)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(members, vec![sib]);
}

#[tokio::test]
async fn a_failed_sibling_start_leaves_the_first_job_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let first = h::start(&node, None, &[]).await;
    until_running(&node, &[&first]).await;
    let before = listed(&node, &first);
    h::alt_goes_missing(&node);
    assert_eq!(
        h::start_beside(&node, &first, Some(ALT), &[]).await,
        Err(OpError::Other {
            message: format!("{MODE}: handler {ALT} is unavailable: {}", h::ALT_MISSING),
        })
    );
    let after = listed(&node, &first);
    assert_eq!(after.state, JobState::Running);
    assert_eq!(after.group, None);
    assert_eq!(after.tuning, before.tuning);
    assert_eq!(on_disk(dir.path(), &first).group, None);
    assert_eq!(node.job_list().len(), 1);
}

/// History keeps one terminal record per handler: a stopped job of another handler on
/// the same band is no re-run of the first (both records stay, run side by side or one
/// after the other), while a re-run on the same handler still takes its predecessor's
/// slot. A record from before handlers counts as the row's own handler's.
#[tokio::test]
async fn a_stopped_job_of_another_handler_keeps_its_record() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path(), false).await;
    let ids = |node: &Node| {
        let mut ids: Vec<JobId> = node.job_list().into_iter().map(|r| r.id).collect();
        ids.sort();
        ids
    };
    let sorted = |mut v: Vec<JobId>| {
        v.sort();
        v
    };
    // Side by side, then stopped.
    let own = h::start(&node, Some(OWN), &[]).await;
    let alt = h::start(&node, Some(ALT), &[]).await;
    node.job_stop(&own).await.unwrap();
    node.job_stop(&alt).await.unwrap();
    assert_eq!(ids(&node), sorted(vec![own.clone(), alt.clone()]));
    // One after the other: alt again, started after the own job ended.
    let alt2 = h::start(&node, Some(ALT), &[]).await;
    node.job_stop(&alt2).await.unwrap();
    assert_eq!(ids(&node), sorted(vec![own.clone(), alt2.clone()]));
    // The own record made one from before handlers: the next own job is its re-run.
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let p = dir
        .path()
        .join("state")
        .join("jobs")
        .join(format!("{}.json", own.0));
    let mut old: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    old["handler"] = serde_json::Value::Null;
    old["handler_named"] = false.into();
    std::fs::write(&p, old.to_string()).unwrap();
    let node = handler_node(dir.path(), false).await;
    assert_eq!(listed(&node, &own).handler, None);
    let own2 = h::start(&node, None, &[]).await;
    assert_eq!(listed(&node, &own2).handler.as_deref(), Some(OWN));
    node.job_stop(&own2).await.unwrap();
    assert_eq!(ids(&node), sorted(vec![alt2, own2]));
}

// Plan 47: a row with no decoder of its own starts only through a handler.

/// A node on [`h::Radio`] over [`h::nodec_cfg`] (`handler` as there) with the
/// `external_decode` stand-in, its sets read from `<dir>/sets`.
async fn nodec_node(dir: &Path, handler: Option<bool>) -> Node {
    let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
    let cfg = NodeConfig {
        sets_dir: Some(dir.join("sets")),
        ..h::nodec_cfg(dir, handler)
    };
    Node::open(cfg, h::stages(None), ports).await.unwrap()
}

/// `mode nodec_decode is unavailable: <reason>`.
fn nodec_refusal(reason: &str) -> Result<OpResult, OpError> {
    Err(OpError::Other {
        message: format!("mode {} is unavailable: {reason}", h::NODEC),
    })
}

#[tokio::test]
async fn a_row_with_no_decoder_starts_through_its_handler() {
    let dir = tempfile::tempdir().unwrap();
    let node = nodec_node(dir.path(), Some(true)).await;
    // Named by none, the policy's pick runs, and the record names it.
    let Ok(OpResult::Job(id)) = node.apply(h::nodec_op(None)).await else {
        panic!("nodec_decode start")
    };
    let rec = listed(&node, &id);
    assert_eq!(
        (rec.handler.as_deref(), rec.handler_named),
        (Some(ALT), false)
    );
    assert_eq!(
        decode_params(&rec),
        serde_json::json!({"mode": h::NODEC, "handler": ALT, "bandwidth_hz": 2_400_000})
    );
    assert_eq!(on_disk(dir.path(), &id).handler.as_deref(), Some(ALT));
    node.job_stop(&id).await.unwrap();
    // Named, it runs; the stand-in is refused with its reason, never launched.
    let Ok(OpResult::Job(named)) = node.apply(h::nodec_op(Some(ALT))).await else {
        panic!("nodec_decode start naming alt")
    };
    assert_eq!(listed(&node, &named).handler.as_deref(), Some(ALT));
    node.job_stop(&named).await.unwrap();
    let before = node.job_list().len();
    assert_eq!(
        node.apply(h::nodec_op(Some("none"))).await,
        Err(OpError::Other {
            message: format!(
                "{}: handler none is unavailable: the row has no decoder of its own",
                h::NODEC
            ),
        })
    );
    // An unknown one names the handlers there are, the stand-in left out.
    assert_eq!(
        node.apply(h::nodec_op(Some("nope"))).await,
        Err(OpError::Unknown {
            field: "handler".into(),
            value: "nope".into(),
            known: vec![ALT.into()],
        })
    );
    assert_eq!(node.job_list().len(), before, "nothing more started");

    // No handler file serving it: refused with the row's reason.
    let dir = tempfile::tempdir().unwrap();
    let node = nodec_node(dir.path(), None).await;
    let refusal = nodec_refusal(h::NO_DECODER_REASON);
    assert_eq!(node.apply(h::nodec_op(None)).await, refusal);
    let needs = h::start_kind_op(
        JobKind::Mode(zeta_core::modes::ModeId(h::NODEC.into())),
        1_090_000_000,
        2_400_000,
        None,
        &[StreamKind::Packet],
    );
    assert_eq!(node.apply(needs).await, refusal);
    assert!(node.job_list().is_empty(), "nothing started");

    // Its handler's program missing: the reason names it.
    let dir = tempfile::tempdir().unwrap();
    let node = nodec_node(dir.path(), Some(false)).await;
    let why = node
        .modes()
        .handler(h::NODEC, ALT)
        .and_then(|a| a.reason.clone())
        .unwrap();
    let reason = format!("{}; {ALT}: {why}", h::NO_DECODER_REASON);
    assert_eq!(node.apply(h::nodec_op(None)).await, nodec_refusal(&reason));
    assert!(node.job_list().is_empty(), "nothing started");
}

/// Every other way a job starts refuses a row with no decoder and no handler, with the
/// row's reason: a set's member, a set's watch naming it, a watch's `graph_put` naming
/// it, a plan, and a capture's replay. (Identify passes over it: `identify.rs`.)
#[tokio::test]
async fn nothing_starts_a_row_with_no_decoder_and_no_handler() {
    let dir = tempfile::tempdir().unwrap();
    let sets = dir.path().join("sets");
    std::fs::create_dir_all(&sets).unwrap();
    let member = |id: &str, kind: &str, params: &str| {
        format!(
            r#"{{"id": "{id}", "radio": "r", "op": "job_start", "kind": "{kind}",
                "center_hz": 1090000000, "bandwidth_hz": 2400000, "deviation_hz": null,
                "sideband": null{params}}}"#
        )
    };
    let set = format!(
        r#"{{"id": "nodec", "name": "nodec", "notes": "", "radios": {{"r": {{"note": "any"}}}},
            "members": [{}, {}]}}"#,
        member("decoder", h::NODEC, ""),
        member(
            "watch",
            "watch",
            &format!(r#", "params": {{"modes": ["{}"]}}"#, h::NODEC)
        ),
    );
    std::fs::write(sets.join("nodec.json"), set).unwrap();
    let node = nodec_node(dir.path(), None).await;
    let reason = h::NO_DECODER_REASON;

    // A set: each member refused, nothing started.
    let r = node
        .apply(Op::SetStart {
            set: "nodec".into(),
            radios: [("r".to_string(), Some(SourceId(h::RADIO.into())))]
                .into_iter()
                .collect(),
            members: None,
            replace: vec![],
        })
        .await;
    let Ok(OpResult::SetStart {
        started, refused, ..
    }) = r
    else {
        panic!("set_start: {r:?}")
    };
    assert!(started.is_empty(), "{started:?}");
    let refusals: Vec<(String, String)> = refused
        .iter()
        .map(|r| (r.member.clone(), r.error.to_string()))
        .collect();
    assert_eq!(refusals.len(), 2, "{refusals:?}");
    for (member, error) in &refusals {
        assert!(error.contains(reason), "{member}: {error}");
    }

    // A watch's `modes` naming it, put on a running watch.
    let watch = h::start_kind_op(JobKind::Watch, 1_090_000_000, 2_400_000, None, &[]);
    let Ok(OpResult::Job(watch)) = node.apply(watch).await else {
        panic!("watch start")
    };
    let mut graph = listed(&node, &watch).graph;
    let detect = graph.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
    detect.params["modes"] = serde_json::json!([h::NODEC]);
    let r = node
        .apply(Op::GraphPut {
            job: watch.clone(),
            graph,
        })
        .await;
    let e = r.expect_err("graph_put naming the row");
    assert!(e.to_string().contains(reason), "{e}");
    node.job_stop(&watch).await.unwrap();

    // A plan.
    let r = node
        .apply(Op::PlanStart {
            plan: "uhf_cb_au".into(),
            channels: Some(vec![1]),
            group: None,
            kind: JobKind::Mode(zeta_core::modes::ModeId(h::NODEC.into())),
            source: SourceId(h::RADIO.into()),
            deviation_hz: None,
            sideband: None,
            attach: None,
            language: None,
            replace: vec![],
        })
        .await;
    let text = match &r {
        Ok(OpResult::PlanStart {
            started, refused, ..
        }) => {
            assert!(started.is_empty(), "{started:?}");
            serde_json::to_string(refused).unwrap()
        }
        Err(e) => e.to_string(),
        r => panic!("plan_start: {r:?}"),
    };
    assert!(text.contains(reason), "{text}");

    // A capture's replay.
    let rate = 2_400_000u64;
    let iq = vec![Complex::new(0.0f32, 0.0); rate as usize / 10];
    let capture = node
        .bursts()
        .write(
            zeta_core::bursts::NewCapture {
                source: SourceId(h::RADIO.into()),
                set: JobId("nodec".into()),
                center_hz: Hz(1_090_000_000),
                bandwidth_hz: Hz(rate),
                sample_rate_hz: Hz(rate),
                start_ns: 1_760_000_000_000_000_000,
                end_ns: 1_760_000_000_100_000_000,
                features: serde_json::json!({}),
                signal: None,
                job: JobId("nodec".into()),
                watch: None,
                allocations: serde_json::json!([]),
                file_center_hz: None,
                trial: false,
            },
            &iq,
        )
        .unwrap()
        .id;
    for packets in [false, true] {
        let r = node
            .apply(Op::CaptureDecode {
                capture: capture.clone(),
                kind: h::NODEC.into(),
                handler: None,
                packets,
            })
            .await;
        assert_eq!(r, nodec_refusal(reason), "packets {packets}");
    }
    assert!(
        node.job_list()
            .iter()
            .all(|r| r.kind != h::NODEC && r.state != JobState::Running),
        "{:?}",
        node.job_list()
    );
}
