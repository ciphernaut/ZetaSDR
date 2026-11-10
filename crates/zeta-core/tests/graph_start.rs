//! `graph_start`: a job from a graph whose root takes no input (`capture_in` here), run
//! on the node's placeholder source with no radio and no lease. Its launch, its
//! `graph_put` relaunch, its resume and `job_retune` all go by the record's source.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::broadcast;

use zeta_core::bursts::NewCapture;
use zeta_core::graph::Graph;
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, StreamKind};

const RATE_HZ: u32 = 48_000;
const CENTER_HZ: u64 = 433_920_000;
const BAND_HZ: u64 = 20_000;

/// A radio that is never fed: only here so `source_list` has a source to show unheld.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
}

impl Radio {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: SourceId("rx".into()),
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

async fn open(dir: &Path) -> Node {
    let cfg = NodeConfig {
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
    };
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut registry);
    Node::open(cfg, registry, vec![Radio::new()]).await.unwrap()
}

/// 200 ms of 2-FSK at 4800 Bd, written to the node's burst store; its id.
fn write_fsk(node: &Node) -> String {
    write_fsk_at(node, CENTER_HZ)
}

/// [`write_fsk`] centred on `center_hz`.
fn write_fsk_at(node: &Node, center_hz: u64) -> String {
    let symbols: Vec<u8> = (0..960u32).map(|i| ((i * 7 + i / 3) % 2) as u8).collect();
    let iq = zeta_dsp::synth::gfsk_modulate(&symbols, 2, 4_800, 2_400.0, RATE_HZ);
    let start_ns = 1_760_000_000_000_000_000;
    let rec = node
        .bursts()
        .write(
            NewCapture {
                source: SourceId("rx".into()),
                set: JobId("set".into()),
                center_hz: Hz(center_hz),
                bandwidth_hz: Hz(BAND_HZ),
                sample_rate_hz: Hz(RATE_HZ as u64),
                start_ns,
                end_ns: start_ns + 200_000_000,
                features: json!({}),
                signal: None,
                job: JobId("cap".into()),
                watch: None,
                allocations: json!([]),
            },
            &iq,
        )
        .unwrap();
    rec.id
}

fn graph(v: serde_json::Value) -> Graph {
    serde_json::from_value(v).unwrap()
}

/// `capture_in {params} -> passthrough {kind: iq}`.
fn replay(params: serde_json::Value) -> Graph {
    graph(json!({
        "id": "g",
        "nodes": [
            {"id": "src", "kind": "capture_in", "params": params},
            {"id": "pass", "kind": "passthrough", "params": {"kind": "iq"}},
        ],
        "edges": [{"from": "src", "to": "pass", "kind": "iq"}],
    }))
}

async fn start(node: &Node, g: Graph) -> Result<JobId, OpError> {
    match node.apply(Op::GraphStart { graph: g }).await? {
        OpResult::Job(id) => Ok(id),
        r => panic!("graph_start: {r:?}"),
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list().into_iter().find(|r| &r.id == id).unwrap()
}

/// The record once it leaves `Running`, within `within`.
async fn ended(node: &Node, id: &JobId, within: Duration) -> JobRecord {
    let t0 = Instant::now();
    loop {
        let rec = record(node, id);
        if rec.state != JobState::Running || t0.elapsed() > within {
            return rec;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn other_message(e: OpError) -> String {
    match e {
        OpError::Other { message } => message,
        e => panic!("expected other, got {e:?}"),
    }
}

#[tokio::test]
async fn graph_start_runs_a_capture_in_chain_to_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node);
    let id = start(
        &node,
        replay(json!({"captures": [cap], "loop": false, "realtime": false})),
    )
    .await
    .unwrap();
    let rec = record(&node, &id);
    assert_eq!(rec.kind, "graph");
    assert_eq!(rec.source, SourceId("node".into()));
    assert_eq!(rec.tuning.center_hz, Hz(CENTER_HZ));
    assert_eq!(rec.tuning.bandwidth_hz, Hz(BAND_HZ));
    assert!(rec.operator_graph);
    let kinds: Vec<StreamKind> = node.job_outputs(&id).into_iter().map(|(k, _)| k).collect();
    assert!(kinds.contains(&StreamKind::Iq), "{kinds:?}");
    assert!(kinds.contains(&StreamKind::Event), "{kinds:?}");
    let rec = ended(&node, &id, Duration::from_secs(3)).await;
    assert_eq!(rec.state, JobState::Stopped);
}

#[tokio::test]
async fn graph_start_refuses_an_unfed_input() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let g = graph(json!({
        "id": "g",
        "nodes": [{"id": "rs", "kind": "resample", "params": {"out_rate_hz": 38400}}],
        "edges": [],
    }));
    let m = other_message(start(&node, g).await.unwrap_err());
    assert!(m.contains("node rs (resample)"), "{m}");
    assert!(m.contains("unfed iq input"), "{m}");
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn graph_start_refuses_a_root_that_takes_input() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let g = graph(json!({
        "id": "g",
        "nodes": [
            {"id": "a", "kind": "passthrough", "params": {"kind": "iq"}},
            {"id": "b", "kind": "passthrough", "params": {"kind": "iq"}},
        ],
        "edges": [{"from": "a", "to": "b", "kind": "iq"}],
    }));
    let m = other_message(start(&node, g).await.unwrap_err());
    assert!(m.contains("node a (passthrough)"), "{m}");
    assert!(
        m.contains("takes input; the first node must take none"),
        "{m}"
    );
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn graph_start_refuses_an_unknown_capture() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node);
    let g = replay(json!({"captures": [cap, "01NOPE"], "realtime": false}));
    match start(&node, g).await.unwrap_err() {
        OpError::Unknown {
            field,
            value,
            known,
        } => {
            assert_eq!(field, "captures[1]");
            assert_eq!(value, "01NOPE");
            assert_eq!(known, [cap]);
        }
        e => panic!("expected unknown, got {e:?}"),
    }
    let g = replay(json!({"captures": ["01NOPE"], "realtime": false}));
    match start(&node, g).await.unwrap_err() {
        OpError::Unknown { field, .. } => assert_eq!(field, "captures[0]"),
        e => panic!("expected unknown, got {e:?}"),
    }
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn job_start_refuses_the_graph_and_bits_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    for name in ["graph", "bits"] {
        let e = node
            .apply(Op::JobStart {
                kind: JobKind::from_name(name).unwrap(),
                source: SourceId("rx".into()),
                center_hz: Hz(CENTER_HZ),
                bandwidth_hz: Hz(BAND_HZ),
                deviation_hz: None,
                sideband: None,
                elastic: false,
                replace: vec![],
            })
            .await
            .unwrap_err();
        let m = other_message(e);
        assert!(
            m.contains(&format!(
                "{name} takes no band: use graph_start / bits_start"
            )),
            "{m}"
        );
    }
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn graph_put_on_a_graph_job_relaunches_without_a_radio() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node);
    let id = start(
        &node,
        replay(json!({"captures": [cap], "loop": true, "realtime": true})),
    )
    .await
    .unwrap();
    let put = replay(json!({
        "captures": [cap], "loop": true, "realtime": true, "loop_pause_ms": 100,
    }));
    let r = node
        .apply(Op::GraphPut {
            job: id.clone(),
            graph: put,
        })
        .await
        .unwrap();
    assert!(matches!(r, OpResult::Ok), "{r:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.source, SourceId("node".into()));
    assert_eq!(rec.graph.nodes[0].params["loop_pause_ms"], 100);
    assert!(!node.job_outputs(&id).is_empty());
    let OpResult::Sources(sources) = node.apply(Op::SourceList {}).await.unwrap() else {
        panic!("sources")
    };
    assert!(!sources.is_empty());
    for s in &sources {
        assert!(s.holders.is_empty(), "{s:?}");
        assert!(s.current.is_none(), "{s:?}");
    }
    node.job_stop(&id).await.unwrap();
}

#[tokio::test]
async fn a_graph_job_resumed_on_an_expired_capture_fails_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let (id, cap) = {
        let node = open(dir.path()).await;
        let cap = write_fsk(&node);
        let id = start(
            &node,
            replay(json!({"captures": [cap], "loop": true, "realtime": true})),
        )
        .await
        .unwrap();
        // The running stage holds the samples; the store forgets the capture.
        node.bursts().delete(&cap).unwrap();
        assert_eq!(record(&node, &id).state, JobState::Running);
        (id, cap)
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let node = open(dir.path()).await;
    let rec = ended(&node, &id, Duration::from_secs(3)).await;
    match rec.state {
        JobState::Failed { reason } => {
            assert!(
                reason.contains(&format!("capture {cap} not found")),
                "{reason}"
            )
        }
        s => panic!("resumed as {s:?}"),
    }
}

#[tokio::test]
async fn job_retune_refuses_a_graph_job() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node);
    let id = start(
        &node,
        replay(json!({"captures": [cap], "loop": true, "realtime": true})),
    )
    .await
    .unwrap();
    let e = node
        .apply(Op::JobRetune {
            job: id.clone(),
            center_hz: Hz(CENTER_HZ),
            bandwidth_hz: Hz(BAND_HZ),
            replace: vec![],
        })
        .await
        .unwrap_err();
    let m = other_message(e);
    assert!(m.contains("graph has no band: it runs on no radio"), "{m}");
    assert_eq!(record(&node, &id).state, JobState::Running);
    node.job_stop(&id).await.unwrap();
}

#[tokio::test]
async fn jobs_stop_with_a_plan_skips_a_graph_job_on_a_plan_channel() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    // UHF CB ch1: the graph job's centre is the capture's, on the plan's channel.
    let cap = write_fsk_at(&node, 476_425_000);
    let id = start(
        &node,
        replay(json!({"captures": [cap], "loop": true, "realtime": true})),
    )
    .await
    .unwrap();
    assert_eq!(record(&node, &id).tuning.center_hz, Hz(476_425_000));
    let r = node
        .apply(Op::JobsStop {
            source: None,
            kind: None,
            plan: Some("uhf_cb_au".into()),
        })
        .await
        .unwrap();
    match r {
        OpResult::JobsStopped { stopped, failed } => {
            assert!(stopped.is_empty(), "{stopped:?}");
            assert!(failed.is_empty(), "{failed:?}");
        }
        r => panic!("jobs_stop: {r:?}"),
    }
    assert_eq!(record(&node, &id).state, JobState::Running);
    node.job_stop(&id).await.unwrap();
}
