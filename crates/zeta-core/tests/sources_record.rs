//! A worker port's capabilities are cached in its source record, and
//! `source_list` shows the worker.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use zeta_core::node::{Node, NodeConfig, load_source};
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::{IqReceiver, Port, PortError, Tuning, WorkerInfo};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Capabilities, SourceId};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// A recording that claims to run in a child process, as a Soapy worker port does.
struct WorkerFixture(FileSource);

#[async_trait]
impl Port for WorkerFixture {
    fn id(&self) -> &SourceId {
        self.0.id()
    }
    fn capabilities(&self) -> &Capabilities {
        self.0.capabilities()
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.0.tune(t).await
    }
    fn current(&self) -> Option<Tuning> {
        self.0.current()
    }
    fn subscribe(&self) -> IqReceiver {
        self.0.subscribe()
    }
    fn worker(&self) -> Option<WorkerInfo> {
        Some(WorkerInfo {
            pid: Some(1),
            restarts: 0,
            since_ns: None,
        })
    }
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: 20,
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

fn file(dir: &Path) -> FileSource {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    FileSource::open(SourceId("file0".into()), &meta, opts).unwrap()
}

async fn sources(node: &Node) -> Vec<zeta_core::ops::SourceInfo> {
    let Ok(OpResult::Sources(v)) = node.apply(Op::SourceList {}).await else {
        panic!("expected sources");
    };
    v
}

#[tokio::test]
async fn a_worker_port_writes_its_capabilities_to_the_source_record() {
    let dir = tempfile::tempdir().unwrap();
    let port = Arc::new(WorkerFixture(file(dir.path())));
    let caps = port.capabilities().clone();
    let _node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let rec = load_source(&dir.path().join("state/sources"), &SourceId("file0".into())).unwrap();
    assert_eq!(rec.capabilities, Some(caps));
}

#[tokio::test]
async fn a_plain_port_writes_no_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let port = Arc::new(file(dir.path()));
    let _node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let rec = load_source(&dir.path().join("state/sources"), &SourceId("file0".into()));
    assert!(rec.is_none_or(|r| r.capabilities.is_none() && r.pin.is_none() && r.gain.is_none()));
}

#[tokio::test]
async fn source_list_shows_worker_for_a_worker_port_and_null_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(WorkerFixture(file(dir.path())))],
    )
    .await
    .unwrap();
    let v = sources(&node).await;
    assert_eq!(
        v[0].worker,
        Some(WorkerInfo {
            pid: Some(1),
            restarts: 0,
            since_ns: None
        })
    );
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json[0]["worker"]["pid"], 1);

    let dir2 = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir2.path()),
        registry(),
        vec![Arc::new(file(dir2.path()))],
    )
    .await
    .unwrap();
    let json = serde_json::to_value(sources(&node).await).unwrap();
    assert!(json[0]["worker"].is_null(), "{json}");
}
