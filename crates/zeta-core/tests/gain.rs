//! Live receive gain: `source_set_gain` sets a port's gain through `Port::set_gain`,
//! `source_list` shows it as `gain`, and the value is persisted in
//! `sources/<id>.json` and re-applied when the node opens.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;

use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpError, OpResult, SourceInfo};
use zeta_core::port::{GainInfo, IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, Timestamp};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// A radio with a 0..=76 dB gain, automatic gain where `automatic_available`;
/// every tune and gain call is logged in order. With `fail` set every gain call
/// is refused with `amplifier on fire`.
struct Amp {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    gain: Mutex<GainInfo>,
    fail: bool,
    log: Mutex<Vec<String>>,
    tx: broadcast::Sender<IqBlock>,
}

impl Amp {
    fn new(automatic_available: bool, fail: bool) -> Arc<Self> {
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
            gain: Mutex::new(GainInfo {
                db: Some(0.0),
                min_db: 0.0,
                max_db: 76.0,
                automatic: false,
                automatic_available,
            }),
            fail,
            log: Mutex::new(Vec::new()),
            tx: broadcast::channel(1).0,
        })
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait]
impl Port for Amp {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.log.lock().unwrap().push("tune".into());
        *self.cur.lock().unwrap() = Some(t);
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
    fn gain(&self) -> Option<GainInfo> {
        Some(*self.gain.lock().unwrap())
    }
    async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        self.log.lock().unwrap().push(format!("set_gain({db:?})"));
        if self.fail {
            return Err(PortError::Io("amplifier on fire".into()));
        }
        let mut g = self.gain.lock().unwrap();
        match db {
            None if !g.automatic_available => {
                return Err(PortError::Unsupported(format!(
                    "source {} has no automatic gain",
                    self.id.0
                )));
            }
            None => {
                g.db = None;
                g.automatic = true;
            }
            Some(d) if !(g.min_db..=g.max_db).contains(&d) => {
                return Err(PortError::Unsupported(format!(
                    "gain_db {d} out of range {}..={} dB for source {}",
                    g.min_db, g.max_db, self.id.0
                )));
            }
            Some(d) => {
                g.db = Some(d);
                g.automatic = false;
            }
        }
        Ok(*g)
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r
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

async fn open(dir: &Path, port: Arc<dyn Port>) -> Node {
    Node::open(cfg(dir), registry(), vec![port]).await.unwrap()
}

fn set_gain(source: &str, gain_db: Option<f64>) -> Op {
    Op::SourceSetGain {
        source: SourceId(source.into()),
        gain_db,
    }
}

async fn sources(node: &Node) -> Vec<SourceInfo> {
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v,
        r => panic!("source_list: {r:?}"),
    }
}

fn source_file(dir: &Path) -> std::path::PathBuf {
    dir.join("state").join("sources").join("radio0.json")
}

fn on_disk(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(source_file(dir)).unwrap()).unwrap()
}

fn other(r: Result<OpResult, OpError>) -> String {
    match r {
        Err(OpError::Other { message }) => message,
        r => panic!("expected other, got {r:?}"),
    }
}

#[tokio::test]
async fn set_gain_round_trips_and_is_listed_and_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let amp = Amp::new(true, false);
    let node = open(dir.path(), amp.clone()).await;
    let want = GainInfo {
        db: Some(40.0),
        min_db: 0.0,
        max_db: 76.0,
        automatic: false,
        automatic_available: true,
    };
    let got = node.apply(set_gain("radio0", Some(40.0))).await.unwrap();
    assert_eq!(got, OpResult::Gain(want));
    assert_eq!(sources(&node).await[0].gain, Some(want));
    assert_eq!(on_disk(dir.path())["gain"], serde_json::json!({"db": 40.0}));
    assert_eq!(on_disk(dir.path())["pin"], serde_json::Value::Null);
    let wire = serde_json::to_value(&got).unwrap();
    assert_eq!(
        wire,
        serde_json::json!({"result": "gain", "value": {"db": 40.0, "min_db": 0.0,
            "max_db": 76.0, "automatic": false, "automatic_available": true}})
    );
}

#[tokio::test]
async fn null_is_automatic_where_available_and_an_error_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), Amp::new(true, false)).await;
    match node.apply(set_gain("radio0", None)).await.unwrap() {
        OpResult::Gain(g) => {
            assert_eq!(g.db, None);
            assert!(g.automatic);
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(on_disk(dir.path())["gain"], serde_json::json!({"db": null}));

    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), Amp::new(false, false)).await;
    let m = other(node.apply(set_gain("radio0", None)).await);
    assert_eq!(m, "source radio0 has no automatic gain");
    assert!(
        !source_file(dir.path()).exists(),
        "a refused gain is not persisted"
    );
}

#[tokio::test]
async fn out_of_range_and_unsupported_are_other_with_the_text() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), Amp::new(true, false)).await;
    let m = other(node.apply(set_gain("radio0", Some(90.0))).await);
    assert_eq!(m, "gain_db 90 out of range 0..=76 dB for source radio0");

    let dir = tempfile::tempdir().unwrap();
    let meta = fixtures::write_tone(dir.path());
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let file: Arc<dyn Port> =
        Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let node = open(dir.path(), file).await;
    let m = other(node.apply(set_gain("file0", Some(10.0))).await);
    assert_eq!(m, "source has no adjustable gain");
    assert_eq!(sources(&node).await[0].gain, None);
    match node.apply(set_gain("nope", Some(10.0))).await {
        Err(OpError::Unknown { field, .. }) => assert_eq!(field, "source"),
        r => panic!("{r:?}"),
    }
}

/// A `Running` passthrough record on `radio0`.
fn running() -> JobRecord {
    JobRecord {
        format: JOB_RECORD_FORMAT,
        id: JobId::new(),
        kind: "passthrough".into(),
        graph: Graph {
            id: GraphId::new(),
            nodes: vec![GraphNode {
                id: NodeId("p".into()),
                kind: "passthrough".into(),
                params: serde_json::json!({"kind": "iq"}),
            }],
            edges: vec![],
        },
        source: SourceId("radio0".into()),
        tuning: Tuning {
            center_hz: Hz(100_000_000),
            bandwidth_hz: Hz(48_000),
        },
        state: JobState::Running,
        context: serde_json::json!({}),
        created_ns: Timestamp(1),
        input: None,
        capture_rate_hz: None,
        elastic: false,
        operator_graph: false,
        owner: None,
    }
}

#[tokio::test]
async fn a_persisted_gain_is_reapplied_at_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let node = open(dir.path(), Amp::new(true, false)).await;
        node.apply(set_gain("radio0", Some(40.0))).await.unwrap();
    }
    let rec = running();
    let jobs = dir.path().join("state").join("jobs");
    std::fs::write(
        jobs.join(format!("{}.json", rec.id.0)),
        serde_json::to_vec(&rec).unwrap(),
    )
    .unwrap();
    let amp = Amp::new(true, false);
    let node = open(dir.path(), amp.clone()).await;
    assert!(node.load_errors().is_empty(), "{:?}", node.load_errors());
    let log = amp.log();
    assert_eq!(
        log.first().map(String::as_str),
        Some("set_gain(Some(40.0))")
    );
    assert_eq!(
        log.iter().filter(|l| l.starts_with("set_gain")).count(),
        1,
        "{log:?}"
    );
    assert!(
        log.contains(&"tune".to_string()),
        "the job resumed: {log:?}"
    );
    assert_eq!(sources(&node).await[0].gain.unwrap().db, Some(40.0));
    drop(node);

    let before = std::fs::read(source_file(dir.path())).unwrap();
    let node = open(dir.path(), Amp::new(true, true)).await;
    assert_eq!(
        node.load_errors(),
        ["source radio0: gain: io: amplifier on fire"]
    );
    assert_eq!(std::fs::read(source_file(dir.path())).unwrap(), before);
}
