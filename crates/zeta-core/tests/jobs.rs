//! Node job runtime: persistence, stop, resume and the per-source ring.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zeta_core::graph::{Graph, GraphError, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig, NodeError};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId};
use zeta_source_file::{FileOpts, FileSource, fixtures};

fn source(dir: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    source_with(dir, true)
}

fn source_with(dir: &Path, repeat: bool) -> (Arc<FileSource>, Arc<dyn Port>) {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts { realtime: false, block_samples: 4800, repeat };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig { state_dir: dir.join("state"), ring_seconds: 1.0, backends: vec![Backend::Cpu] }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r
}

fn graph(kind: &str) -> Graph {
    Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode { id: NodeId("p".into()), kind: kind.into(), params: serde_json::json!({"kind": "iq"}) }],
        edges: vec![],
    }
}

fn tuning() -> Tuning {
    Tuning { center_hz: Hz(100_000_000), bandwidth_hz: Hz(48_000) }
}

fn on_disk(dir: &Path, id: &JobId) -> JobRecord {
    let p = dir.join("state").join("jobs").join(format!("{}.json", id.0));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

async fn start_job(node: &Node) -> JobId {
    node.job_start("monitor", SourceId("file0".into()), tuning(), graph("passthrough")).await.unwrap()
}

#[tokio::test]
async fn start_writes_record_and_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
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
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
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
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()]).await.unwrap();
    src.start();
    let id = start_job(&node).await;
    node.job_context_merge(&id, serde_json::json!({"note": "kept"})).unwrap();
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
    let list = node.job_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].state, JobState::Running);
    assert_eq!(node.job_context(&id).unwrap()["note"], "kept");
    assert_eq!(node.lease_holders(&SourceId("file0".into())).len(), 1);
    assert_eq!(on_disk(dir.path(), &id).state, JobState::Running);
}

#[tokio::test]
async fn restart_with_missing_source_marks_failed_not_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
    src.start();
    let id = start_job(&node).await;
    drop(node);
    let node = Node::open(cfg(dir.path()), registry(), vec![]).await.unwrap();
    let list = node.job_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    match &list[0].state {
        JobState::Failed { reason } => assert!(reason.contains("file0"), "{reason}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(matches!(on_disk(dir.path(), &id).state, JobState::Failed { .. }));
}

#[tokio::test]
async fn unknown_stage_rejected_before_start() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
    match node.job_start("monitor", SourceId("file0".into()), tuning(), graph("nope")).await {
        Err(NodeError::Graph(GraphError::UnknownStage { kind, .. })) => assert_eq!(kind, "nope"),
        other => panic!("expected UnknownStage, got {other:?}"),
    }
    assert_eq!(std::fs::read_dir(dir.path().join("state").join("jobs")).unwrap().count(), 0);
    assert!(node.job_list().is_empty());
    assert!(node.lease_holders(&SourceId("file0".into())).is_empty());
}

#[tokio::test]
async fn ring_fills_from_port() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
    src.start();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let ring = node.ring(&SourceId("file0".into())).expect("ring created");
    assert!(ring.read().unwrap().latest().is_some());
}

fn job_streams(node: &Node, id: &JobId) -> usize {
    node.bus().list().iter().filter(|s| s.provenance.job.as_ref() == Some(id)).count()
}

#[tokio::test]
async fn stopped_job_streams_are_unregistered() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
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
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
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
    let node = Node::open(cfg(dir.path()), registry(), vec![port]).await.unwrap();
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
