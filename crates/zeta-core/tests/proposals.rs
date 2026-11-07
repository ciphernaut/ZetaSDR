//! The node's own proposals: a hand `capture` job proposes a features-only signature at
//! its end (unless its classify proved a mode, or an accepted signature already holds
//! the band); a `bits` or frame-row job proposes once when it reaches five CRC-clean
//! frames. Every exploration job leaves a session record under `state_dir/sessions`,
//! swept with its captures. Run over 48 kHz file sources of a plain 2-FSK burst train,
//! a POCSAG page and the `framing` synthesiser family.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use num_complex::Complex;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use zeta_core::bits::Modulation;
use zeta_core::bursts::NewCapture;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::Port;
use zeta_core::signatures::{CONFIDENCE_CRC, CONFIDENCE_FEATURES, SignatureState};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Hz, JobId, SourceId, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource};

const RATE_HZ: u32 = 48_000;
const CENTER_HZ: u64 = 433_920_000;
const BANDWIDTH_HZ: u64 = 12_500;
const BAUD: u32 = 4_800;
const ROW_ID: &str = "fsk_test_frames";
const T0_NS: u64 = 1_760_000_000_000_000_000;

/// Each node runs a real-time file source: the node tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The framing of the `framing` fixture and of the frame row: a 16-bit sync, a fixed
/// 272-bit frame, a CRC-16/CCITT.
fn framing() -> Value {
    json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"})
}

/// The frame row of `tests/frame_rows.rs`.
fn row_json() -> Value {
    json!({
        "id": ROW_ID,
        "name": "FSK test frames",
        "family": "general",
        "band": {"bandwidth_hz": BANDWIDTH_HZ, "centers_hz": [CENTER_HZ]},
        "modulation": ["fsk", "fm"],
        "label": "FSK frames",
        "protocol": "frame",
        "decoder": {
            "kind": "frame",
            "demod": {"modulation": "fsk", "symbol_rate_baud": BAUD, "order": 2, "tone_spacing_hz": null},
            "framing": framing(),
            "parser": "opaque"
        },
        "event": {
            "schema": {"type": "object", "properties": {
                "protocol": {"type": "string"},
                "sync": {"type": "string"},
                "length_bytes": {"type": "integer"},
                "bytes": {"type": "string"},
                "crc_ok": {"type": "boolean"},
                "payload": {"type": "string"}
            }},
            "summary": "frame {length_bytes} B"
        },
        "identify": {"proof": "field", "field": "crc_ok", "min_events": 1, "dwell_ms": 10000,
            "min_span_ms": 1000, "quick": "frame"},
        "catalogue": []
    })
}

/// Task 3's `framing` spec: 8 frames of 32 pseudo-random bytes at 4800 Bd fsk, 2400 Hz
/// deviation, 20 dB, at [`RATE_HZ`].
fn framed_iq() -> Vec<Complex<f32>> {
    let params = json!({"family": "framing", "modulation": "fsk", "symbol_rate_baud": BAUD,
        "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32, "seed": 7,
        "framing": framing(), "repeat": 1, "snr_db": 20.0});
    let (spec, imp) = zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
    zeta_dsp::synth::synthesise(&spec, &imp, RATE_HZ, CENTER_HZ)
}

/// One second of a 50 ms 4800-baud GFSK burst every 100 ms, 20 dB over the noise (as
/// `tests/capture_job.rs`): no row proves it.
fn fsk_train() -> Vec<Complex<f32>> {
    let mut state = 0x5eed_u64;
    let symbols: Vec<u8> = (0..240)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 63) as u8
        })
        .collect();
    let mut burst = zeta_dsp::synth::gfsk_modulate(&symbols, 2, BAUD, 2_400.0, RATE_HZ);
    burst.resize(RATE_HZ as usize / 10, Complex::new(0.0, 0.0));
    burst.rotate_right(RATE_HZ as usize / 20);
    let mut iq: Vec<Complex<f32>> = burst
        .iter()
        .copied()
        .cycle()
        .take(RATE_HZ as usize)
        .collect();
    zeta_dsp::synth::awgn(&mut iq, 20.0, 7);
    iq
}

/// A 1200-baud POCSAG page then 0.5 s off, 20 dB over the noise (as
/// `tests/capture_job.rs`): the classify proves `pocsag_decode`.
fn pocsag() -> Vec<Complex<f32>> {
    let mut iq = zeta_dsp::synth::pocsag_packet_iq(1_234_567, 3, "HELLO ZETA", 1200, RATE_HZ, 0.0);
    iq.resize(iq.len() + RATE_HZ as usize / 2, Complex::new(0.0, 0.0));
    zeta_dsp::synth::awgn(&mut iq, 20.0, 11);
    iq
}

fn write(dir: &Path, stem: &str, iq: &[Complex<f32>]) -> PathBuf {
    let meta = CaptureMeta {
        sample_rate_hz: Hz(RATE_HZ as u64),
        center_hz: Hz(CENTER_HZ),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    write_cf32(dir, stem, &meta, iq).expect("write fixture")
}

fn rx() -> SourceId {
    SourceId("rx".into())
}

/// A node over a looping real-time file source of `iq` (none for `None`), with the
/// frame row in its modes directory when `row`, and zeta-dsp's classifiers.
async fn open(dir: &Path, iq: Option<&[Complex<f32>]>, row: bool) -> Node {
    let modes_dir = row.then(|| {
        let m = dir.join("modes");
        std::fs::create_dir_all(&m).unwrap();
        std::fs::write(m.join(format!("{ROW_ID}.json")), row_json().to_string()).unwrap();
        m
    });
    let cfg = NodeConfig {
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 4.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: 20,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir,
    };
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut registry);
    let mut ports: Vec<Arc<dyn Port>> = Vec::new();
    let mut src = None;
    if let Some(iq) = iq {
        let meta = write(dir, "fixture", iq);
        let opts = FileOpts {
            realtime: true,
            block_samples: 2_400,
            repeat: true,
            tune_range: None,
        };
        let s = Arc::new(FileSource::open(rx(), &meta, opts).unwrap());
        ports.push(s.clone());
        src = Some(s);
    }
    let node = Node::open(cfg, registry, ports).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    if let Some(s) = src {
        s.start();
    }
    node
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn state(node: &Node, id: &JobId) -> Option<JobState> {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .map(|r| r.state)
}

fn session_path(node_dir: &Path, job: &JobId) -> PathBuf {
    node_dir
        .join("state")
        .join("sessions")
        .join(format!("{}.json", job.0))
}

fn session(node_dir: &Path, job: &JobId) -> Option<Value> {
    let bytes = std::fs::read(session_path(node_dir, job)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn close(a: f32, b: Value) -> bool {
    b.as_f64().is_some_and(|b| (b - f64::from(a)).abs() < 1e-6)
}

/// Every event `job`'s event outputs carry from now on.
struct Events(Arc<Mutex<Vec<Value>>>);

impl Events {
    fn of(node: &Node, job: &JobId) -> Events {
        let seen = Arc::new(Mutex::new(Vec::new()));
        for (_, sid) in node
            .job_outputs(job)
            .into_iter()
            .filter(|(k, _)| *k == StreamKind::Event)
        {
            let mut rx = node.bus().subscribe(&sid).expect("registered");
            let out = seen.clone();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(f) => {
                            if let Payload::Event(v) = f.payload {
                                out.lock().unwrap().push(v);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => return,
                    }
                }
            });
        }
        Events(seen)
    }

    fn signatures(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["protocol"] == "signature")
            .cloned()
            .collect()
    }
}

/// A `capture` job on the centre, its `burst_capture` put to `bursts` whole windows as
/// the e2e's `two_windows` does; its id once the put run's outputs are registered.
async fn capture_job(node: &Node, bursts: u32) -> JobId {
    let op = Op::JobStart {
        kind: JobKind::Capture,
        source: rx(),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    let job = match node.apply(op).await {
        Ok(OpResult::Job(j)) => j,
        r => panic!("job_start: {r:?}"),
    };
    let mut graph = node
        .job_list()
        .into_iter()
        .find(|r| r.id == job)
        .unwrap()
        .graph;
    for n in graph.nodes.iter_mut().filter(|n| n.kind == "burst_capture") {
        n.params["bursts"] = bursts.into();
        n.params["whole"] = true.into();
    }
    let r = node
        .apply(Op::GraphPut {
            job: job.clone(),
            graph,
        })
        .await;
    assert!(matches!(r, Ok(OpResult::Ok)), "{r:?}");
    until("the put run's outputs", Duration::from_secs(5), || {
        !node.job_outputs(&job).is_empty()
    })
    .await;
    job
}

async fn ended(node: &Node, job: &JobId) {
    until("the job ends", Duration::from_secs(30), || {
        state(node, job) != Some(JobState::Running)
    })
    .await;
}

fn written(node: &Node, job: &JobId) -> Vec<String> {
    node.job_context(job).unwrap()["capture"]["written"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// Run a hand capture of two windows of the FSK train to its end; its id, its events.
async fn hand_capture(node: &Node) -> (JobId, Events) {
    let job = capture_job(node, 2).await;
    let events = Events::of(node, &job);
    ended(node, &job).await;
    assert_eq!(state(node, &job), Some(JobState::Stopped));
    (job, events)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hand_capture_proposes_features_only_at_its_end() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), Some(&fsk_train()), false).await;
    let (job, events) = hand_capture(&node).await;
    until("one proposal", Duration::from_secs(10), || {
        node.signatures().list(None).len() == 1
    })
    .await;
    let sig = node.signatures().list(None).remove(0);
    assert_eq!(sig.state, SignatureState::Proposed);
    assert_eq!(sig.confidence, CONFIDENCE_FEATURES, "{sig:?}");
    assert_eq!(sig.modulation, "fsk", "{sig:?}");
    assert_eq!(sig.framing, None, "{sig:?}");
    assert!(sig.evidence.starts_with("features only: "), "{sig:?}");
    let written = written(&node, &job);
    assert_eq!(written.len(), 2);
    assert_eq!(sig.provenance.captures, written, "{sig:?}");
    assert_eq!(sig.provenance.jobs, std::slice::from_ref(&job), "{sig:?}");
    assert_eq!(sig.provenance.proposed_by, "node", "{sig:?}");
    assert_eq!(sig.provenance.source, rx(), "{sig:?}");
    until("the signature event", Duration::from_secs(5), || {
        !events.signatures().is_empty()
    })
    .await;
    let ev = events.signatures();
    assert_eq!(ev.len(), 1, "{ev:?}");
    let e = &ev[0];
    assert_eq!(e["state"], "proposed", "{e}");
    assert_eq!(e["id"], sig.id.as_str(), "{e}");
    assert_eq!(e["name"], sig.name.as_str(), "{e}");
    assert!(close(CONFIDENCE_FEATURES, e["confidence"].clone()), "{e}");
    assert_eq!(
        e["summary"],
        format!("proposed {} ({})", sig.name, sig.confidence),
        "{e}"
    );
    assert!(e["t_ns"].is_u64(), "{e}");
    let ctx = node.job_context(&job).unwrap();
    assert_eq!(ctx["capture"]["proposal"], sig.id.as_str(), "{ctx}");
    let s = session(dir.path(), &job).expect("the session record");
    assert_eq!(
        s["outcome"],
        json!({"kind": "proposal", "id": sig.id}),
        "{s}"
    );
    assert_eq!(s["kind"], "capture", "{s}");
    assert_eq!(s["captures"], json!(written), "{s}");
    assert_eq!(s["center_hz"], CENTER_HZ, "{s}");
    assert!(s["features"].is_object(), "{s}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proved_capture_proposes_nothing() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), Some(&pocsag()), false).await;
    let job = capture_job(&node, 1).await;
    ended(&node, &job).await;
    until("the session record", Duration::from_secs(5), || {
        session(dir.path(), &job).is_some()
    })
    .await;
    let ctx = node.job_context(&job).unwrap();
    let tried = &ctx["capture"]["classified"]["tried"];
    assert!(
        tried
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["outcome"] == "proved"),
        "{ctx}"
    );
    assert!(node.signatures().list(None).is_empty());
    assert!(ctx["capture"].get("proposal").is_none(), "{ctx}");
    let s = session(dir.path(), &job).unwrap();
    assert_eq!(s["outcome"], json!({"kind": "none"}), "{s}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_proposal_when_an_accepted_signature_holds_the_band() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), Some(&fsk_train()), false).await;
    // A window of the train on the band, proposed and accepted by hand.
    let iq: Vec<Complex<f32>> = fsk_train().into_iter().take(RATE_HZ as usize / 5).collect();
    let features = json!({
        "bandwidth_hz": 9_600, "center_hz": CENTER_HZ, "symbol_rate_baud": BAUD,
        "snr_db": 20.0, "burst": true, "duty": 0.5, "bursts": 2, "burst_ms": 50.0,
        "period_ms": 100.0, "order": 2, "tone_spacing_hz": 4_800, "symbol_rate_line": null,
    });
    let cap = node
        .bursts()
        .write(
            NewCapture {
                source: rx(),
                set: JobId("hand".into()),
                center_hz: Hz(CENTER_HZ),
                bandwidth_hz: Hz(BANDWIDTH_HZ),
                sample_rate_hz: Hz(RATE_HZ as u64),
                start_ns: T0_NS,
                end_ns: T0_NS + 200_000_000,
                features,
                signal: None,
                job: JobId("hand".into()),
                watch: None,
                allocations: json!([]),
            },
            &iq,
        )
        .unwrap()
        .id;
    let sig = node
        .signature_propose(&cap, None, None, "test")
        .await
        .unwrap();
    node.signature_accept(&sig.id, None, "test").unwrap();
    let (job, _events) = hand_capture(&node).await;
    until("the session record", Duration::from_secs(5), || {
        session(dir.path(), &job).is_some()
    })
    .await;
    let all = node.signatures().list(None);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].id, sig.id);
    let ctx = node.job_context(&job).unwrap();
    assert!(ctx["capture"].get("proposal").is_none(), "{ctx}");
}

/// The framed capture written to the store, and a `bits` job started on it.
async fn bits_on_framed(node: &Node) -> (String, JobId) {
    let iq = framed_iq();
    let ms = iq.len() as u64 * 1_000 / RATE_HZ as u64;
    let features = json!({
        "bandwidth_hz": 9_600, "center_hz": CENTER_HZ, "symbol_rate_baud": BAUD,
        "snr_db": 20.0, "burst": false, "duty": 1.0, "bursts": 1, "burst_ms": ms,
        "period_ms": null, "order": 2, "tone_spacing_hz": 4_800, "symbol_rate_line": null,
    });
    let cap = node
        .bursts()
        .write(
            NewCapture {
                source: rx(),
                set: JobId("set".into()),
                center_hz: Hz(CENTER_HZ),
                bandwidth_hz: Hz(20_000),
                sample_rate_hz: Hz(RATE_HZ as u64),
                start_ns: T0_NS,
                end_ns: T0_NS + ms * 1_000_000,
                features,
                signal: None,
                job: JobId("set".into()),
                watch: None,
                allocations: json!([]),
            },
            &iq,
        )
        .unwrap()
        .id;
    let job = match node
        .apply(Op::BitsStart {
            capture: cap.clone(),
            modulation: Some(Modulation::Fsk),
            symbol_rate_baud: None,
            order: None,
        })
        .await
    {
        Ok(OpResult::Job(id)) => id,
        r => panic!("bits_start: {r:?}"),
    };
    (cap, job)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bits_job_proposes_once_however_many_passes() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), None, false).await;
    let (cap, job) = bits_on_framed(&node).await;
    until("a proposal", Duration::from_secs(20), || {
        !node.signatures().list(None).is_empty()
    })
    .await;
    let sig = node.signatures().list(None).remove(0);
    assert_eq!(sig.confidence, CONFIDENCE_CRC, "{sig:?}");
    assert_eq!(sig.modulation, "fsk", "{sig:?}");
    let f = sig.framing.clone().expect("a framing");
    assert_eq!(f["sync"], json!({"hex": "2dd4", "bits": 16}), "{f}");
    assert_eq!(f["length"], json!({"fixed_bits": 272}), "{f}");
    assert_eq!(f["crc"], "crc16-ccitt", "{f}");
    assert_eq!(f["encoding"], "nrz", "{f}");
    assert_eq!(f["bit_order"], "msb", "{f}");
    assert!(
        sig.evidence.starts_with("crc16-ccitt on ")
            && sig.evidence.ends_with(", sync 0x2dd4 every 288 bits"),
        "{sig:?}"
    );
    assert_eq!(
        sig.provenance.captures,
        std::slice::from_ref(&cap),
        "{sig:?}"
    );
    assert_eq!(sig.provenance.jobs, std::slice::from_ref(&job), "{sig:?}");
    assert_eq!(sig.provenance.proposed_by, "node", "{sig:?}");
    // Two more passes (each under a second, `loop_pause_ms` 500 between them) propose
    // nothing more.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(node.signatures().list(None).len(), 1);
    let ctx = node.job_context(&job).unwrap();
    assert_eq!(ctx["proposal"], sig.id.as_str(), "{ctx}");
    node.job_stop(&job).await.unwrap();
    until("the session record", Duration::from_secs(5), || {
        session(dir.path(), &job).is_some()
    })
    .await;
    let s = session(dir.path(), &job).unwrap();
    assert_eq!(s["kind"], "bits", "{s}");
    assert_eq!(
        s["outcome"],
        json!({"kind": "proposal", "id": sig.id}),
        "{s}"
    );
    assert!(s["hypotheses"]["crc_candidates"].is_array(), "{s}");
    assert_eq!(s["captures"], json!([cap]), "{s}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proposal_from_a_jobs_last_events_is_in_its_session() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), None, false).await;
    let (_cap, job) = bits_on_framed(&node).await;
    // One unpaced pass: `bit_view` publishes its hypotheses as its input closes, and the
    // job drains and ends right after them.
    let mut graph = node
        .job_list()
        .into_iter()
        .find(|r| r.id == job)
        .unwrap()
        .graph;
    let src = graph
        .nodes
        .iter_mut()
        .find(|n| n.kind == "capture_in")
        .unwrap();
    src.params["loop"] = json!(false);
    src.params["realtime"] = json!(false);
    let r = node
        .apply(Op::GraphPut {
            job: job.clone(),
            graph,
        })
        .await;
    assert!(matches!(r, Ok(OpResult::Ok)), "{r:?}");
    ended(&node, &job).await;
    let ctx = node.job_context(&job).unwrap();
    let id = ctx["proposal"]
        .as_str()
        .unwrap_or_else(|| panic!("a proposal: {ctx}"))
        .to_string();
    until("the session record", Duration::from_secs(5), || {
        session(dir.path(), &job).is_some()
    })
    .await;
    let s = session(dir.path(), &job).unwrap();
    assert_eq!(s["outcome"], json!({"kind": "proposal", "id": id}), "{s}");
    assert_eq!(node.signatures().list(None).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frame_row_job_proposes_nothing() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), Some(&framed_iq()), true).await;
    let job = frame_row_job(&node).await;
    // The row decodes the signal: its frames are its proof, not a proposal's.
    until("five CRC-clean frames", Duration::from_secs(20), || {
        node.job_context(&job)
            .is_some_and(|c| c["frame_decode"]["crc_ok"].as_u64().is_some_and(|n| n >= 5))
    })
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(node.signatures().list(None).is_empty());
    let ctx = node.job_context(&job).unwrap();
    assert!(ctx.get("proposal").is_none(), "{ctx}");
    node.job_stop(&job).await.unwrap();
    until("the session record", Duration::from_secs(5), || {
        session(dir.path(), &job).is_some()
    })
    .await;
    let s = session(dir.path(), &job).unwrap();
    assert_eq!(
        s["outcome"],
        json!({"kind": "proved", "mode": ROW_ID}),
        "{s}"
    );
    assert!(node.signatures().list(None).is_empty());
}

async fn frame_row_job(node: &Node) -> JobId {
    let op = Op::JobStart {
        kind: JobKind::from_name(ROW_ID).unwrap(),
        source: rx(),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start: {r:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_are_swept_with_their_captures() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), Some(&fsk_train()), true).await;
    let (job, _events) = hand_capture(&node).await;
    until("the session record", Duration::from_secs(5), || {
        session(dir.path(), &job).is_some()
    })
    .await;
    // A frame-row job on the radio: its session lists no captures.
    let frames = frame_row_job(&node).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    node.job_stop(&frames).await.unwrap();
    until("the frame job's session", Duration::from_secs(5), || {
        session(dir.path(), &frames).is_some()
    })
    .await;
    assert_eq!(session(dir.path(), &frames).unwrap()["captures"], json!([]));
    let ids = written(&node, &job);
    node.apply(Op::CaptureDelete {
        capture: ids[0].clone(),
    })
    .await
    .unwrap();
    // One capture left: the session stays.
    assert!(session_path(dir.path(), &job).is_file());
    node.apply(Op::CaptureDelete {
        capture: ids[1].clone(),
    })
    .await
    .unwrap();
    assert!(!session_path(dir.path(), &job).exists());
    assert!(session_path(dir.path(), &frames).is_file());
}
