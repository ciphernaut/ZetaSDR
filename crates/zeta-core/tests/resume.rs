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
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
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
    };
    let id = match node.apply(op).await.unwrap() {
        OpResult::Job(id) => id,
        r => panic!("job_start: {r:?}"),
    };
    let rate = port.current().unwrap().bandwidth_hz.0;
    assert_eq!(rate, 48_000);
    assert_eq!(on_disk(dir.path(), &id).capture_rate_hz, Some(rate));
}
