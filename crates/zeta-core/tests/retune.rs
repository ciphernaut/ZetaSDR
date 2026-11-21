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
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
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
        owns_state_dir: true,
        bank_factory: None,
        modes_dir: None,
        handlers_dir: None,
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
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy: None,
        dwell_scale: None,
        elastic,
        replace: vec![],
        handler: None,
        needs: vec![],
        beside: None,
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
        bin_hz: None,
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
            stream: None,
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

/// A job a controller started (its record's `owner`, as a watch's identify run and its
/// trial child carry) is neither retuned nor stopped by the caller while the owner runs:
/// both are refused naming the owner, the job left running. Once the owner has stopped,
/// a record it left behind can still be stopped. The owner is written into the record
/// on disk between two opens, as no op sets it.
#[tokio::test]
async fn a_job_a_controller_owns_is_neither_retuned_nor_stopped_while_its_owner_runs() {
    let dir = tempfile::tempdir().unwrap();
    let band = t(100_000_000, 12_500);
    let (owner, child) = {
        let (node, _radio) = open(dir.path()).await;
        let owner = start(&node, JobKind::Spectrum, t(100_000_000, 1_000_000)).await;
        let child = start(&node, JobKind::FmDemod, band).await;
        (owner, child)
    };
    // The first node's supervisors see its shutdown and leave the records `running`.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let path = dir.path().join(format!("state/jobs/{}.json", child.0));
    let mut rec: JobRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    rec.owner = Some(owner.clone());
    std::fs::write(&path, serde_json::to_vec(&rec).unwrap()).unwrap();

    let (node, _radio) = open(dir.path()).await;
    assert_eq!(record(&node, &child).state, JobState::Running);
    assert_eq!(
        retune(&node, &child, 100_050_000, 12_500, &[]).await,
        Err(other(format!(
            "job {} is run by spectrum {}: stop that job, or start a new one",
            child.0, owner.0
        )))
    );
    assert_eq!(record(&node, &child).tuning, band);
    assert_eq!(
        node.apply(Op::JobStop { job: child.clone() }).await,
        Err(other(format!(
            "job {} is run by spectrum {}: stop that job",
            child.0, owner.0
        )))
    );
    assert_eq!(record(&node, &child).state, JobState::Running);
    stop(&node, &owner).await;
    stop(&node, &child).await;
    assert!(
        node.job_list()
            .iter()
            .all(|r| r.id != child || r.state != JobState::Running)
    );
}

/// Rewrite `id`'s record on disk under `dir` with `owner` and `state`, between opens.
fn own_on_disk(dir: &Path, id: &JobId, owner: &JobId, state: JobState) {
    let path = dir.join(format!("state/jobs/{}.json", id.0));
    let mut rec: JobRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    rec.owner = Some(owner.clone());
    rec.state = state;
    std::fs::write(&path, serde_json::to_vec(&rec).unwrap()).unwrap();
}

/// A job a controller started cannot be stopped through another request's `replace`,
/// nor its graph replaced by `graph_put`, while its owner runs: both are refused naming
/// the owner, the job left as it was. Once the owner has ended a retune is refused
/// without pointing at the owner.
#[tokio::test]
async fn replace_and_graph_put_of_a_job_a_controller_owns_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let band = t(100_000_000, 12_500);
    let (owner, child) = {
        let (node, _radio) = open(dir.path()).await;
        let owner = start(&node, JobKind::Spectrum, t(100_000_000, 1_000_000)).await;
        let child = start(&node, JobKind::FmDemod, band).await;
        (owner, child)
    };
    // The first node's supervisors see its shutdown and leave the records `running`.
    tokio::time::sleep(Duration::from_millis(300)).await;
    own_on_disk(dir.path(), &child, &owner, JobState::Running);

    let (node, _radio) = open(dir.path()).await;
    let by = format!("job {} is run by spectrum {}", child.0, owner.0);
    let op = Op::JobStart {
        kind: JobKind::FmDemod,
        source: SourceId(RADIO.into()),
        center_hz: Hz(100_300_000),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy: None,
        dwell_scale: None,
        elastic: false,
        replace: vec![child.clone()],
        handler: None,
        needs: vec![],
        beside: None,
    };
    assert_eq!(
        node.apply(op).await,
        Err(other(format!("{by}: stop that job")))
    );
    assert_eq!(record(&node, &child).state, JobState::Running);
    let graph = record(&node, &child).graph;
    // Its own graph again: a restart under its id, were it anyone's job.
    assert_eq!(
        node.apply(Op::GraphPut {
            job: child.clone(),
            graph: graph.clone(),
        })
        .await,
        Err(other(format!("{by}: stop that job, or start a new one")))
    );
    assert_eq!(record(&node, &child).graph, graph);
    stop(&node, &owner).await;
    assert_eq!(
        retune(&node, &child, 100_050_000, 12_500, &[]).await,
        Err(other(format!(
            "job {} was started by spectrum {}, which is not running: stop it, or start a new job",
            child.0, owner.0
        )))
    );
    stop(&node, &child).await;
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

// Plan 43: a Send to group retunes as one lease move, its members relaunched in turn.

mod common;
use common::handlers::{self as h, ALT};
use std::sync::atomic::AtomicBool;

/// What [`Refusing`] says of a node it refuses.
const REFUSED: &str = "alt will not start again";

/// Stands in for `external_decode` as [`h::External`] does, but refuses a node of the
/// handler `refuse` names once `armed`: the graph check of a launch (a relaunch's) fails.
struct Refusing {
    inner: Arc<dyn Stage>,
    refuse: &'static str,
    armed: Arc<AtomicBool>,
}

#[async_trait]
impl Stage for Refusing {
    fn spec(&self) -> &StageSpec {
        self.inner.spec()
    }
    fn backend(&self) -> Backend {
        self.inner.backend()
    }
    fn outputs_for(&self, params: &serde_json::Value) -> Vec<StreamKind> {
        self.inner.outputs_for(params)
    }
    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let handler = params.get("handler").and_then(|h| h.as_str());
        match self.armed.load(Ordering::SeqCst) && handler == Some(self.refuse) {
            true => Err(REFUSED.into()),
            false => Ok(()),
        }
    }
    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        self.inner
            .run(params, inputs, outputs, bus, cancel, ctx)
            .await
    }
}

/// A node on [`h::Radio`] with `adsb_decode`'s handlers `dump1090` (the row's own) and
/// `alt` (packets beside its events when `packets`), and the switch that makes alt's
/// next launch fail.
async fn group_node(dir: &Path, packets: bool) -> (Node, Arc<h::Radio>, Arc<AtomicBool>) {
    let mut stages = StageRegistry::new();
    zeta_dsp::register_all(&mut stages);
    stages.register(Arc::new(StreamInStage::new()));
    stages.register(Arc::new(
        zeta_core::packet_capture::PacketCaptureStage::new(),
    ));
    let armed = Arc::new(AtomicBool::new(false));
    stages.register(Arc::new(Refusing {
        inner: h::External::new(packets.then_some(ALT)),
        refuse: ALT,
        armed: armed.clone(),
    }));
    let radio = h::Radio::new();
    let node = Node::open(
        h::cfg(dir, packets),
        stages,
        vec![radio.clone() as Arc<dyn Port>],
    )
    .await
    .unwrap();
    (node, radio, armed)
}

/// The ADS-B band the pair starts on and the band outside its capture they move to.
const HERE: Tuning = Tuning {
    center_hz: Hz(1_090_000_000),
    bandwidth_hz: Hz(2_400_000),
};
const THERE: Tuning = Tuning {
    center_hz: Hz(1_093_000_000),
    bandwidth_hz: Hz(2_400_000),
};

/// The row's own decoder and a sibling on alt beside it, both running.
async fn pair(node: &Node) -> (JobId, JobId) {
    let first = h::start(node, None, &[]).await;
    let sib = match h::start_beside(node, &first, Some(ALT), &[]).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("the sibling: {r:?}"),
    };
    running_at(node, &[&first, &sib], HERE).await;
    (first, sib)
}

/// Wait until every job of `ids` is `Running` at `band`.
async fn running_at(node: &Node, ids: &[&JobId], band: Tuning) {
    common::until("the jobs run at the band", Duration::from_secs(10), || {
        ids.iter().all(|id| {
            let r = record(node, id);
            r.state == JobState::Running && r.tuning == band
        })
    })
    .await;
}

async fn retune_to(
    node: &Node,
    id: &JobId,
    band: Tuning,
    replace: &[&JobId],
) -> Result<OpResult, OpError> {
    retune(node, id, band.center_hz.0, band.bandwidth_hz.0, replace).await
}

#[tokio::test]
async fn retuning_one_member_moves_the_whole_group() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, _) = group_node(dir.path(), false).await;
    let (first, sib) = pair(&node).await;
    let tunes = radio.tunes();
    assert_eq!(
        retune_to(&node, &sib, THERE, &[]).await,
        Ok(OpResult::Job(sib.clone()))
    );
    running_at(&node, &[&first, &sib], THERE).await;
    assert_eq!(radio.tunes(), tunes + 1, "one tune for the group");
    let s = source(&node).await;
    assert_eq!(s.current, Some(THERE));
    let mut holders = s.holders;
    holders.sort();
    let mut both = vec![first.clone(), sib.clone()];
    both.sort();
    assert_eq!(holders, both);
    for id in [&first, &sib] {
        assert_eq!(record(&node, id).group, Some(first.clone()), "{id:?}");
    }
    assert_eq!(record(&node, &sib).handler.as_deref(), Some(ALT));
    assert_eq!(record(&node, &first).handler.as_deref(), Some(h::OWN));
}

/// The common case: the group moves to a band inside the capture it is on. Both
/// members follow under their ids and the radio is not tuned.
#[tokio::test]
async fn a_group_retuned_inside_its_capture_does_not_tune_the_radio() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, _) = group_node(dir.path(), false).await;
    // A 6 MHz capture holding HERE and, 500 kHz up, NEARBY.
    let capture = Tuning {
        center_hz: Hz(1_090_000_000),
        bandwidth_hz: Hz(6_000_000),
    };
    const NEARBY: Tuning = Tuning {
        center_hz: Hz(1_090_500_000),
        bandwidth_hz: Hz(2_400_000),
    };
    pin(&node, capture).await;
    let (first, sib) = pair(&node).await;
    let tunes = radio.tunes();
    assert_eq!(
        retune_to(&node, &first, NEARBY, &[]).await,
        Ok(OpResult::Job(first.clone()))
    );
    running_at(&node, &[&first, &sib], NEARBY).await;
    assert_eq!(radio.tunes(), tunes, "the group retune tuned nothing");
    assert_eq!(source(&node).await.current, Some(capture));
    for id in [&first, &sib] {
        assert_eq!(record(&node, id).group, Some(first.clone()), "{id:?}");
    }
    assert_eq!(record(&node, &sib).handler.as_deref(), Some(ALT));
}

/// The named member stopped while its group's retune waited for the gate (a stop takes
/// none): the retune says it is stopped, as the op's own check would, and the sibling
/// runs on where it was.
#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn a_member_stopped_before_the_gate_is_named_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio, _) = group_node(dir.path(), false).await;
    let node = Arc::new(node);
    let (first, sib) = pair(&node).await;
    let gate = node.debug_gate(&SourceId(RADIO.into())).await;
    let retuning = {
        let (node, first) = (node.clone(), first.clone());
        tokio::spawn(async move { retune_to(&node, &first, THERE, &[]).await })
    };
    // The retune is past its own check and waits on the gate.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!retuning.is_finished(), "the retune waits for the gate");
    // The stop marks the record under the jobs lock alone; its supervisor's ending may
    // wait on the gate, so the stop returns only once the gate is let go.
    let stopping = {
        let (node, first) = (node.clone(), first.clone());
        tokio::spawn(async move { stop(&node, &first).await })
    };
    common::until("the member is stopped", Duration::from_secs(10), || {
        record(&node, &first).state == JobState::Stopped
    })
    .await;
    drop(gate);
    tokio::time::timeout(Duration::from_secs(10), stopping)
        .await
        .expect("the stop ends")
        .unwrap();
    match retuning.await.unwrap() {
        Err(OpError::Other { message }) => {
            assert_eq!(message, format!("job {} is stopped", first.0))
        }
        r => panic!("expected the member named stopped, got {r:?}"),
    }
    running_at(&node, &[&sib], HERE).await;
}

#[tokio::test]
async fn a_group_retune_conflicts_only_with_holders_outside_it() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, _) = group_node(dir.path(), false).await;
    let (first, sib) = pair(&node).await;
    let fm = match node
        .apply(h::start_kind_op(
            JobKind::FmDemod,
            1_089_000_000,
            12_500,
            None,
            &[],
        ))
        .await
    {
        Ok(OpResult::Job(id)) => id,
        r => panic!("fm_demod: {r:?}"),
    };
    let graphs = (record(&node, &first).graph.id, record(&node, &sib).graph.id);
    let tunes = radio.tunes();
    match retune_to(&node, &sib, THERE, &[]).await {
        Err(OpError::Conflict { holders, .. }) => assert_eq!(holders, vec![fm.clone()]),
        r => panic!("expected a conflict, got {r:?}"),
    }
    for id in [&first, &sib] {
        let r = record(&node, id);
        assert_eq!((r.state, r.tuning), (JobState::Running, HERE), "{id:?}");
    }
    assert_eq!(
        (record(&node, &first).graph.id, record(&node, &sib).graph.id),
        graphs,
        "nothing relaunched"
    );
    assert_eq!(radio.tunes(), tunes, "the conflict tuned nothing");
    assert_eq!(source(&node).await.holders.len(), 3);
    // With the holder outside replaced, the group moves.
    assert_eq!(
        retune_to(&node, &sib, THERE, &[&fm]).await,
        Ok(OpResult::Job(sib.clone()))
    );
    running_at(&node, &[&first, &sib], THERE).await;
    assert_eq!(record(&node, &fm).state, JobState::Stopped);
}

#[tokio::test]
async fn a_member_that_fails_its_relaunch_leaves_the_others_standing() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio, armed) = group_node(dir.path(), false).await;
    let (first, sib) = pair(&node).await;
    armed.store(true, Ordering::SeqCst);
    assert_eq!(
        retune_to(&node, &first, THERE, &[]).await,
        Ok(OpResult::Job(first.clone()))
    );
    running_at(&node, &[&first], THERE).await;
    match record(&node, &sib).state {
        JobState::Failed { reason } => assert!(reason.contains(REFUSED), "{reason}"),
        s => panic!("the sibling should have failed: {s:?}"),
    }
    assert_eq!(source(&node).await.holders, vec![first]);
}

/// A member that cannot be settled (its named handler gone) refuses the whole retune
/// naming it, before any lease moves.
#[tokio::test]
async fn a_member_whose_handler_is_gone_refuses_the_whole_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, _) = group_node(dir.path(), false).await;
    let (first, sib) = pair(&node).await;
    h::alt_goes_missing(&node);
    let tunes = radio.tunes();
    assert_eq!(
        retune_to(&node, &first, THERE, &[]).await,
        Err(other(format!(
            "job {}, a member of the retuned job's group: handler {ALT} is unavailable: {}",
            sib.0,
            h::ALT_MISSING
        )))
    );
    for id in [&first, &sib] {
        let r = record(&node, id);
        assert_eq!((r.state, r.tuning), (JobState::Running, HERE), "{id:?}");
    }
    assert_eq!(radio.tunes(), tunes, "nothing moved");
}

#[tokio::test]
async fn a_packet_capture_follows_its_member_through_a_group_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio, _) = group_node(dir.path(), true).await;
    let (first, sib) = pair(&node).await;
    let cap = match node
        .apply(
            serde_json::from_value(serde_json::json!({
                "op": "job_attach", "kind": "packets", "job": sib.0,
                "language": null, "stream": null
            }))
            .unwrap(),
        )
        .await
    {
        Ok(OpResult::Job(id)) => id,
        r => panic!("attach packets: {r:?}"),
    };
    assert_eq!(
        retune_to(&node, &first, THERE, &[]).await,
        Ok(OpResult::Job(first.clone()))
    );
    running_at(&node, &[&first, &sib, &cap], THERE).await;
    let rec = record(&node, &cap);
    assert_eq!(rec.input, Some(sib.clone()));
    assert_eq!(rec.tuning, record(&node, &sib).tuning);
}

#[tokio::test]
async fn replace_may_not_name_a_member_of_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, _) = group_node(dir.path(), false).await;
    let (first, sib) = pair(&node).await;
    let tunes = radio.tunes();
    assert_eq!(
        retune_to(&node, &first, THERE, &[&sib]).await,
        Err(other(format!(
            "replace names job {}, a member of the retuned job's group",
            sib.0
        )))
    );
    for id in [&first, &sib] {
        let r = record(&node, id);
        assert_eq!((r.state, r.tuning), (JobState::Running, HERE), "{id:?}");
    }
    assert_eq!(radio.tunes(), tunes);
}

/// A member whose graph an operator put (`graph_put`) would lose it to the group's
/// re-expansion: the retune is refused naming it, before anything moves.
#[tokio::test]
async fn a_member_running_an_operator_graph_refuses_the_group_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio, _) = group_node(dir.path(), false).await;
    let (first, sib) = pair(&node).await;
    let mut graph = match node.apply(Op::GraphGet { job: sib.clone() }).await {
        Ok(OpResult::Graph(g)) => g,
        r => panic!("graph_get: {r:?}"),
    };
    let decode = graph
        .nodes
        .iter_mut()
        .find(|n| n.kind == "external_decode")
        .unwrap();
    decode.params["operator"] = true.into();
    node.apply(Op::GraphPut {
        job: sib.clone(),
        graph,
    })
    .await
    .unwrap();
    running_at(&node, &[&sib], HERE).await;
    assert!(record(&node, &sib).operator_graph);
    let tunes = radio.tunes();
    assert_eq!(
        retune_to(&node, &first, THERE, &[]).await,
        Err(other(format!(
            "job {}, a member of the retuned job's group, runs an operator graph \
             (graph_put): stop it, or put a new graph",
            sib.0
        )))
    );
    for id in [&first, &sib] {
        let r = record(&node, id);
        assert_eq!((r.state, r.tuning), (JobState::Running, HERE), "{id:?}");
    }
    assert_eq!(radio.tunes(), tunes);
}
