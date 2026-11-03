//! `bits_start`: a capture set's feature record becomes the
//! `capture_in → resample → demod → clock → slicer → bit_view` chain, run on the node's
//! placeholder source as `graph_start` runs a graph.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::bits::Modulation;
use zeta_core::bursts::NewCapture;
use zeta_core::graph::Graph;
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{DRAIN_TIMEOUT_MS, Node, NodeConfig};
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{Baud, Hz, JobId, SourceId, StreamId, StreamKind};

const RATE_HZ: u32 = 48_000;
const CENTER_HZ: u64 = 433_920_000;
const BAND_HZ: u64 = 20_000;
const BAUD: u32 = 4_800;
/// One window: 200 ms of symbols, four 50 ms blocks of `capture_in`.
const SYMBOLS: usize = 960;
const T0_NS: u64 = 1_760_000_000_000_000_000;

async fn open(dir: &Path) -> Node {
    open_with(dir, None).await
}

async fn open_with(dir: &Path, extra: Option<Arc<dyn Stage>>) -> Node {
    let cfg = NodeConfig {
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
    if let Some(stage) = extra {
        registry.register(stage);
    }
    let node = Node::open(cfg, registry, vec![]).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes()) {
        node.add_classifier(c);
    }
    node
}

/// A layer-1 record of a 2-FSK window at 4800 Bd; `symbol_rate_baud` as given.
fn features(rate: Option<u32>) -> Value {
    json!({
        "bandwidth_hz": 9_600, "center_hz": CENTER_HZ, "symbol_rate_baud": rate,
        "snr_db": 30.0, "burst": false, "duty": 1.0, "bursts": 1, "burst_ms": 200.0,
        "period_ms": null, "order": 2, "tone_spacing_hz": 4_800,
    })
}

/// 200 ms of 2-FSK at 4800 Bd in set `set`, starting `start_ns`, captured at `rate_hz`;
/// its id.
fn write_fsk(node: &Node, set: &str, start_ns: u64, rate_hz: u32, features: Value) -> String {
    let symbols: Vec<u8> = (0..SYMBOLS as u32)
        .map(|i| ((i * 7 + i / 3) % 2) as u8)
        .collect();
    let iq = zeta_dsp::synth::gfsk_modulate(&symbols, 2, BAUD, 2_400.0, rate_hz);
    node.bursts()
        .write(
            NewCapture {
                source: SourceId("rx".into()),
                set: JobId(set.into()),
                center_hz: Hz(CENTER_HZ),
                bandwidth_hz: Hz(BAND_HZ),
                sample_rate_hz: Hz(rate_hz as u64),
                start_ns,
                end_ns: start_ns + 200_000_000,
                features,
                signal: None,
                job: JobId(set.into()),
                watch: None,
                allocations: json!([]),
            },
            &iq,
        )
        .unwrap()
        .id
}

async fn bits_start(
    node: &Node,
    capture: &str,
    modulation: Option<Modulation>,
    symbol_rate_baud: Option<Baud>,
) -> Result<JobId, OpError> {
    match node
        .apply(Op::BitsStart {
            capture: capture.into(),
            modulation,
            symbol_rate_baud,
            order: None,
        })
        .await?
    {
        OpResult::Job(id) => Ok(id),
        r => panic!("bits_start: {r:?}"),
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list().into_iter().find(|r| &r.id == id).unwrap()
}

/// The job's context once `pick` finds something in it, within `within`.
async fn context_until(
    node: &Node,
    id: &JobId,
    within: Duration,
    pick: impl Fn(&Value) -> Option<Value>,
) -> Option<Value> {
    let t0 = Instant::now();
    loop {
        if let Some(v) = node.job_context(id).as_ref().and_then(&pick) {
            return Some(v);
        }
        if t0.elapsed() > within {
            return None;
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
async fn bits_start_builds_the_chain_and_streams_bits() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let first = write_fsk(&node, "set", T0_NS, RATE_HZ, features(Some(BAUD)));
    let second = write_fsk(
        &node,
        "set",
        T0_NS + 1_000_000_000,
        RATE_HZ,
        features(Some(BAUD)),
    );
    // Another set's window is not replayed.
    write_fsk(
        &node,
        "other",
        T0_NS + 500_000_000,
        RATE_HZ,
        features(Some(BAUD)),
    );
    let id = bits_start(&node, &second, Some(Modulation::Fsk), None)
        .await
        .unwrap();
    let rec = record(&node, &id);
    assert_eq!(rec.kind, "bits");
    assert_eq!(rec.source, SourceId("node".into()));
    assert_eq!(rec.tuning.center_hz, Hz(CENTER_HZ));
    assert!(rec.operator_graph);
    let bits = &rec.context["bits"];
    assert_eq!(bits["captures"], json!([first, second]));
    assert_eq!(bits["modulation"], "fsk");
    assert_eq!(bits["symbol_rate_baud"], BAUD);
    assert_eq!(bits["order"], 2);
    assert_eq!(bits["rate_hz"], 38_400);
    let stream = context_until(&node, &id, Duration::from_secs(5), |c| {
        c["slicer"]["stream"].as_str().map(Value::from)
    })
    .await
    .expect("slicer.stream within 5 s");
    let r = node
        .apply(Op::StreamTail {
            stream: StreamId(stream.as_str().unwrap().into()),
            max_frames: 2,
            timeout_ms: 5_000,
        })
        .await
        .unwrap();
    let OpResult::Frames { frames, .. } = r else {
        panic!("frames: {r:?}")
    };
    let counted = frames
        .iter()
        .any(|f| f.payload["protocol"] == "bits" && f.payload["count"].as_u64() > Some(0));
    assert!(counted, "{frames:?}");
    assert!(
        context_until(&node, &id, Duration::from_secs(10), |c| {
            c.get("bit_view").cloned()
        })
        .await
        .is_some(),
        "bit_view in the context within 10 s"
    );
    // The plan stays in the context beside the stages' notes.
    assert_eq!(node.job_context(&id).unwrap()["bits"]["captures"][0], first);
    node.job_stop(&id).await.unwrap();
}

#[tokio::test]
async fn bits_start_infers_the_modulation_from_layer_1() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node, "set", T0_NS, RATE_HZ, features(Some(BAUD)));
    let id = bits_start(&node, &cap, None, None).await.unwrap();
    assert_eq!(record(&node, &id).context["bits"]["modulation"], "fsk");
    node.job_stop(&id).await.unwrap();
}

#[tokio::test]
async fn bits_start_refuses_without_a_rate() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node, "set", T0_NS, RATE_HZ, features(None));
    let e = bits_start(&node, &cap, Some(Modulation::Fsk), None)
        .await
        .unwrap_err();
    assert_eq!(
        other_message(e),
        "no symbol rate measured; give symbol_rate_baud"
    );
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn bits_start_refuses_a_rate_the_capture_cannot_hold() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node, "set", T0_NS, RATE_HZ, features(Some(BAUD)));
    let e = bits_start(&node, &cap, Some(Modulation::Fsk), Some(Baud(20_000)))
        .await
        .unwrap_err();
    assert_eq!(
        other_message(e),
        "symbol_rate_baud 20000 needs 80000 Hz; the capture holds 48000 Hz"
    );
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn bits_start_refuses_a_set_of_mixed_rates() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let first = write_fsk(&node, "set", T0_NS, RATE_HZ, features(Some(BAUD)));
    let second = write_fsk(
        &node,
        "set",
        T0_NS + 1_000_000_000,
        96_000,
        features(Some(BAUD)),
    );
    let e = bits_start(&node, &first, Some(Modulation::Fsk), None)
        .await
        .unwrap_err();
    assert_eq!(
        other_message(e),
        format!("capture {second}: 96000 Hz differs from {first}'s 48000 Hz")
    );
    assert!(node.job_list().is_empty());
}

#[tokio::test]
async fn bits_start_refuses_an_unknown_capture() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node, "set", T0_NS, RATE_HZ, features(Some(BAUD)));
    match bits_start(&node, "01NOPE", Some(Modulation::Fsk), None)
        .await
        .unwrap_err()
    {
        OpError::Unknown {
            field,
            value,
            known,
        } => {
            assert_eq!(field, "capture");
            assert_eq!(value, "01NOPE");
            assert_eq!(known, [cap]);
        }
        e => panic!("expected unknown, got {e:?}"),
    }
    assert!(node.job_list().is_empty());
}

/// When `capture_in` ends a pass that does not loop, the job drains: its streams close
/// one stage after another, the slicer flushes its short chunk and `bit_view` publishes
/// what it holds, then the job ends `stopped`. Stream ids are fresh on every launch, so
/// the ended record's context is read rather than a stream subscribed before the put.
#[tokio::test]
async fn a_one_pass_chain_ends_stopped_with_its_last_chunk_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node, "one", T0_NS, RATE_HZ, features(Some(BAUD)));
    let id = bits_start(&node, &cap, Some(Modulation::Fsk), None)
        .await
        .unwrap();
    let mut graph = record(&node, &id).graph;
    let src = graph
        .nodes
        .iter_mut()
        .find(|n| n.id.0 == "capture_in")
        .unwrap();
    src.params["loop"] = json!(false);
    src.params["realtime"] = json!(false);
    let r = node
        .apply(Op::GraphPut {
            job: id.clone(),
            graph,
        })
        .await
        .unwrap();
    assert!(matches!(r, OpResult::Ok), "{r:?}");
    let rec = ended(&node, &id, Duration::from_secs(5)).await;
    assert_eq!(rec.state, JobState::Stopped, "{:?}", rec.state);
    let slicer = &rec.context["slicer"];
    let sliced = slicer["symbols"]
        .as_u64()
        .unwrap_or_else(|| panic!("{}", rec.context));
    assert!(
        sliced.abs_diff(SYMBOLS as u64) <= 2,
        "{sliced} of {SYMBOLS} symbols"
    );
    assert!(slicer["events"].as_u64() >= Some(1), "{slicer}");
    // `bit_view`'s window holds the whole pass: it was handed every bit sliced.
    assert_eq!(
        rec.context["bit_view"]["window_bits"], sliced,
        "{}",
        rec.context
    );
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

/// A stage that never reads its input, so never sees it close: it ends only on cancel.
struct Deaf {
    spec: StageSpec,
}

#[async_trait]
impl Stage for Deaf {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: Value,
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

#[tokio::test]
async fn a_drain_a_stage_ignores_is_cut_after_the_drain_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let deaf = Deaf {
        spec: StageSpec {
            kind: "deaf",
            inputs: vec![StreamKind::Iq],
            outputs: vec![StreamKind::Event],
            params_schema: schemars::json_schema!({"type": "object"}),
        },
    };
    let node = open_with(dir.path(), Some(Arc::new(deaf))).await;
    let cap = write_fsk(&node, "one", T0_NS, RATE_HZ, features(Some(BAUD)));
    let graph: Graph = serde_json::from_value(json!({
        "id": "g",
        "nodes": [
            {"id": "src", "kind": "capture_in", "params": {
                "captures": [cap], "loop": false, "realtime": false, "loop_pause_ms": 0,
            }},
            {"id": "deaf", "kind": "deaf", "params": {}},
        ],
        "edges": [{"from": "src", "to": "deaf", "kind": "iq"}],
    }))
    .unwrap();
    let t0 = Instant::now();
    let OpResult::Job(id) = node.apply(Op::GraphStart { graph }).await.unwrap() else {
        panic!("graph_start")
    };
    let rec = ended(&node, &id, Duration::from_secs(5)).await;
    let took = t0.elapsed();
    assert_eq!(rec.state, JobState::Stopped, "{:?}", rec.state);
    let drain = Duration::from_millis(DRAIN_TIMEOUT_MS);
    assert!(
        took >= drain,
        "ended after {took:?}, before the drain timeout"
    );
    assert!(
        took < drain + Duration::from_millis(1_000),
        "ended after {took:?}"
    );
}
