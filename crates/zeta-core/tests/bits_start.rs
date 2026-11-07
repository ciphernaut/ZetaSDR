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
/// The sync word [`planted`] repeats.
const SYNC: u16 = 0x2DD4;
/// Bits of the alternating preamble [`planted`] opens with.
const PREAMBLE_BITS: usize = 64;
/// Bits of one planted frame: the sync and a 48-bit payload.
const FRAME_BITS: usize = 64;

/// [`SYMBOLS`] bits: a [`PREAMBLE_BITS`] alternating preamble for the clock, then frames
/// of [`SYNC`] and a pseudo-random 48-bit payload, back to back.
fn planted() -> Vec<u8> {
    let mut out: Vec<u8> = (0..PREAMBLE_BITS).map(|i| (i % 2) as u8).collect();
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    while out.len() < SYMBOLS {
        out.extend((0..16).rev().map(|b| ((SYNC >> b) & 1) as u8));
        for _ in 0..FRAME_BITS - 16 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            out.push((s >> 40) as u8 & 1);
        }
    }
    out.truncate(SYMBOLS);
    out
}

/// How many frames [`planted`] holds.
const FRAMES: usize = (SYMBOLS - PREAMBLE_BITS) / FRAME_BITS;

/// [`planted`] as hex, MSB first, for the synthesiser's `bits`.
fn planted_hex() -> String {
    planted()
        .chunks(4)
        .map(|c| format!("{:x}", c.iter().fold(0u8, |a, &b| a << 1 | b)))
        .collect()
}

async fn open(dir: &Path) -> Node {
    open_with(dir, None).await
}

async fn open_with(dir: &Path, extra: Option<Arc<dyn Stage>>) -> Node {
    let cfg = NodeConfig {
        source_positions: Default::default(),
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

/// 200 ms of 2-FSK at 4800 Bd carrying [`planted`] in set `set`, starting `start_ns`,
/// captured at `rate_hz`; its id.
fn write_fsk(node: &Node, set: &str, start_ns: u64, rate_hz: u32, features: Value) -> String {
    let iq = zeta_dsp::synth::gfsk_modulate(&planted(), 2, BAUD, 2_400.0, rate_hz);
    write_iq(node, set, start_ns, rate_hz, features, &iq)
}

/// [`planted`] at 4800 Bd by the synthesiser's `family` (`psk` with `order` 2, `ook`),
/// at [`RATE_HZ`].
fn synth_planted(family: &str) -> Vec<num_complex::Complex<f32>> {
    let mut spec = json!({
        "family": family, "symbol_rate_baud": BAUD, "bits": planted_hex(), "repeat": 1,
    });
    if family == "psk" {
        spec["order"] = json!(2);
    }
    let spec: zeta_dsp::synth::SynthSpec = serde_json::from_value(spec).unwrap();
    spec.validate().unwrap();
    zeta_dsp::synth::synthesise(&spec, &Default::default(), RATE_HZ, CENTER_HZ)
}

/// `iq`, 200 ms at `rate_hz` from `start_ns`, written to set `set`; its id.
fn write_iq(
    node: &Node,
    set: &str,
    start_ns: u64,
    rate_hz: u32,
    features: Value,
    iq: &[num_complex::Complex<f32>],
) -> String {
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
            iq,
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
    bits_start_order(node, capture, modulation, symbol_rate_baud, None).await
}

async fn bits_start_order(
    node: &Node,
    capture: &str,
    modulation: Option<Modulation>,
    symbol_rate_baud: Option<Baud>,
    order: Option<u8>,
) -> Result<JobId, OpError> {
    match node
        .apply(Op::BitsStart {
            capture: capture.into(),
            modulation,
            symbol_rate_baud,
            order,
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

/// Put `id`'s graph back with `capture_in` making one unpaced pass, and wait for the
/// job to drain and end.
async fn one_pass(node: &Node, id: &JobId) -> JobRecord {
    let mut graph = record(node, id).graph;
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
    let rec = ended(node, id, Duration::from_secs(5)).await;
    assert_eq!(rec.state, JobState::Stopped, "{:?}", rec.state);
    rec
}

/// The sync candidates of the ended job's `bit_view`, as `(word, count)`.
fn sync_candidates(rec: &JobRecord) -> Vec<(String, u64)> {
    rec.context["bit_view"]["sync_candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("{}", rec.context))
        .iter()
        .map(|c| {
            (
                c["word"].as_str().unwrap().to_string(),
                c["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// Whether the planted sync (or, with `either_polarity`, its complement) is a candidate
/// seen on all but at most one of the [`FRAMES`] frames.
fn finds_the_sync(rec: &JobRecord, either_polarity: bool) -> bool {
    let words = [format!("{SYNC:04x}"), format!("{:04x}", !SYNC)];
    let words = if either_polarity {
        &words[..]
    } else {
        &words[..1]
    };
    sync_candidates(rec)
        .iter()
        .any(|(w, n)| words.contains(w) && *n as usize + 1 >= FRAMES)
}

/// When `capture_in` ends a pass that does not loop, the job drains: its streams close
/// one stage after another, the slicer flushes its short chunk and `bit_view` publishes
/// what it holds, then the job ends `stopped`. Stream ids are fresh on every launch, so
/// the ended record's context is read rather than a stream subscribed before the put.
/// The planted bits come out: the sync word is `bit_view`'s candidate.
#[tokio::test]
async fn a_one_pass_chain_ends_stopped_with_its_last_chunk_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let cap = write_fsk(&node, "one", T0_NS, RATE_HZ, features(Some(BAUD)));
    let id = bits_start(&node, &cap, Some(Modulation::Fsk), None)
        .await
        .unwrap();
    let rec = one_pass(&node, &id).await;
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
    assert!(finds_the_sync(&rec, false), "{}", rec.context["bit_view"]);
}

/// BPSK built by `bits_start` with the rate given: the planted sync is found (in either
/// polarity: a BPSK carrier loop locks at 0 or 180 degrees).
#[tokio::test]
async fn a_one_pass_bpsk_chain_finds_the_planted_sync() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let iq = synth_planted("psk");
    let cap = write_iq(&node, "psk", T0_NS, RATE_HZ, features(None), &iq);
    let id = bits_start_order(
        &node,
        &cap,
        Some(Modulation::Psk),
        Some(Baud(BAUD)),
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(record(&node, &id).context["bits"]["modulation"], "psk");
    let rec = one_pass(&node, &id).await;
    assert!(finds_the_sync(&rec, true), "{}", rec.context["bit_view"]);
}

/// OOK built by `bits_start`: the planted sync is found.
#[tokio::test]
async fn a_one_pass_ook_chain_finds_the_planted_sync() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let iq = synth_planted("ook");
    let cap = write_iq(&node, "ook", T0_NS, RATE_HZ, features(Some(BAUD)), &iq);
    let id = bits_start(&node, &cap, Some(Modulation::Ook), None)
        .await
        .unwrap();
    assert_eq!(record(&node, &id).context["bits"]["modulation"], "ook");
    let rec = one_pass(&node, &id).await;
    assert!(finds_the_sync(&rec, false), "{}", rec.context["bit_view"]);
}

/// The wide capture of the off-centre test: 2.048 MHz centred on 433.4 MHz.
const WIDE_RATE_HZ: u32 = 2_048_000;
const WIDE_CENTER_HZ: u64 = 433_400_000;
/// Where the signal sits from the wide capture's centre (the operator's remote).
const WIDE_OFFSET_HZ: f64 = 482_000.0;
const WIDE_BAUD: u32 = 2_400;
/// [`planted`] at [`WIDE_BAUD`], in ns.
const WIDE_SPAN_NS: u64 = SYMBOLS as u64 * 1_000_000_000 / WIDE_BAUD as u64;

/// OOK whose signal sits [`WIDE_OFFSET_HZ`] off a [`WIDE_RATE_HZ`] capture's centre, its
/// features recording the measured centre: `bits_start` shifts to it with a `channel`
/// stage (the resampler alone would cut it away) and the planted sync is found.
#[tokio::test]
async fn an_off_centre_ook_capture_is_shifted_to_its_measured_centre() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let spec = json!({
        "family": "ook", "symbol_rate_baud": WIDE_BAUD, "bits": planted_hex(), "repeat": 1,
    });
    let spec: zeta_dsp::synth::SynthSpec = serde_json::from_value(spec).unwrap();
    let imp = zeta_dsp::synth::Impairments {
        offset_hz: WIDE_OFFSET_HZ,
        ..Default::default()
    };
    let iq = zeta_dsp::synth::synthesise(&spec, &imp, WIDE_RATE_HZ, WIDE_CENTER_HZ);
    let measured = WIDE_CENTER_HZ + WIDE_OFFSET_HZ as u64;
    let features = json!({
        "bandwidth_hz": 2 * WIDE_BAUD, "center_hz": measured, "symbol_rate_baud": WIDE_BAUD,
        "snr_db": 30.0, "burst": false, "duty": 0.5, "bursts": 1, "burst_ms": 400.0,
        "period_ms": null, "order": null, "tone_spacing_hz": null,
    });
    let cap = node
        .bursts()
        .write(
            NewCapture {
                source: SourceId("rx".into()),
                set: JobId("wide".into()),
                center_hz: Hz(WIDE_CENTER_HZ),
                bandwidth_hz: Hz(WIDE_RATE_HZ as u64),
                sample_rate_hz: Hz(WIDE_RATE_HZ as u64),
                start_ns: T0_NS,
                end_ns: T0_NS + WIDE_SPAN_NS,
                features,
                signal: None,
                job: JobId("wide".into()),
                watch: None,
                allocations: json!([]),
            },
            &iq,
        )
        .unwrap()
        .id;
    let id = bits_start(&node, &cap, Some(Modulation::Ook), None)
        .await
        .unwrap();
    let rec = record(&node, &id);
    let bits = &rec.context["bits"];
    assert_eq!(bits["center_hz"], measured);
    assert_eq!(bits["shift_hz"], WIDE_OFFSET_HZ as i64);
    assert!(
        rec.graph
            .nodes
            .iter()
            .any(|n| n.kind == "channel" && n.params["center_hz"] == measured),
        "{:?}",
        rec.graph
    );
    let rec = one_pass(&node, &id).await;
    assert!(finds_the_sync(&rec, false), "{}", rec.context["bit_view"]);
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

/// The EV1527 remotes' chip rate (313 us a chip).
const REMOTE_BAUD: f64 = 3_200.0;
/// How far the inferred rate may stray from [`REMOTE_BAUD`], as a fraction.
const REMOTE_BAUD_TOLERANCE: f64 = 0.02;

/// Whether `zeta-dsp`'s fixture window `name` is in this checkout: the real remote's
/// captures are the operator's and are not published (the reason is printed when one
/// is absent); the synthesised `ook-ev1527-synth-*` always are.
fn have_fixture(name: &str) -> bool {
    let there = fixture_meta(name).exists();
    if !there {
        eprintln!("skipped: {name}: a private capture, not in this checkout");
    }
    there
}

fn fixture_meta(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../zeta-dsp/tests/data")
        .join(format!("{name}.sigmf-meta"))
}

/// The `zeta-dsp` fixture window `name` written to the store in set `set` from
/// `start_ns`, with the features the node's own burst capture measures (`measure_band`
/// over the capture's band); its id.
fn write_fixture(node: &Node, name: &str, set: &str, start_ns: u64) -> String {
    let meta = fixture_meta(name);
    let (m, iq) = zeta_core::capture::read_cf32(&meta).unwrap();
    let a = &m.annotations[0];
    let edge = |k: &str| a[k].as_u64().unwrap();
    let band = edge("core:freq_upper_edge") - edge("core:freq_lower_edge");
    let rate = m.sample_rate_hz.0;
    let features = zeta_dsp::measure::measure_band(&iq, rate, m.center_hz.0, band);
    node.bursts()
        .write(
            NewCapture {
                source: SourceId("rx".into()),
                set: JobId(set.into()),
                center_hz: m.center_hz,
                bandwidth_hz: Hz(band),
                sample_rate_hz: m.sample_rate_hz,
                start_ns,
                end_ns: start_ns + iq.len() as u64 * 1_000_000_000 / rate,
                features: serde_json::to_value(&features).unwrap(),
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

/// `bits_start` with nothing given on capture `cap`: its plan (`context.bits`), the job
/// stopped, asserted OOK within [`REMOTE_BAUD_TOLERANCE`] of [`REMOTE_BAUD`].
async fn infers_the_remote(node: &Node, cap: &str, name: &str) -> Value {
    let id = bits_start(node, cap, None, None)
        .await
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    let bits = record(node, &id).context["bits"].clone();
    node.job_stop(&id).await.unwrap();
    assert_eq!(bits["modulation"], "ook", "{name}: {bits}");
    let baud = bits["symbol_rate_baud"].as_f64().unwrap();
    assert!(
        (baud - REMOTE_BAUD).abs() <= REMOTE_BAUD * REMOTE_BAUD_TOLERANCE,
        "{name}: {bits}"
    );
    bits
}

/// The real remote's windows from `zeta-dsp`'s fixtures (B210 at 51 200 and 61 440 Hz,
/// a HackRF at 62 500 Hz, all at 433.882 MHz; private, skipped when absent) and the
/// public synthesised ones (433.92 MHz at 51 200 and 62 500 Hz), each written to the
/// store with the features the node's own burst capture measures (`measure_band` over
/// the capture's band): `bits_start` with nothing given picks OOK at the chip rate, not
/// FSK (the HackRF window read `fsk` at 1598 Bd, the bit rate's harmonic; the 433882k
/// ones `fsk`).
#[tokio::test]
async fn bits_start_infers_ook_at_the_chip_rate_on_a_remote() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    for name in [
        "ook-remote-433882k-1",
        "ook-remote-433882k-2",
        "ook-remote-b210-1",
        "ook-remote-b210-2",
        "ook-remote-hackrf-1",
        "ook-ev1527-synth-1",
        "ook-ev1527-synth-2",
    ] {
        if !have_fixture(name) {
            assert!(
                !name.starts_with("ook-ev1527-synth-"),
                "{name}: a public fixture"
            );
            continue;
        }
        let cap = write_fixture(&node, name, name, T0_NS);
        infers_the_remote(&node, &cap, name).await;
    }
}

/// The three HackRF windows of one press of a weak remote (431, 71 and 471 ms at 62 500
/// Hz, 20-34 dB SNR; private, skipped when absent), written as one set as the node's
/// burst capture wrote them: a bare `bits_start` on each picks OOK at the chip rate
/// (live it refused, `no symbol rate measured`, or read `fsk`), and the 471 ms window,
/// whose measured band (56 580 Hz, mostly noise) padded is wider than the capture, takes
/// a channel that fits instead of a refusal.
#[tokio::test]
async fn bits_start_infers_ook_on_every_window_of_a_weak_remote_set() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let names = [
        "ook-remote-hackrf-sparse-1",
        "ook-remote-hackrf-sparse-2",
        "ook-remote-hackrf-sparse-3",
    ];
    if !names.iter().all(|n| have_fixture(n)) {
        return;
    }
    let caps: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(i, name)| write_fixture(&node, name, "press", T0_NS + i as u64 * 1_000_000_000))
        .collect();
    for (name, cap) in names.iter().zip(&caps) {
        let bits = infers_the_remote(&node, cap, name).await;
        assert_eq!(
            bits["captures"].as_array().unwrap().len(),
            3,
            "{name}: {bits}"
        );
        let band = bits["bandwidth_hz"].as_u64().unwrap();
        let shift = bits["shift_hz"].as_i64().unwrap().unsigned_abs();
        assert!(band + 2 * shift <= 62_500, "{name}: {bits}");
    }
}
