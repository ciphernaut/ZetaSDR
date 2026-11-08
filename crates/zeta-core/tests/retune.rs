//! `job_retune`: a running job moved to a new band under the same id. Its lease moves
//! under the source's gate (inside the capture untuned, outside it a tune when nobody
//! else holds the source, else a conflict that leaves the job as it was), and the job
//! is relaunched on its graph expanded again for the new band.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::graph::{Edge, Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, SourceInfo};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{
    Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, StreamId, StreamKind,
};

/// A radio that tunes to exactly the band asked for, counting its tunes.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
    tunes: AtomicUsize,
}

impl Radio {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            id: SourceId(id.into()),
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
            tunes: AtomicUsize::new(0),
        })
    }

    fn tunes(&self) -> usize {
        self.tunes.load(Ordering::SeqCst)
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
        self.tunes.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2)).await;
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

/// Stands in for the `transcribe` stage, which lives in another crate: it waits for
/// its input to close or the job to stop.
struct Transcribe(StageSpec);

#[async_trait]
impl Stage for Transcribe {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: serde_json::Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        _outputs: Vec<StreamId>,
        _bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = inputs[0].recv() => match r {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r.register(Arc::new(StreamInStage::new()));
    r.register(Arc::new(Transcribe(StageSpec {
        kind: "transcribe",
        inputs: vec![StreamKind::Audio],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({"type": "object"}),
    })));
    // Stands in for the program rows' stage (zeta-ext's): `adsb_decode` starts on it.
    r.register(Arc::new(Transcribe(StageSpec {
        kind: "external_decode",
        inputs: vec![StreamKind::Iq, StreamKind::Audio],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({"type": "object"}),
    })));
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

const RADIO: &str = "radio0";

fn t(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

async fn open(dir: &Path) -> (Node, Arc<Radio>) {
    let radio = Radio::new(RADIO);
    let node = Node::open(cfg(dir), registry(), vec![radio.clone() as Arc<dyn Port>])
        .await
        .unwrap();
    (node, radio)
}

async fn start_as(node: &Node, kind: JobKind, band: Tuning, elastic: bool) -> JobId {
    let op = Op::JobStart {
        kind,
        source: SourceId(RADIO.into()),
        center_hz: band.center_hz,
        bandwidth_hz: band.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
        elastic,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    }
}

async fn start(node: &Node, kind: JobKind, band: Tuning) -> JobId {
    start_as(node, kind, band, false).await
}

async fn retune(
    node: &Node,
    id: &JobId,
    center_hz: u64,
    bandwidth_hz: u64,
    replace: &[&JobId],
) -> Result<OpResult, OpError> {
    let op = Op::JobRetune {
        job: id.clone(),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
        replace: replace.iter().map(|j| (*j).clone()).collect(),
    };
    tokio::time::timeout(Duration::from_secs(10), node.apply(op))
        .await
        .expect("no deadlock")
}

async fn pin(node: &Node, band: Tuning) {
    node.apply(Op::SourcePin {
        source: SourceId(RADIO.into()),
        center_hz: band.center_hz,
        bandwidth_hz: band.bandwidth_hz,
        replace: vec![],
    })
    .await
    .unwrap();
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

async fn source(node: &Node) -> SourceInfo {
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v.into_iter().find(|s| s.id.0 == RADIO).expect("listed"),
        r => panic!("source_list: {r:?}"),
    }
}

/// The params of the first node of `stage_kind` in `rec`'s graph.
fn node_params(rec: &JobRecord, stage_kind: &str) -> serde_json::Value {
    rec.graph
        .nodes
        .iter()
        .find(|n| n.kind == stage_kind)
        .unwrap_or_else(|| panic!("no {stage_kind} node in {:?}", rec.graph))
        .params
        .clone()
}

fn other(message: impl Into<String>) -> OpError {
    OpError::Other {
        message: message.into(),
    }
}

async fn stop(node: &Node, id: &JobId) {
    node.apply(Op::JobStop { job: id.clone() }).await.unwrap();
}

#[tokio::test]
async fn a_retune_inside_the_capture_keeps_the_id_and_does_not_tune() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let capture = t(476_918_750, 1_024_000);
    pin(&node, capture).await;
    let id = start(&node, JobKind::FmDemod, t(476_900_000, 12_500)).await;
    assert_eq!(
        retune(&node, &id, 476_905_000, 10_000, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    let rec = record(&node, &id);
    assert_eq!(rec.tuning, t(476_905_000, 10_000));
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.capture_rate_hz, Some(1_024_000));
    assert!(!rec.operator_graph);
    let channel = node_params(&rec, "channel");
    assert_eq!(channel["center_hz"], 476_905_000);
    assert_eq!(channel["bandwidth_hz"], 10_000);
    let s = source(&node).await;
    assert_eq!(s.current, Some(capture));
    assert_eq!(s.holders, vec![id]);
    assert_eq!(radio.tunes(), 1, "the pin's tune only");
}

#[tokio::test]
async fn a_lone_job_retuned_inside_its_capture_does_not_tune_the_port() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let capture = t(100_000_000, 1_000_000);
    let id = start(&node, JobKind::Passthrough, capture).await;
    // A start on an unheld radio tunes it, then its lease tunes it again.
    let tunes = radio.tunes();
    assert_eq!(
        retune(&node, &id, 100_200_000, 10_000, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    assert_eq!(source(&node).await.current, Some(capture));
    assert_eq!(record(&node, &id).tuning, t(100_200_000, 10_000));
    assert_eq!(radio.tunes(), tunes, "the retune tuned nothing");
}

#[tokio::test]
async fn a_retune_outside_the_capture_tunes_when_nobody_else_holds() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let el = start_as(&node, JobKind::Spectrum, t(100_000_000, 1_000_000), true).await;
    let id = start(&node, JobKind::Passthrough, t(100_100_000, 10_000)).await;
    let far = t(433_000_000, 12_500);
    assert_eq!(
        retune(&node, &id, 433_000_000, 12_500, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    assert_eq!(source(&node).await.current, Some(far));
    assert_eq!(record(&node, &id).tuning, far);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let rec = record(&node, &el);
        if rec.tuning == far && rec.state == JobState::Running {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the elastic job did not follow: {rec:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_retune_outside_the_capture_with_another_holder_is_a_conflict_and_leaves_the_job_as_it_was()
 {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let capture = t(100_000_000, 1_000_000);
    let a = start(&node, JobKind::Passthrough, capture).await;
    let b = start(&node, JobKind::Passthrough, t(100_100_000, 10_000)).await;
    let before = record(&node, &b);
    let tunes = radio.tunes();
    match retune(&node, &b, 433_000_000, 12_500, &[]).await {
        Err(OpError::Conflict {
            source,
            holders,
            current,
            options,
        }) => {
            assert_eq!(source, SourceId(RADIO.into()));
            assert_eq!(holders, vec![a.clone()]);
            assert_eq!(current, capture);
            assert_eq!(
                options.last().map(String::as_str),
                Some(format!("retry with replace: [{}]", a.0).as_str())
            );
        }
        r => panic!("expected a conflict, got {r:?}"),
    }
    let rec = record(&node, &b);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, t(100_100_000, 10_000));
    assert_eq!(rec.graph.id, before.graph.id);
    assert_eq!(source(&node).await.holders, vec![a, b]);
    assert_eq!(radio.tunes(), tunes, "the conflict tuned nothing");
}

#[tokio::test]
async fn replace_names_the_holder_and_the_retune_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let a = start(&node, JobKind::Passthrough, t(100_000_000, 1_000_000)).await;
    let b = start(&node, JobKind::Passthrough, t(100_100_000, 10_000)).await;
    let far = t(433_000_000, 12_500);
    assert_eq!(
        retune(&node, &b, 433_000_000, 12_500, &[&a]).await,
        Ok(OpResult::Job(b.clone()))
    );
    assert_eq!(record(&node, &a).state, JobState::Stopped);
    assert_eq!(record(&node, &b).tuning, far);
    let s = source(&node).await;
    assert_eq!(s.holders, vec![b]);
    assert_eq!(s.current, Some(far));
}

#[tokio::test]
async fn a_retune_onto_a_band_another_rigid_job_holds_inside_the_capture_is_granted() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let capture = t(476_918_750, 1_024_000);
    pin(&node, capture).await;
    let a = start(&node, JobKind::FmDemod, t(476_900_000, 12_500)).await;
    let b = start(&node, JobKind::FmDemod, t(476_925_000, 12_500)).await;
    assert_eq!(
        retune(&node, &b, 476_900_000, 12_500, &[]).await,
        Ok(OpResult::Job(b.clone()))
    );
    assert_eq!(record(&node, &a).state, JobState::Running);
    assert_eq!(record(&node, &b).state, JobState::Running);
    let s = source(&node).await;
    assert_eq!(s.holders, vec![a, b]);
    assert_eq!(s.current, Some(capture));
}

#[tokio::test]
async fn graph_put_squelch_db_is_carried_through_a_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let id = start(&node, JobKind::FmDemod, t(100_000_000, 12_500)).await;
    let mut graph = match node.apply(Op::GraphGet { job: id.clone() }).await {
        Ok(OpResult::Graph(g)) => g,
        r => panic!("graph_get: {r:?}"),
    };
    let demod = graph
        .nodes
        .iter_mut()
        .find(|n| n.kind == "nbfm_demod")
        .unwrap();
    demod.params["squelch_db"] = serde_json::json!(-70);
    node.apply(Op::GraphPut {
        job: id.clone(),
        graph,
    })
    .await
    .unwrap();
    assert!(!record(&node, &id).operator_graph, "only a carried param");
    assert_eq!(
        retune(&node, &id, 100_002_000, 12_500, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    let rec = record(&node, &id);
    let demod = node_params(&rec, "nbfm_demod");
    assert_eq!(demod["squelch_db"], -70);
    assert_eq!(demod["squelch"], true);
    assert!(!rec.operator_graph);
    assert!(rec.capture_rate_hz.is_some());
}

/// An `fm_demod` on ch20 (476 900 000 Hz) in the 476 918 750 / 1 024 000 Hz pin, its graph
/// put back with the guard off and `squelch_db` -65 (carried params only).
async fn ch20_with_operator_squelch(node: &Node) -> JobId {
    pin(node, t(476_918_750, 1_024_000)).await;
    let id = start(node, JobKind::FmDemod, t(476_900_000, 12_500)).await;
    let mut graph = match node.apply(Op::GraphGet { job: id.clone() }).await {
        Ok(OpResult::Graph(g)) => g,
        r => panic!("graph_get: {r:?}"),
    };
    let demod = graph
        .nodes
        .iter_mut()
        .find(|n| n.kind == "nbfm_demod")
        .unwrap();
    demod.params["guard_hz"] = serde_json::Value::Null;
    demod.params["squelch_db"] = serde_json::json!(-65);
    node.apply(Op::GraphPut {
        job: id.clone(),
        graph,
    })
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn a_retune_onto_a_neighbouring_channel_rederives_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let id = ch20_with_operator_squelch(&node).await;
    assert_eq!(
        retune(&node, &id, 476_887_500, 12_500, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    let rec = record(&node, &id);
    let demod = node_params(&rec, "nbfm_demod");
    assert_eq!(demod["guard_hz"], 12_500, "{demod}");
    assert_eq!(demod["guard_db"], 12);
    assert_eq!(demod["channel_hz"], 12_500);
    assert_eq!(demod["squelch_db"], -65);
    let channel = node_params(&rec, "channel");
    assert_eq!(channel["center_hz"], 476_887_500);
    assert_eq!(channel["wide_hz"], 37_500);
}

#[tokio::test]
async fn a_width_preset_retune_changes_the_channel_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    pin(&node, t(476_918_750, 1_024_000)).await;
    let id = start(&node, JobKind::FmDemod, t(476_900_000, 12_500)).await;
    assert_eq!(
        retune(&node, &id, 476_900_000, 25_000, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    let rec = record(&node, &id);
    let channel = node_params(&rec, "channel");
    assert_eq!(channel["bandwidth_hz"], 25_000);
    assert_eq!(channel["wide_hz"], 50_000);
    assert_eq!(node_params(&rec, "nbfm_demod")["channel_hz"], 25_000);
}

#[tokio::test]
async fn a_retune_off_plan_drops_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let id = ch20_with_operator_squelch(&node).await;
    assert_eq!(
        retune(&node, &id, 476_905_000, 12_500, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    let rec = record(&node, &id);
    let demod = node_params(&rec, "nbfm_demod");
    for k in ["guard_hz", "guard_db", "channel_hz"] {
        assert!(demod.get(k).is_none(), "{k}: {demod}");
    }
    assert!(node_params(&rec, "channel").get("wide_hz").is_none());
}

#[tokio::test]
async fn refusals_name_the_kind_the_state_and_the_id() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let narrow = t(100_000_000, 12_500);

    for (kind, band, elastic) in [
        (JobKind::Scan, t(100_000_000, 40_000_000), false),
        (JobKind::Watch, t(403_025_000, 5_950_000), false),
        (JobKind::Identify, narrow, false),
        (JobKind::Spectrum, t(100_000_000, 1_000_000), true),
        (JobKind::Spectrum, t(100_000_000, 1_000_000), false),
    ] {
        let name = kind.name().to_string();
        let id = start_as(&node, kind, band, elastic).await;
        assert_eq!(
            retune(&node, &id, 100_000_000, 12_500, &[]).await,
            Err(other(format!("{name} is not retunable"))),
            "{name} elastic={elastic}"
        );
        stop(&node, &id).await;
    }

    // An attached job: retune its input instead.
    let fm = start(&node, JobKind::FmDemod, narrow).await;
    let tr = match node
        .apply(Op::JobAttach {
            kind: JobKind::Transcribe,
            job: fm.clone(),
            language: None,
        })
        .await
    {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_attach: {r:?}"),
    };
    assert_eq!(
        retune(&node, &tr, 100_000_000, 12_500, &[]).await,
        Err(other(format!(
            "transcribe is attached to job {}: retune that job",
            fm.0
        )))
    );

    // Replacing itself is refused, the job left running.
    assert_eq!(
        retune(&node, &fm, 100_000_000, 12_500, &[&fm]).await,
        Err(other(format!("job {} cannot replace itself", fm.0)))
    );
    assert_eq!(record(&node, &fm).state, JobState::Running);

    // A topology change by graph_put: the operator's graph.
    let mut graph = record(&node, &fm).graph;
    let demod = graph
        .nodes
        .iter()
        .find(|n| n.kind == "nbfm_demod")
        .unwrap()
        .id
        .clone();
    let tap = NodeId::new();
    graph.nodes.push(GraphNode {
        id: tap.clone(),
        kind: "passthrough".into(),
        params: serde_json::json!({"kind": "audio"}),
    });
    graph.edges.push(Edge {
        from: demod,
        to: tap,
        kind: StreamKind::Audio,
    });
    node.apply(Op::GraphPut {
        job: fm.clone(),
        graph,
    })
    .await
    .unwrap();
    assert_eq!(
        retune(&node, &fm, 100_000_000, 12_500, &[]).await,
        Err(other(format!(
            "job {} runs an operator graph (graph_put): put a new graph or start a new job",
            fm.0
        )))
    );
    stop(&node, &fm).await;

    // A stopped job.
    let stopped = start(&node, JobKind::FmDemod, narrow).await;
    stop(&node, &stopped).await;
    assert_eq!(
        retune(&node, &stopped, 100_000_000, 12_500, &[]).await,
        Err(other(format!("job {} is stopped", stopped.0)))
    );

    // An unknown id.
    match retune(&node, &JobId::new(), 100_000_000, 12_500, &[]).await {
        Err(OpError::Unknown { field, .. }) => assert_eq!(field, "job"),
        r => panic!("expected unknown, got {r:?}"),
    }

    // A caller's own kind, whose graph the node does not expand.
    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId::new(),
            kind: "passthrough".into(),
            params: serde_json::json!({"kind": "iq"}),
        }],
        edges: vec![],
    };
    let monitor = node
        .job_start(
            "monitor",
            SourceId(RADIO.into()),
            narrow,
            graph,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        retune(&node, &monitor, 100_000_000, 12_500, &[]).await,
        Err(other(
            "monitor is not a kind the node expands: start a new job"
        ))
    );
}

#[tokio::test]
async fn an_out_of_range_band_is_refused_before_anything_moves() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let id = start(&node, JobKind::FmDemod, t(100_000_000, 12_500)).await;
    let before = record(&node, &id);
    let tunes = radio.tunes();
    match retune(&node, &id, 7_000_000_000, 12_500, &[]).await {
        Err(OpError::OutOfRange { field, .. }) => assert_eq!(field, "center_hz"),
        r => panic!("expected out_of_range, got {r:?}"),
    }
    match retune(&node, &id, 100_000_000, 30_000_000, &[]).await {
        Err(OpError::OutOfRange { field, max, .. }) => {
            assert_eq!(field, "bandwidth_hz");
            assert_eq!(max, 20_000_000);
        }
        r => panic!("expected out_of_range, got {r:?}"),
    }
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, before.tuning);
    assert_eq!(rec.graph, before.graph);
    assert_eq!(radio.tunes(), tunes, "nothing moved");
}

/// A program row's `min_hz` (`adsb_decode`: 2 MHz) is checked before the gate, as
/// `job_start` checks it: a narrower band outside the capture is refused without the
/// lease moving or the radio tuning.
#[tokio::test]
async fn a_band_under_the_rows_min_hz_is_refused_before_anything_moves() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let id = start(&node, JobKind::AdsbDecode, t(1_090_000_000, 2_400_000)).await;
    let before = record(&node, &id);
    let tunes = radio.tunes();
    match retune(&node, &id, 1_000_000_000, 1_000_000, &[]).await {
        Err(OpError::OutOfRange {
            field, value, min, ..
        }) => {
            assert_eq!(field, "bandwidth_hz");
            assert_eq!((value, min), (1_000_000, 2_000_000));
        }
        r => panic!("expected out_of_range, got {r:?}"),
    }
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, before.tuning);
    assert_eq!(rec.graph, before.graph);
    assert_eq!(radio.tunes(), tunes, "nothing moved");
    assert_eq!(
        source(&node).await.current,
        Some(t(1_090_000_000, 2_400_000))
    );
}

#[tokio::test]
async fn a_retune_to_the_same_band_restarts_the_job_and_keeps_its_tuning() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let band = t(100_000_000, 12_500);
    let id = start(&node, JobKind::FmDemod, band).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let n = node.debug_stage_count();
    assert_eq!(
        retune(&node, &id, 100_000_000, 12_500, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    let rec = record(&node, &id);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, band);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(node.debug_stage_count(), n);
}

/// The next frame on the `ops` stream, with its event payload.
async fn next(rx: &mut broadcast::Receiver<Frame>) -> (Frame, serde_json::Value) {
    let f = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("ops frame within 5 s")
        .expect("ops stream open");
    let Payload::Event(v) = f.payload.clone() else {
        panic!("ops frame is not an Event: {:?}", f.payload);
    };
    (f, v)
}

#[tokio::test]
async fn the_ops_stream_logs_job_retune_with_its_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let mut rx = node.bus().subscribe(&node.ops_stream()).unwrap();
    let id = start(&node, JobKind::FmDemod, t(100_000_000, 12_500)).await;
    let _ = next(&mut rx).await;
    assert_eq!(
        retune(&node, &id, 100_002_000, 10_000, &[]).await,
        Ok(OpResult::Job(id.clone()))
    );
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["op"]["op"], "job_retune");
    assert_eq!(p["op"]["job"], id.0.as_str());
    assert_eq!(p["outcome"], "ok");
    assert_eq!(p["result"]["result"], "job");
    assert_eq!(p["result"]["value"], id.0.as_str());
    assert_eq!(p["transport"], "local");
    retune(&node, &JobId::new(), 100_000_000, 12_500, &[])
        .await
        .unwrap_err();
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["op"]["op"], "job_retune");
    assert_eq!(p["outcome"], "error");
    assert_eq!(p["error"]["error"], "unknown");
}
