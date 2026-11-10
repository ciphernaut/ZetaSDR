//! A worker port's capabilities are cached in its source record, and
//! `source_list` shows the worker.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use zeta_core::node::{Node, NodeConfig, load_source};
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::{IqReceiver, Port, PortError, Tuning, WorkerInfo};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Capabilities, Claim, Depth, Hz, PositionFrom, SourceId, SourcePosition};
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
            transport: None,
            transport_dropped: 0,
            starting: None,
        })
    }
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
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
            since_ns: None,
            transport: None,
            transport_dropped: 0,
            starting: None,
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

#[test]
fn a_format_1_record_from_before_capabilities_loads_with_none_and_its_pin() {
    // As a node of plan 19 wrote it: format, pin and gain, no `capabilities`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("b210.json"),
        r#"{"format":1,"pin":{"center_hz":403025000,"bandwidth_hz":5950000},"gain":{"db":40.0}}"#,
    )
    .unwrap();
    let rec = load_source(dir.path(), &SourceId("b210".into())).expect("the record loads");
    assert_eq!(rec.format, 1);
    assert_eq!(rec.capabilities, None);
    assert_eq!(
        rec.pin,
        Some(Tuning {
            center_hz: zeta_core::types::Hz(403_025_000),
            bandwidth_hz: zeta_core::types::Hz(5_950_000),
        })
    );
    assert_eq!(rec.gain.and_then(|g| g.db), Some(40.0));
}

/// A recording that also reports a position of its own, as a KiwiSDR with GPS does.
struct Located(FileSource);

#[async_trait]
impl Port for Located {
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
    fn position(&self) -> Option<SourcePosition> {
        Some(SourcePosition {
            lat: -27.0,
            lon: 153.0,
            alt_m: Some(10.0),
            from: PositionFrom::Status,
        })
    }
}

fn brisbane() -> SourcePosition {
    SourcePosition {
        lat: -27.3842,
        lon: 153.1175,
        alt_m: Some(4.0),
        from: PositionFrom::Configured,
    }
}

#[tokio::test]
async fn source_list_reports_a_configured_position_and_null_without() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(file(dir.path()))],
    )
    .await
    .unwrap();
    let json = serde_json::to_value(sources(&node).await).unwrap();
    assert!(json[0]["position"].is_null(), "{json}");

    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg(dir.path());
    c.source_positions
        .insert(SourceId("file0".into()), brisbane());
    let node = Node::open(c, registry(), vec![Arc::new(file(dir.path()))])
        .await
        .unwrap();
    let json = serde_json::to_value(sources(&node).await).unwrap();
    assert_eq!(
        json[0]["position"],
        serde_json::json!({"lat": -27.3842, "lon": 153.1175, "alt_m": 4.0, "from": "configured"})
    );

    // A port's own position wins over the configured one.
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg(dir.path());
    c.source_positions
        .insert(SourceId("file0".into()), brisbane());
    let node = Node::open(c, registry(), vec![Arc::new(Located(file(dir.path())))])
        .await
        .unwrap();
    let json = serde_json::to_value(sources(&node).await).unwrap();
    assert_eq!(
        json[0]["position"],
        serde_json::json!({"lat": -27.0, "lon": 153.0, "alt_m": 10.0, "from": "status"})
    );
}

#[tokio::test]
async fn the_schema_lists_position_as_nullable() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(file(dir.path()))],
    )
    .await
    .unwrap();
    let schema = node.schema();
    let si = &schema["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"]
            .as_array()
            .unwrap()
            .contains(&"position".into()),
        "{si}"
    );
    let ty = si["properties"]["position"]["type"].to_string();
    assert!(ty.contains("null"), "{si}");
}

/// A worker port whose link has two sample depths, running at 8 bits, and
/// whose latest respawn measured a figure its record (set once per run)
/// could not take.
struct Deep(FileSource, Capabilities);

impl Deep {
    fn new(dir: &Path) -> Self {
        let f = file(dir);
        let mut later = f.capabilities().clone();
        later.depths = vec![eight_bits()];
        Deep(f, later)
    }
}

fn eight_bits() -> Depth {
    Depth {
        bits: 8,
        lossless: false,
        max_bandwidth_hz: Claim::Measured(Hz(30_720_000)),
    }
}

#[async_trait]
impl Port for Deep {
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
            restarts: 1,
            since_ns: None,
            transport: None,
            transport_dropped: 0,
            starting: None,
        })
    }
    fn depth(&self) -> Option<Depth> {
        Some(eight_bits())
    }
    fn record_to_cache(&self) -> Capabilities {
        self.1.clone()
    }
}

#[tokio::test]
async fn a_worker_port_caches_the_record_its_respawn_measured() {
    let dir = tempfile::tempdir().unwrap();
    let port = Arc::new(Deep::new(dir.path()));
    let later = port.1.clone();
    assert_ne!(port.capabilities(), &later);
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    let rec = load_source(&dir.path().join("state/sources"), &SourceId("file0".into())).unwrap();
    assert_eq!(rec.capabilities, Some(later));
    // `source_list` shows the run's record, and the depth in force.
    let v = sources(&node).await;
    assert_eq!(&v[0].capabilities, port.capabilities());
    assert_eq!(v[0].depth, Some(eight_bits()));
}

#[tokio::test]
async fn source_list_shows_the_depth_in_force_and_null_without() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(file(dir.path()))],
    )
    .await
    .unwrap();
    let json = serde_json::to_value(sources(&node).await).unwrap();
    assert!(json[0]["depth"].is_null(), "{json}");
    let schema = node.schema();
    let si = &schema["result"]["$defs"]["SourceInfo"];
    assert!(
        si["required"].as_array().unwrap().contains(&"depth".into()),
        "{si}"
    );

    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg(dir.path()),
        registry(),
        vec![Arc::new(Deep::new(dir.path()))],
    )
    .await
    .unwrap();
    let json = serde_json::to_value(sources(&node).await).unwrap();
    assert_eq!(
        json[0]["depth"],
        serde_json::json!({"bits": 8, "lossless": false, "max_bandwidth_hz": {"measured": 30720000}})
    );
}
