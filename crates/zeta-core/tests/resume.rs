//! Resume restores the capture: a source's running records launch widest first, a scan
//! first of all, and `job_start` records the capture rate its graph was expanded for.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{
    Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, StreamId, StreamKind, Timestamp,
};
use zeta_source_file::{FileOpts, FileSource, fixtures};

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
    r.register(Arc::new(IdleFft(StageSpec {
        kind: "fft",
        inputs: vec![StreamKind::Iq],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({}),
    })));
    r
}

fn on_disk(dir: &Path, id: &JobId) -> JobRecord {
    let p = dir
        .join("state")
        .join("jobs")
        .join(format!("{}.json", id.0));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// Stands in for the dsp `fft` stage a scan needs (core cannot depend on it): it
/// emits nothing and ends when cancelled.
struct IdleFft(StageSpec);

#[async_trait]
impl Stage for IdleFft {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: serde_json::Value,
        _inputs: Vec<broadcast::Receiver<Frame>>,
        _outputs: Vec<StreamId>,
        _bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        cancel.cancelled().await;
        Err(StageError::Cancelled)
    }
}

/// A radio that tunes to exactly the band asked for, so its capture is the band of
/// whichever job tuned it first and a wider job after a narrower one conflicts.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
}

impl Radio {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: SourceId("radio0".into()),
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
            },
            cur: Mutex::new(None),
            tx: broadcast::channel(1).0,
        })
    }
}

#[async_trait]
impl Port for Radio {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        *self.cur.lock().unwrap() = Some(t);
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

fn band(bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

/// A `Running` record on `radio0`, as a node persists one.
fn running(id: &JobId, kind: &str, tuning: Tuning, created_ns: u64) -> JobRecord {
    let graph = match kind {
        "scan" => Graph {
            id: GraphId::new(),
            nodes: vec![],
            edges: vec![],
        },
        _ => Graph {
            id: GraphId::new(),
            nodes: vec![GraphNode {
                id: NodeId("p".into()),
                kind: "passthrough".into(),
                params: serde_json::json!({"kind": "iq"}),
            }],
            edges: vec![],
        },
    };
    JobRecord {
        format: JOB_RECORD_FORMAT,
        id: id.clone(),
        kind: kind.into(),
        graph,
        source: SourceId("radio0".into()),
        tuning,
        state: JobState::Running,
        context: serde_json::json!({}),
        created_ns: Timestamp(created_ns),
        input: None,
        capture_rate_hz: None,
        elastic: false,
        operator_graph: false,
        owner: None,
    }
}

fn write_records(dir: &Path, recs: &[JobRecord]) {
    let jobs = dir.join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    for r in recs {
        std::fs::write(
            jobs.join(format!("{}.json", r.id.0)),
            serde_json::to_vec(r).unwrap(),
        )
        .unwrap();
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

#[tokio::test]
async fn resume_launches_the_widest_record_first_whatever_the_file_order() {
    // The same two file names in both cases, so whatever order the directory lists them
    // in, one case puts the narrow record first on disk; the narrow one is older in both.
    let (a, b) = (JobId::new(), JobId::new());
    for (narrow, wide) in [(&a, &b), (&b, &a)] {
        let dir = tempfile::tempdir().unwrap();
        write_records(
            dir.path(),
            &[
                running(narrow, "passthrough", band(9_600), 1),
                running(wide, "passthrough", band(48_000), 2),
            ],
        );
        let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()])
            .await
            .unwrap();
        for id in [narrow, wide] {
            assert_eq!(record(&node, id).state, JobState::Running, "{id:?}");
        }
        let sources = match node.apply(Op::SourceList {}).await.unwrap() {
            OpResult::Sources(v) => v,
            r => panic!("source_list: {r:?}"),
        };
        assert_eq!(sources[0].current.unwrap().bandwidth_hz, Hz(48_000));
    }
}

#[tokio::test]
async fn scan_resumes_first_and_names_itself_in_the_others_reason() {
    let (a, b) = (JobId::new(), JobId::new());
    for (demod, scan) in [(&a, &b), (&b, &a)] {
        let dir = tempfile::tempdir().unwrap();
        write_records(
            dir.path(),
            &[
                running(demod, "passthrough", band(48_000), 1),
                running(scan, "scan", band(40_000_000), 2),
            ],
        );
        let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()])
            .await
            .unwrap();
        assert_eq!(record(&node, scan).state, JobState::Running);
        match record(&node, demod).state {
            JobState::Failed { reason } => {
                assert!(reason.contains(&scan.0), "{reason}");
                assert!(reason.contains("is held by"), "{reason}");
            }
            s => panic!("demod resumed as {s:?}"),
        }
        node.job_stop(scan).await.unwrap();
    }
}

/// Narrow rigid jobs that shared one wide capture: resume tunes the source to the band
/// bounding them all, so every one fits again.
#[tokio::test]
async fn resume_restores_a_capture_shared_by_narrow_rigid_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let at = |center_hz: u64| Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(48_000),
    };
    let ids = [JobId::new(), JobId::new(), JobId::new()];
    write_records(
        dir.path(),
        &[
            running(&ids[0], "passthrough", at(99_200_000), 1),
            running(&ids[1], "passthrough", at(100_000_000), 2),
            running(&ids[2], "passthrough", at(101_000_000), 3),
        ],
    );
    let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()])
        .await
        .unwrap();
    for id in &ids {
        assert_eq!(record(&node, id).state, JobState::Running, "{id:?}");
    }
    let sources = match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v,
        r => panic!("source_list: {r:?}"),
    };
    // 99_176_000 ..= 101_024_000 Hz.
    assert_eq!(
        sources[0].current,
        Some(Tuning {
            center_hz: Hz(100_100_000),
            bandwidth_hz: Hz(1_848_000),
        })
    );
    let mut holders = sources[0].holders.clone();
    holders.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want = ids.to_vec();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(holders, want);
}

/// A scan resumes before every rigid record, even one wider than the scan's band.
#[tokio::test]
async fn a_narrow_scan_still_resumes_before_a_wider_rigid_record() {
    let dir = tempfile::tempdir().unwrap();
    let (demod, scan) = (JobId::new(), JobId::new());
    write_records(
        dir.path(),
        &[
            running(&demod, "passthrough", band(48_000), 1),
            running(&scan, "scan", band(40_000), 2),
        ],
    );
    let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()])
        .await
        .unwrap();
    assert_eq!(record(&node, &scan).state, JobState::Running);
    match record(&node, &demod).state {
        JobState::Failed { reason } => assert!(reason.contains(&scan.0), "{reason}"),
        s => panic!("demod resumed as {s:?}"),
    }
    node.job_stop(&scan).await.unwrap();
}

#[tokio::test]
async fn job_start_records_the_capture_rate() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = source_with(dir.path(), true);
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let op = Op::JobStart {
        kind: JobKind::Passthrough,
        source: SourceId("file0".into()),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(9_600),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    let id = match node.apply(op).await.unwrap() {
        OpResult::Job(id) => id,
        r => panic!("job_start: {r:?}"),
    };
    let rate = port.current().unwrap().bandwidth_hz.0;
    assert_eq!(rate, 48_000);
    assert_eq!(on_disk(dir.path(), &id).capture_rate_hz, Some(rate));
}

/// A record resumed against a port that cannot tune (a device that is not there) fails
/// with the port's error under the source's name, as a job that failed on the faulted
/// source reads, so the node's recovery can tell it from any other failure.
#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn a_record_resumed_against_a_faulted_port_fails_naming_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let meta = fixtures::write_tone(dir.path());
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("s".into()), &meta, opts).unwrap());
    let port = zeta_core::test_fixtures::FaultablePort::new(src);
    port.set_fault(Some("device not found: x".into()));
    let id = JobId::new();
    let mut rec = running(&id, "passthrough", band(48_000), 1);
    rec.source = SourceId("s".into());
    write_records(dir.path(), &[rec]);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let want = JobState::Failed {
        reason: "source s: io: device not found: x".into(),
    };
    assert_eq!(record(&node, &id).state, want);
    assert_eq!(on_disk(dir.path(), &id).state, want);
}

/// A scan persisted before the scan carried a mask (an empty graph) gets the expansion
/// and its source's default mask on resume.
#[tokio::test]
async fn a_scan_record_with_an_empty_graph_resumes_with_the_default_mask() {
    let scan = JobId::new();
    let dir = tempfile::tempdir().unwrap();
    write_records(dir.path(), &[running(&scan, "scan", band(40_000_000), 1)]);
    let node = Node::open(cfg(dir.path()), registry(), vec![Radio::new()])
        .await
        .unwrap();
    assert_eq!(record(&node, &scan).state, JobState::Running);
    let g = &record(&node, &scan).graph;
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(g.nodes[0].kind, "fft");
    assert_eq!(g.nodes[0].params["notch_hz"], 50_000);
    // The radio's whole capture is usable: no edges to mask.
    assert_eq!(g.nodes[0].params["edge_mask"], 0.0);
    let ctx = node.job_context(&scan).unwrap();
    assert_eq!(
        ctx["scan"]["mask"],
        serde_json::json!({"notch_hz": 50_000, "edge_mask": 0.0})
    );
    assert!(ctx["scan"]["alternate"].is_object(), "{ctx}");
    node.job_stop(&scan).await.unwrap();
}
