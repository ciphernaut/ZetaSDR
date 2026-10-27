//! The `identify` job: a controller that tries every plausible mode row as a child job in
//! score order, publishing `candidate` events, and ends with a verdict or unknown. Run
//! over a 2.048 MHz file source of the 1200-baud POCSAG fixture and of a plain tone.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use num_complex::Complex;
use serde_json::Value;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Hz, JobId, SourceId, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource};

const RATE_HZ: u32 = 2_048_000;
const BANDWIDTH_HZ: u64 = 12_500;
/// The APRS frequency: `aprs_decode` scores over `pocsag_decode` here, so it is tried first.
const APRS_HZ: u64 = 144_800_000;
/// A well-known POCSAG centre: `pocsag_decode` is tried first.
const POCSAG_HZ: u64 = 148_637_500;

/// Each node runs a 2.048 MHz source in real time: the tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The fixtures, written once: the POCSAG page centred on [`APRS_HZ`] and on
/// [`POCSAG_HZ`], and one second of a 1 kHz tone on [`APRS_HZ`].
struct Fixtures {
    _dir: tempfile::TempDir,
    pocsag_on_aprs: PathBuf,
    pocsag: PathBuf,
    tone: PathBuf,
}

fn fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let iq = zeta_dsp::synth::pocsag_packet_iq(1_234_567, 3, "HELLO ZETA", 1200, RATE_HZ, 0.0);
        let pocsag_on_aprs = write(dir.path(), "pocsag_aprs", &iq, RATE_HZ, APRS_HZ);
        let pocsag = write(dir.path(), "pocsag", &iq, RATE_HZ, POCSAG_HZ);
        let n = RATE_HZ as usize;
        let tone: Vec<Complex<f32>> = (0..n)
            .map(|k| {
                let ph = 2.0 * std::f64::consts::PI * (k as f64 * 1_000.0 / RATE_HZ as f64);
                Complex::from_polar(0.5, ph as f32)
            })
            .collect();
        let tone = write(dir.path(), "tone", &tone, RATE_HZ, APRS_HZ);
        Fixtures {
            _dir: dir,
            pocsag_on_aprs,
            pocsag,
            tone,
        }
    })
}

/// `iq` as a cf32 SigMF pair `<stem>.sigmf-*` in `dir` centred at `center_hz`.
fn write(dir: &Path, stem: &str, iq: &[Complex<f32>], rate_hz: u32, center_hz: u64) -> PathBuf {
    let meta = CaptureMeta {
        sample_rate_hz: Hz(rate_hz as u64),
        center_hz: Hz(center_hz),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    write_cf32(dir, stem, &meta, iq).expect("write fixture")
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

/// A node over a looping, real-time file source `rx` on `meta`, with the node's
/// classifiers (as `zeta`'s setup registers them).
async fn node_over(dir: &Path, meta: &Path) -> Node {
    let cfg = NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 4.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        // No channel bank: an unoptimised PFB at 2.048 MHz drops about a third of the
        // blocks, and no decoder proves on what is left (the bank's own tests cover it).
        bank_factory: None,
        modes_dir: None,
    };
    let opts = FileOpts {
        realtime: true,
        block_samples: 16_384,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("rx".into()), meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let node = Node::open(cfg, registry(), vec![port]).await.unwrap();
    // The node subscribed (ring, bank) when the port was added.
    src.start();
    for c in zeta_dsp::classifiers(node.modes()) {
        node.add_classifier(c);
    }
    node
}

fn rx() -> SourceId {
    SourceId("rx".into())
}

/// Start `identify` at `center_hz`, 12.5 kHz wide, and collect its events (from before
/// the first is published) until its stream closes.
async fn identify(node: &Node, center_hz: u64) -> (JobId, tokio::task::JoinHandle<Vec<Value>>) {
    let op = Op::JobStart {
        kind: JobKind::Identify,
        source: rx(),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    let id = match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("identify: {r:?}"),
    };
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.provenance.job.as_ref() == Some(&id) && s.kind == StreamKind::Event)
        .expect("identify's event stream")
        .id;
    let mut rx = node.bus().subscribe(&stream).expect("subscribed");
    let events = tokio::spawn(async move {
        let mut out = Vec::new();
        loop {
            match rx.recv().await {
                Ok(f) => {
                    if let Payload::Event(v) = f.payload {
                        out.push(v);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return out,
            }
        }
    });
    (id, events)
}

fn state(node: &Node, id: &JobId) -> JobState {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
        .state
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait for `id` to end and return its candidate events.
async fn ended(
    node: &Node,
    id: &JobId,
    events: tokio::task::JoinHandle<Vec<Value>>,
    within: Duration,
) -> Vec<Value> {
    until("identify ends", within, || {
        state(node, id) != JobState::Running
    })
    .await;
    assert_eq!(state(node, id), JobState::Stopped);
    let events = tokio::time::timeout(Duration::from_secs(5), events)
        .await
        .expect("identify's stream closes")
        .unwrap();
    for e in &events {
        assert_eq!(e["protocol"], "candidate", "{e}");
        let summary = format!(
            "{}: {}",
            e["mode"].as_str().unwrap(),
            e["outcome"].as_str().unwrap()
        );
        assert_eq!(e["summary"], summary.as_str(), "{e}");
    }
    events
}

fn outcome<'a>(events: &'a [Value], mode: &str) -> Option<&'a Value> {
    events.iter().find(|e| e["mode"] == mode)
}

fn running(node: &Node) -> Vec<(String, String)> {
    node.job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running)
        .map(|r| (r.id.0, r.kind))
        .collect()
}

/// Stop every running job, then check the node holds nothing on the source.
async fn stop_all(node: &Node) {
    for (id, _) in running(node) {
        node.job_stop(&JobId(id)).await.unwrap();
    }
    assert!(node.lease_holders(&rx()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_on_pocsag_ends_with_a_verdict_and_one_running_child() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().pocsag_on_aprs).await;
    let (id, events) = identify(&node, APRS_HZ).await;
    let events = ended(&node, &id, events, Duration::from_secs(20)).await;
    eprintln!("{events:#?}");
    let aprs = outcome(&events, "aprs_decode").expect("aprs_decode tried");
    let pocsag = outcome(&events, "pocsag_decode").expect("pocsag_decode tried");
    assert_eq!(aprs["outcome"], "no_frame", "{aprs}");
    assert_eq!(pocsag["outcome"], "proved", "{pocsag}");
    // The attempts (those with a child job) in score order: APRS scores over POCSAG here.
    let tried: Vec<&Value> = events.iter().filter(|e| !e["job"].is_null()).collect();
    assert_eq!(tried.len(), 2, "{events:#?}");
    assert_eq!(tried[0]["mode"], "aprs_decode");
    assert!(tried[0]["score"].as_f64() >= tried[1]["score"].as_f64());
    assert!(
        pocsag["evidence"]
            .as_str()
            .unwrap()
            .contains("1234567: HELLO ZETA"),
        "{pocsag}"
    );

    let ctx = node.job_context(&id).unwrap();
    let verdict = &ctx["identify"]["verdict"];
    assert_eq!(verdict["mode"], "pocsag_decode", "{ctx}");
    assert_eq!(verdict["label"], "POCSAG", "{ctx}");
    assert_eq!(verdict["job"], pocsag["job"], "{ctx}");
    assert_eq!(ctx["identify"]["attempts"], 2, "{ctx}");
    let child = verdict["job"].as_str().unwrap().to_string();
    assert_eq!(running(&node), [(child.clone(), "pocsag_decode".into())]);
    let holders: Vec<String> = node
        .lease_holders(&rx())
        .into_iter()
        .map(|(j, _)| j.0)
        .collect();
    assert_eq!(holders, [child]);
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_on_a_tone_ends_unknown_with_a_label() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().tone).await;
    let (id, events) = identify(&node, APRS_HZ).await;
    let events = ended(&node, &id, events, Duration::from_secs(40)).await;
    eprintln!("{events:#?}");
    let tried: Vec<&Value> = events.iter().filter(|e| !e["job"].is_null()).collect();
    let modes: Vec<&str> = tried.iter().map(|e| e["mode"].as_str().unwrap()).collect();
    assert_eq!(modes.len(), 3, "{events:#?}");
    for m in ["aprs_decode", "pocsag_decode", "acars_decode"] {
        assert!(modes.contains(&m), "{m} not tried: {events:#?}");
    }
    for e in &tried {
        assert_eq!(e["outcome"], "no_frame", "{e}");
    }
    let ctx = node.job_context(&id).unwrap();
    let verdict = &ctx["identify"]["verdict"];
    assert!(verdict["mode"].is_null(), "{ctx}");
    let label = verdict["label"].as_str().unwrap();
    assert!(label == "carrier" || label == "fm", "{ctx}");
    assert!(verdict["features"]["bandwidth_hz"].is_u64(), "{ctx}");
    assert_eq!(ctx["identify"]["attempts"], 3, "{ctx}");
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    assert!(node.lease_holders(&rx()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_stop_leaves_no_child_and_no_lease() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().pocsag_on_aprs).await;
    let (id, events) = identify(&node, APRS_HZ).await;
    // Stopped during the first dwell (APRS, which never proves on a page).
    until("the first child runs", Duration::from_secs(10), || {
        running(&node).iter().any(|(_, k)| k == "aprs_decode")
    })
    .await;
    // Both rigid on the one capture while the child runs.
    assert_eq!(node.lease_holders(&rx()).len(), 2);
    node.job_stop(&id).await.unwrap();
    assert_eq!(state(&node, &id), JobState::Stopped);
    assert!(running(&node).is_empty(), "{:?}", running(&node));
    assert!(node.lease_holders(&rx()).is_empty());
    let events = events.await.unwrap();
    assert!(
        events.iter().all(|e| e["outcome"] != "proved"),
        "{events:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proof_leaves_one_named_child() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().pocsag).await;
    let (id, events) = identify(&node, POCSAG_HZ).await;
    let events = ended(&node, &id, events, Duration::from_secs(15)).await;
    // POCSAG scores first at its own centre: the one attempt, and it proves.
    let tried: Vec<&Value> = events.iter().filter(|e| !e["job"].is_null()).collect();
    assert_eq!(tried.len(), 1, "{events:#?}");
    assert_eq!(tried[0]["mode"], "pocsag_decode");
    assert_eq!(tried[0]["outcome"], "proved");
    let ctx = node.job_context(&id).unwrap();
    let child = ctx["identify"]["verdict"]["job"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(tried[0]["job"], child.as_str());
    assert_eq!(running(&node), [(child.clone(), "pocsag_decode".into())]);
    let holders: Vec<String> = node
        .lease_holders(&rx())
        .into_iter()
        .map(|(j, _)| j.0)
        .collect();
    assert_eq!(holders, [child]);
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wide_rows_are_skipped_band_on_a_narrow_identify() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().tone).await;
    let (id, events) = identify(&node, APRS_HZ).await;
    until("the first child runs", Duration::from_secs(10), || {
        running(&node).len() == 2
    })
    .await;
    node.job_stop(&id).await.unwrap();
    let events = events.await.unwrap();
    for m in ["rds_decode", "adsb_decode"] {
        let e = outcome(&events, m).unwrap_or_else(|| panic!("{m}: {events:#?}"));
        assert_eq!(e["outcome"], "skipped_band", "{e}");
        assert!(e["job"].is_null(), "{e}");
        assert!(e["evidence"].as_str().unwrap().contains("12500 Hz"), "{e}");
    }
    assert!(running(&node).is_empty());
    assert!(node.lease_holders(&rx()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_is_neither_elastic_nor_attachable() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().tone).await;
    let op = Op::JobStart {
        kind: JobKind::Identify,
        source: rx(),
        center_hz: Hz(APRS_HZ),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: true,
        replace: vec![],
    };
    let e = node.apply(op).await.unwrap_err();
    assert!(e.to_string().contains("identify cannot be elastic"), "{e}");
    let op = Op::JobAttach {
        kind: JobKind::Identify,
        job: JobId::new(),
        language: None,
    };
    let e = node.apply(op).await.unwrap_err();
    assert!(e.to_string().contains("identify is not attachable"), "{e}");
    assert!(node.job_list().is_empty());
}
