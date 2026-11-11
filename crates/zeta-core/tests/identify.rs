//! The `identify` job: a controller that tries every plausible mode row as a child job in
//! score order, publishing `candidate` events, and ends with a verdict or unknown. Run
//! over a 2.048 MHz file source of the 1200-baud POCSAG fixture and of a plain tone.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use std::time::Duration;

use num_complex::Complex;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::job::JobState;
use zeta_core::knowledge::{Allocation, KnowledgeProvider, Signature};
use zeta_core::modes::score::score_rows;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Capabilities, Hz, JobId, SourceId, StreamId, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource};

const RATE_HZ: u32 = 2_048_000;
const BANDWIDTH_HZ: u64 = 12_500;
/// The APRS frequency: `aprs_decode` scores over `pocsag_decode` here, so it is tried first.
const APRS_HZ: u64 = 144_800_000;
/// A well-known POCSAG centre: `pocsag_decode` is tried first.
const POCSAG_HZ: u64 = 148_637_500;
/// Where the tone fixture's 1 kHz tone sits.
const TONE_HZ: u64 = APRS_HZ + 1_000;
/// The narrow fixture's rate: a capture no wider than an identify band of this width.
const NARROW_RATE_HZ: u32 = 48_000;
/// The narrow fixture's tone, above its centre.
const NARROW_TONE_HZ: u64 = 2_000;
/// A radiosonde frequency in 400.05-406 MHz, where an operator's sonde watch runs.
const SONDE_HZ: u64 = 403_025_000;
/// The rows that live elsewhere (pagers above 151.8 MHz, AIS at 162 MHz, APRS at 144.39-145.175 MHz):
/// identify never tries them on the sonde band.
const NOT_ON_THE_SONDE_BAND: [&str; 3] = ["flex_decode", "ais_decode", "aprs_decode"];

/// Whether the `external_decode` stand-in answers for `rs41_decode` with a sonde frame.
static PROVE_RS41: AtomicBool = AtomicBool::new(false);

/// Each node runs a 2.048 MHz source in real time: the tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The fixtures, written once: the POCSAG page centred on [`APRS_HZ`] and on
/// [`POCSAG_HZ`], one second of a 1 kHz tone on [`APRS_HZ`], and one second of random
/// 2400-baud GFSK (no row proves it) on [`APRS_HZ`], and one second sampled at
/// [`NARROW_RATE_HZ`] on [`APRS_HZ`] holding a tone [`NARROW_TONE_HZ`] above it, and one
/// second of RS41-like 4800-baud GFSK (the same random symbols) on [`SONDE_HZ`].
struct Fixtures {
    _dir: tempfile::TempDir,
    pocsag_on_aprs: PathBuf,
    pocsag: PathBuf,
    tone: PathBuf,
    gfsk: PathBuf,
    narrow: PathBuf,
    sonde: PathBuf,
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
        let mut state = 0x5eed_u64;
        let symbols: Vec<u8> = (0..2400)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 63) as u8
            })
            .collect();
        let mut gfsk = zeta_dsp::synth::gfsk_modulate(&symbols, 2, 2400, 3_000.0, RATE_HZ);
        zeta_dsp::synth::awgn(&mut gfsk, 20.0, 1);
        let gfsk = write(dir.path(), "gfsk", &gfsk, RATE_HZ, APRS_HZ);
        // RS41-like: 4800-baud GFSK, ±2.4 kHz, on the sonde band.
        let mut sonde = zeta_dsp::synth::gfsk_modulate(&symbols, 2, 4800, 2_400.0, RATE_HZ);
        zeta_dsp::synth::awgn(&mut sonde, 20.0, 3);
        let sonde = write(dir.path(), "sonde", &sonde, RATE_HZ, SONDE_HZ);
        let n = NARROW_RATE_HZ as usize;
        let mut narrow: Vec<Complex<f32>> = (0..n)
            .map(|k| {
                let cycles = k as f64 * NARROW_TONE_HZ as f64 / NARROW_RATE_HZ as f64;
                Complex::from_polar(0.5, (2.0 * std::f64::consts::PI * cycles.fract()) as f32)
            })
            .collect();
        zeta_dsp::synth::awgn(&mut narrow, 20.0, 2);
        let narrow = write(dir.path(), "narrow", &narrow, NARROW_RATE_HZ, APRS_HZ);
        Fixtures {
            _dir: dir,
            pocsag_on_aprs,
            pocsag,
            tone,
            gfsk,
            narrow,
            sonde,
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
    // Stands in for the program rows' stage (zeta-ext's), so they start as trials.
    r.register(Arc::new(ExternalStandIn(StageSpec {
        kind: "external_decode",
        inputs: vec![StreamKind::Iq, StreamKind::Audio],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({"type": "object"}),
    })));
    r
}

/// Stands in for `external_decode`: it drains its input, and for `rs41_decode` while
/// [`PROVE_RS41`] is set publishes a sonde frame per input frame.
struct ExternalStandIn(StageSpec);

#[async_trait]
impl Stage for ExternalStandIn {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        params: Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let prove = params["mode"] == "rs41_decode" && PROVE_RS41.load(Ordering::SeqCst);
        let Some(rx) = inputs.first_mut() else {
            return Err(StageError::Failed("expected 1 input".into()));
        };
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        if prove && let Some(out) = outputs.first() {
                            let ev = serde_json::json!({"protocol": "rs41", "frame": 1, "id": "S1234567"});
                            let f = Frame::new(out.clone(), frame.provenance.clone(), Payload::Event(ev));
                            bus.publish(f);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

/// A node over a looping, real-time file source `rx` on `meta`, with the node's
/// classifiers (as `zeta`'s setup registers them).
async fn node_over(dir: &Path, meta: &Path) -> Node {
    node_with(dir, meta, |src| src).await
}

/// [`node_over`] with the port `wrap` makes of the file source.
async fn node_with(
    dir: &Path,
    meta: &Path,
    wrap: impl FnOnce(Arc<dyn Port>) -> Arc<dyn Port>,
) -> Node {
    let (node, src) = node_unstarted(dir, meta, wrap).await;
    src.start();
    node
}

/// [`node_with`] with the file source not yet started: it delivers nothing until the
/// caller starts it.
async fn node_unstarted(
    dir: &Path,
    meta: &Path,
    wrap: impl FnOnce(Arc<dyn Port>) -> Arc<dyn Port>,
) -> (Node, Arc<FileSource>) {
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
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
    let port: Arc<dyn Port> = wrap(src.clone());
    let node = Node::open(cfg, registry(), vec![port]).await.unwrap();
    // The node subscribed (ring, bank) when the port was added: the caller starts it.
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    (node, src)
}

/// A port that is the file source but for a fault the test sets.
struct Faulty {
    inner: Arc<dyn Port>,
    fault: Mutex<Option<String>>,
}

#[async_trait]
impl Port for Faulty {
    fn id(&self) -> &SourceId {
        self.inner.id()
    }
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.inner.tune(t).await
    }
    fn current(&self) -> Option<Tuning> {
        self.inner.current()
    }
    fn subscribe(&self) -> IqReceiver {
        self.inner.subscribe()
    }
    fn fault(&self) -> Option<String> {
        self.fault.lock().unwrap().clone()
    }
    async fn idle(&self) {
        self.inner.idle().await
    }
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
        dwell_ms: None,
        average: None,
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

/// Wait for `id` to end and return its candidate events (the `verdict` event, which
/// must come last and once, is checked against the context's verdict and left out).
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
    let mut events = tokio::time::timeout(Duration::from_secs(5), events)
        .await
        .expect("identify's stream closes")
        .unwrap();
    let last = events.pop().expect("identify published events");
    assert_eq!(last["protocol"], "verdict", "{last}");
    // One `origin` event per run, ahead of the candidates.
    let origin = events.iter().filter(|e| e["protocol"] == "origin").count();
    assert_eq!(origin, 1, "{events:#?}");
    events.retain(|e| e["protocol"] != "origin");
    let ctx = node.job_context(id).unwrap();
    let v = &ctx["identify"]["verdict"];
    assert_eq!(last["mode"], v["mode"], "{last} vs {ctx}");
    assert_eq!(last["job"], v["job"].clone(), "{last} vs {ctx}");
    assert_eq!(last["allocations"], v["allocations"], "{last} vs {ctx}");
    let summary = last["summary"].as_str().unwrap();
    match (v["mode"].is_null(), v["learned"].is_null()) {
        (true, true) => assert!(summary.starts_with("unknown at "), "{last}"),
        // A learned match names the signature, as the watch does.
        (true, false) => assert!(summary.starts_with("learned "), "{last}"),
        (false, _) => assert!(summary.contains(" at "), "{last}"),
    }
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

/// Stop every running job, then check the node comes to hold nothing on the source (a
/// cancelled child launch finishing in a task takes the lease until the task stops it).
async fn stop_all(node: &Node) {
    for (id, _) in running(node) {
        node.job_stop(&JobId(id)).await.unwrap();
    }
    until("no lease on rx", Duration::from_secs(5), || {
        node.lease_holders(&rx()).is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_on_pocsag_ends_with_a_verdict_and_one_running_child() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().pocsag_on_aprs).await;
    let (id, events) = identify(&node, APRS_HZ).await;
    let events = ended(&node, &id, events, Duration::from_secs(60)).await;
    eprintln!("{events:#?}");
    let aprs = outcome(&events, "aprs_decode").expect("aprs_decode tried");
    let pocsag = outcome(&events, "pocsag_decode").expect("pocsag_decode tried");
    assert_eq!(aprs["outcome"], "no_frame", "{aprs}");
    assert_eq!(pocsag["outcome"], "proved", "{pocsag}");
    // The attempts (those with a child job) in score order: APRS scores over POCSAG here,
    // M17 (its empty centre list scores anywhere) ties APRS and goes after it; AIS lives
    // on its two channels at 162 MHz, so it is skipped here, never tried.
    let tried: Vec<&Value> = events.iter().filter(|e| !e["job"].is_null()).collect();
    assert_eq!(tried.len(), 3, "{events:#?}");
    assert_eq!(tried[0]["mode"], "aprs_decode");
    assert_eq!(tried[1]["mode"], "m17_decode");
    assert_eq!(tried[1]["outcome"], "no_frame");
    assert_eq!(tried[2]["mode"], "pocsag_decode");
    assert!(tried[0]["score"].as_f64() >= tried[2]["score"].as_f64());
    let ais = outcome(&events, "ais_decode").expect("ais_decode listed");
    assert_eq!(ais["outcome"], "skipped_band", "{ais}");
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
    assert_eq!(ctx["identify"]["attempts"], 3, "{ctx}");
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
    let events = ended(&node, &id, events, Duration::from_secs(60)).await;
    eprintln!("{events:#?}");
    let tried: Vec<&Value> = events.iter().filter(|e| !e["job"].is_null()).collect();
    let modes: Vec<&str> = tried.iter().map(|e| e["mode"].as_str().unwrap()).collect();
    // AIS (162 MHz only) is not tried at 144.8 MHz.
    assert_eq!(modes.len(), 4, "{events:#?}");
    for m in ["aprs_decode", "m17_decode", "pocsag_decode", "acars_decode"] {
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
    assert_eq!(ctx["identify"]["attempts"], 4, "{ctx}");
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
async fn skipped_rows_publish_at_their_turn_not_ahead_of_the_trials() {
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
    // Candidates come in score order, each at its row's turn: stopped during the first
    // trial, the wide rows (scored below it) have published nothing, and no skip was
    // batched ahead of the trials.
    assert!(
        events.iter().all(|e| e["outcome"] != "skipped_band"),
        "{events:#?}"
    );
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
        dwell_ms: None,
        average: None,
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

/// The first child's start, once it runs: its id and `created_ns`.
async fn first_child(node: &Node, kind: &str) -> (JobId, u64) {
    until("the first child runs", Duration::from_secs(10), || {
        running(node).iter().any(|(_, k)| k == kind)
    })
    .await;
    let rec = node
        .job_list()
        .into_iter()
        .find(|r| r.kind == kind && r.state == JobState::Running)
        .expect("child listed");
    (rec.id, rec.created_ns.0)
}

/// A `replace` of identify that takes the source's gate before the first child's dwell
/// ends: identify then stops that child while the replace holds the gate and waits for
/// identify. The child is stopped gated, so the replace completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replace_of_identify_completes_while_a_child_is_stopped() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().pocsag_on_aprs).await;
    let (id, events) = identify(&node, APRS_HZ).await;
    let (child, created_ns) = first_child(&node, "aprs_decode").await;
    let at = |ms: u64| {
        let wall = std::time::UNIX_EPOCH + Duration::from_nanos(created_ns + ms * 1_000_000);
        let left = wall
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default();
        tokio::time::Instant::now() + left
    };
    // The APRS dwell is 5000 ms from about `created_ns`: the gate is held across its end.
    tokio::time::sleep_until(at(4_300)).await;
    let gate = node.debug_gate(&rx()).await;
    let op = Op::JobStart {
        kind: JobKind::Measure,
        source: rx(),
        center_hz: Hz(APRS_HZ + 100_000),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        elastic: false,
        replace: vec![id.clone()],
    };
    let replace = node.apply(op);
    tokio::pin!(replace);
    // Queued on the gate before the dwell ends and identify stops the child.
    let early = tokio::time::timeout_at(at(5_800), &mut replace).await;
    assert!(
        early.is_err(),
        "the replace ran while the gate was held: {early:?}"
    );
    drop(gate);
    let measure = match tokio::time::timeout(Duration::from_secs(10), replace).await {
        Ok(Ok(OpResult::Job(j))) => j,
        r => panic!("replace of identify: {r:?}"),
    };
    assert_eq!(state(&node, &id), JobState::Stopped);
    assert_ne!(state(&node, &child), JobState::Running);
    // The next child's launch was queued on the gate behind the replace: identify cannot
    // wait for it under that gate, so a task stops it once it is up. That launch takes the
    // lease before the stop: wait for the end state, which a leaked lease never reaches.
    let only_measure = [(measure.0.clone(), "measure".to_string())];
    let holders = || -> Vec<JobId> {
        node.lease_holders(&rx())
            .into_iter()
            .map(|(j, _)| j)
            .collect()
    };
    until(
        "only the measure job runs and holds the radio",
        Duration::from_secs(5),
        || running(&node) == only_measure && holders() == [measure.clone()],
    )
    .await;
    let events = events.await.unwrap();
    assert!(
        events.iter().all(|e| e["outcome"] != "proved"),
        "{events:#?}"
    );
    stop_all(&node).await;
}

/// A source that faults while a child is tried fails identify with the fault, as any
/// job on a faulted source; no child or lease is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fault_fails_identify_with_the_source_fault() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let faulty = Arc::new(Mutex::new(None::<Arc<Faulty>>));
    let keep = faulty.clone();
    let node = node_with(dir.path(), &fixtures().pocsag_on_aprs, move |inner| {
        let p = Arc::new(Faulty {
            inner,
            fault: Mutex::new(None),
        });
        *keep.lock().unwrap() = Some(p.clone());
        p
    })
    .await;
    let port = faulty.lock().unwrap().clone().expect("wrapped");
    let (id, _events) = identify(&node, APRS_HZ).await;
    first_child(&node, "aprs_decode").await;
    *port.fault.lock().unwrap() = Some("device gone".into());
    until("identify ends", Duration::from_secs(10), || {
        state(&node, &id) != JobState::Running
    })
    .await;
    assert_eq!(
        state(&node, &id),
        JobState::Failed {
            reason: "source rx: device gone".into()
        }
    );
    assert!(node.job_context(&id).unwrap()["identify"]["verdict"].is_null());
    until("the child ends", Duration::from_secs(5), || {
        running(&node).is_empty()
    })
    .await;
    until("no lease", Duration::from_secs(5), || {
        node.lease_holders(&rx()).is_empty()
    })
    .await;
}

/// A node killed while identify tries a child leaves both records `running`; on the
/// restart the resumed identify stops that stale child before it measures (here with a
/// source that delivers nothing yet, so a stop after the measurement would come too
/// late), then runs on as usual once samples arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_resumed_after_a_restart_stops_its_stale_child() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let jobs = dir.path().join("state").join("jobs");
    let file = |id: &JobId| jobs.join(format!("{}.json", id.0));
    let node = node_over(dir.path(), &fixtures().pocsag_on_aprs).await;
    let (id, _events) = identify(&node, APRS_HZ).await;
    let (child, _) = first_child(&node, "aprs_decode").await;
    // The records as a kill mid-dwell leaves them: both running, identify naming its child.
    let identify_rec = std::fs::read_to_string(file(&id)).unwrap();
    let child_rec = std::fs::read_to_string(file(&child)).unwrap();
    let v: Value = serde_json::from_str(&identify_rec).unwrap();
    assert_eq!(v["context"]["identify"]["current"], child.0.as_str(), "{v}");
    assert_eq!(v["state"]["status"], "running", "{v}");
    let v: Value = serde_json::from_str(&child_rec).unwrap();
    assert_eq!(v["state"]["status"], "running", "{v}");
    drop(node);
    tokio::time::sleep(Duration::from_millis(500)).await;
    std::fs::write(file(&id), &identify_rec).unwrap();
    std::fs::write(file(&child), &child_rec).unwrap();

    let (node, src) = node_unstarted(dir.path(), &fixtures().pocsag_on_aprs, |p| p).await;
    assert_eq!(state(&node, &id), JobState::Running);
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.provenance.job.as_ref() == Some(&id) && s.kind == StreamKind::Event)
        .expect("identify's event stream")
        .id;
    let mut events = node.bus().subscribe(&stream).expect("subscribed");
    // No samples yet: the stale child ends all the same, and its lease with it. Its state
    // turns before its lease is released, so wait for both (a leaked lease never gets there).
    let holders = || -> Vec<JobId> {
        node.lease_holders(&rx())
            .into_iter()
            .map(|(j, _)| j)
            .collect()
    };
    until(
        "the stale child ends and only identify holds the radio",
        Duration::from_secs(3),
        || state(&node, &child) != JobState::Running && holders() == [id.clone()],
    )
    .await;
    assert_eq!(state(&node, &id), JobState::Running);
    // Samples arrive: the run measures and publishes its candidates.
    src.start();
    let first = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match events.recv().await {
                Ok(f) => {
                    // The run's one `origin` event comes ahead of its candidates.
                    if let Payload::Event(v) = f.payload
                        && v["protocol"] != "origin"
                    {
                        return v;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("identify's stream closed: {e}"),
            }
        }
    })
    .await
    .expect("a candidate from the resumed identify");
    assert_eq!(first["protocol"], "candidate", "{first}");
    assert_ne!(state(&node, &child), JobState::Running);
    stop_all(&node).await;
}

/// A licence register's answer: one band-plan row, and one transmitter licensed for
/// 16K0F1D on the band, 12 km away.
struct Register;

/// The licensee [`Register`] names.
const LICENSEE: &str = "Test Paging Pty Ltd";

#[async_trait]
impl KnowledgeProvider for Register {
    fn name(&self) -> &str {
        "register"
    }
    async fn allocations(&self, region: &str, center_hz: Hz, _bandwidth_hz: Hz) -> Vec<Allocation> {
        vec![
            Allocation {
                provider: "register".into(),
                region: region.into(),
                min_hz: Hz(center_hz.0 - 1_000_000),
                max_hz: Hz(center_hz.0 + 1_000_000),
                service: "fixed primary".into(),
                note: None,
                kind: Some("plan".into()),
                ..Allocation::default()
            },
            Allocation {
                provider: "register".into(),
                region: region.into(),
                min_hz: Hz(center_hz.0 - 8_000),
                max_hz: Hz(center_hz.0 + 8_000),
                service: "assignment".into(),
                note: None,
                kind: Some("assignment".into()),
                licensee: Some(LICENSEE.into()),
                site: Some("Mt Coot-tha".into()),
                distance_km: Some(12.3),
                center_hz: Some(center_hz),
                bandwidth_hz: Some(Hz(16_000)),
                role: Some("tx".into()),
                emission: Some("16K0F1D".into()),
                emission_decoded: zeta_core::emission::parse_emission("16K0F1D"),
                ..Allocation::default()
            },
        ]
    }
    async fn signatures(&self, _center_hz: Hz, _bandwidth_hz: Hz) -> Vec<Signature> {
        vec![]
    }
}

/// With a register answering a 16K0F1D transmitter, an unknown verdict carries the nearest
/// assignments beside its features, and a row the designator fits (POCSAG: 12.5 kHz,
/// `fm`/`fsk`) scores the allocation term over what the measurement alone gives it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_verdict_carries_the_nearest_allocations_and_the_score_term() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().gfsk).await;
    node.add_knowledge(Arc::new(Register));
    let (id, events) = identify(&node, APRS_HZ).await;
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.provenance.job.as_ref() == Some(&id) && s.kind == StreamKind::Event)
        .unwrap()
        .id;
    let mut tap = node.bus().subscribe(&stream).unwrap();
    let events = ended(&node, &id, events, Duration::from_secs(60)).await;
    // The verdict event words the register's transmitter as the watch's unknown does.
    let mut last = None;
    while let Ok(f) = tap.try_recv() {
        if let Payload::Event(v) = f.payload {
            last = Some(v);
        }
    }
    let last = last.expect("events");
    let summary = last["summary"].as_str().unwrap();
    assert!(
        summary.contains(
            "; register says 16K0F1D (16 kHz FM data) Test Paging Pty Ltd, Mt Coot-tha, 12 km"
        ),
        "{summary}"
    );
    let ctx = node.job_context(&id).unwrap();
    let verdict = &ctx["identify"]["verdict"];
    assert!(verdict["mode"].is_null(), "{ctx}");
    assert_eq!(verdict["recentred"], true, "{ctx}");
    let allocations = verdict["allocations"].as_array().expect("allocations");
    // The assignment first, then the plan row.
    assert_eq!(allocations.len(), 2, "{ctx}");
    assert_eq!(allocations[0]["licensee"], LICENSEE, "{ctx}");
    assert_eq!(allocations[1]["kind"], "plan", "{ctx}");
    let pocsag = outcome(&events, "pocsag_decode").expect("pocsag_decode considered");
    assert_eq!(pocsag["allocation"]["licensee"], LICENSEE, "{pocsag}");
    // What the measurement alone scores the row.
    let label = verdict["label"].as_str().unwrap();
    let center = verdict["features"]["center_hz"].as_u64().unwrap();
    let modes = node.modes();
    let base = score_rows(&modes, center, BANDWIDTH_HZ, label, &|_| 0.0, &|_| 0.0)
        .into_iter()
        .find(|s| s.mode.row.id.0 == "pocsag_decode")
        .unwrap()
        .score;
    let score = pocsag["score"].as_f64().unwrap() as f32;
    assert!(score >= base + 0.2 - 1e-4, "{score} vs {base}: {pocsag}");
    // A row the designator does not fit (ACARS: 10 kHz, and 16 kHz is over 1.5 times
    // that) has no allocation.
    let acars = outcome(&events, "acars_decode").expect("acars_decode considered");
    assert!(acars["allocation"].is_null(), "{acars}");
}

/// An identify run publishes one `origin` event after its lookup, and its verdict (and the
/// job's context) carry the same answer, scored on the measured band and label.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_identify_run_publishes_one_origin_event_and_its_verdict_carries_origins() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().gfsk).await;
    node.add_knowledge(Arc::new(Register));
    let (id, events) = identify(&node, APRS_HZ).await;
    until("identify ends", Duration::from_secs(60), || {
        state(&node, &id) != JobState::Running
    })
    .await;
    let events = tokio::time::timeout(Duration::from_secs(5), events)
        .await
        .expect("identify's stream closes")
        .unwrap();
    let frames: Vec<&Value> = events
        .iter()
        .filter(|e| e["protocol"] == "origin")
        .collect();
    assert_eq!(frames.len(), 1, "{events:#?}");
    let frame = frames[0];
    let ctx = node.job_context(&id).unwrap();
    let verdict = &ctx["identify"]["verdict"];
    let origins = &verdict["origins"];
    // The measured band and label.
    assert_eq!(
        origins["band"]["center_hz"], verdict["features"]["center_hz"],
        "{ctx}"
    );
    assert_eq!(
        origins["band"]["bandwidth_hz"], verdict["features"]["bandwidth_hz"],
        "{ctx}"
    );
    assert_eq!(
        origins["modulation"],
        serde_json::json!([verdict["label"]]),
        "{ctx}"
    );
    assert_eq!(origins["source"], "rx", "{ctx}");
    assert_eq!(
        origins["providers"],
        serde_json::json!(["learned", "register"]),
        "{ctx}"
    );
    assert_eq!(
        origins["origins"][0]["allocation"]["licensee"], LICENSEE,
        "{ctx}"
    );
    // The event and the context's `origins` are the same answer.
    assert_eq!(frame["origins"], origins["origins"], "{frame}");
    assert_eq!(frame["band"], origins["band"], "{frame}");
    assert_eq!(frame["job"], id.0.as_str(), "{frame}");
    assert_eq!(ctx["origins"]["t_ns"], origins["t_ns"], "{ctx}");
}

/// Identify's band centred 2 kHz above the tone: each trial job starts on the measured
/// centre (the tone), not on the band's, with the band's width.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trial_jobs_start_on_the_measured_centre() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().tone).await;
    let (id, _events) = identify(&node, TONE_HZ + 2_000).await;
    let (child, _) = first_child(&node, "aprs_decode").await;
    let rec = node.job_list().into_iter().find(|r| r.id == child).unwrap();
    assert!(
        rec.tuning.center_hz.0.abs_diff(TONE_HZ) <= 500,
        "child at {} Hz, the tone at {TONE_HZ} Hz",
        rec.tuning.center_hz.0
    );
    assert_eq!(rec.tuning.bandwidth_hz, Hz(BANDWIDTH_HZ));
    node.job_stop(&id).await.unwrap();
    stop_all(&node).await;
}

/// An identify band as wide as the capture: a trial moved to the measured centre (the
/// tone, 2 kHz up) would leave the capture the job holds, so every trial starts on the
/// band's centre and the verdict says it did not re-centre.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trials_stay_on_the_band_when_the_capture_is_no_wider() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().narrow).await;
    let op = Op::JobStart {
        kind: JobKind::Identify,
        source: rx(),
        center_hz: Hz(APRS_HZ),
        bandwidth_hz: Hz(NARROW_RATE_HZ as u64),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        elastic: false,
        replace: vec![],
    };
    let id = match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("identify: {r:?}"),
    };
    until("identify ends", Duration::from_secs(60), || {
        state(&node, &id) != JobState::Running
    })
    .await;
    let ctx = node.job_context(&id).unwrap();
    let verdict = &ctx["identify"]["verdict"];
    assert!(verdict["mode"].is_null(), "{ctx}");
    let measured = verdict["features"]["center_hz"].as_u64().unwrap();
    assert!(
        measured.abs_diff(APRS_HZ + NARROW_TONE_HZ) <= 500,
        "measured {measured}: {ctx}"
    );
    assert_eq!(verdict["recentred"], false, "{ctx}");
    let trials: Vec<_> = node.job_list().into_iter().filter(|r| r.id != id).collect();
    assert!(!trials.is_empty(), "no trial started: {ctx}");
    for t in &trials {
        assert_eq!(
            t.tuning.center_hz,
            Hz(APRS_HZ),
            "{} at {:?}",
            t.kind,
            t.tuning
        );
    }
    stop_all(&node).await;
}

/// Accept, straight into the node's store, a signature of the band `width_hz` wide at
/// `offset_hz` from the centre of `meta`'s recording, from layer 1's measurement of its
/// first second; features only (confidence 0.3), as the node's own proposals are.
async fn accept_signature(
    node: &Node,
    meta: &Path,
    offset_hz: i64,
    width_hz: u64,
) -> zeta_core::signatures::SignatureRecord {
    use zeta_core::classify::{Band, ClassifierSet};
    use zeta_core::signatures::NewSignature;
    let (m, iq) = zeta_core::capture::read_cf32(meta).unwrap();
    let second = &iq[..iq.len().min(m.sample_rate_hz.0 as usize)];
    let mut set = ClassifierSet::default();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        set.register(c);
    }
    let band = Band {
        offset_hz,
        bandwidth_hz: Hz(width_hz),
    };
    let measured = set
        .measure(second, m.sample_rate_hz, m.center_hz, band)
        .await
        .expect("layer 1 measures the band");
    let new = NewSignature {
        name: Some("test link".into()),
        features: measured.features,
        modulation: measured.label.name,
        framing: None,
        crc_frames: 0,
        evidence: "test".into(),
        source: rx(),
        station: None,
        captures: vec![],
        jobs: vec![],
        allocation: None,
        proposed_by: "cli".into(),
    };
    let store = node.signatures();
    let rec = store.propose(new, 1).unwrap();
    store.accept(&rec.id, None, "cli", 2).unwrap()
}

/// An accepted signature of the GFSK signal: identify still tries every row and ends
/// with no mode, but its verdict names the learned match (`label` and `learned`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_reports_a_learned_signal_in_its_unknown_verdict() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().gfsk).await;
    let sig = accept_signature(&node, &fixtures().gfsk, 0, BANDWIDTH_HZ).await;
    let (id, events) = identify(&node, APRS_HZ).await;
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.provenance.job.as_ref() == Some(&id) && s.kind == StreamKind::Event)
        .unwrap()
        .id;
    let mut tap = node.bus().subscribe(&stream).unwrap();
    ended(&node, &id, events, Duration::from_secs(60)).await;
    let ctx = node.job_context(&id).unwrap();
    let verdict = &ctx["identify"]["verdict"];
    assert!(verdict["mode"].is_null(), "{ctx}");
    assert_eq!(verdict["label"], "learned test link", "{ctx}");
    assert_eq!(verdict["learned"]["id"], sig.id.as_str(), "{ctx}");
    assert_eq!(verdict["learned"]["name"], "test link", "{ctx}");
    // A features-only record names the verdict by its match score; the confidence
    // reported is the label's, 0.3 times that score (0.7 to 1.0).
    assert!((sig.confidence - 0.3).abs() < 1e-6, "{sig:?}");
    let confidence = verdict["learned"]["confidence"].as_f64().unwrap();
    assert!((0.21 - 1e-4..=0.3 + 1e-4).contains(&confidence), "{ctx}");
    assert!(verdict["features"]["bandwidth_hz"].is_u64(), "{ctx}");
    // The verdict event words it as the watch does.
    let mut last = None;
    while let Ok(f) = tap.try_recv() {
        if let Payload::Event(v) = f.payload {
            last = Some(v);
        }
    }
    let last = last.expect("events");
    let summary = last["summary"].as_str().unwrap();
    assert!(summary.starts_with("learned test link at "), "{summary}");
    assert_eq!(last["label"], "learned test link", "{last}");
    // The match is counted.
    let after = node.signatures().get(&sig.id).unwrap();
    assert!(after.matches >= 1, "{after:?}");
    assert!(running(&node).is_empty(), "{:?}", running(&node));
}

/// An unproven sonde-like signal at 403 MHz (an operator's sonde watch's band): identify
/// tries the sonde rows and the rows found anywhere, but never starts FLEX (pagers on
/// 151.8-153.4 MHz), AIS (162 MHz) or APRS (144.39-145.175 MHz); each is skipped as out of its range.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_on_the_sonde_band_never_tries_the_pager_ais_or_aprs_rows() {
    let _one = SERIAL.lock().await;
    PROVE_RS41.store(false, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(node_over(dir.path(), &fixtures().sonde).await);
    let (id, events) = identify(&node, SONDE_HZ).await;
    // Every kind the run starts as a child, sampled while it runs.
    let seen = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let sampler = {
        let (node, seen) = (node.clone(), seen.clone());
        tokio::spawn(async move {
            loop {
                for (_, kind) in running(&node) {
                    seen.lock().unwrap().insert(kind);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };
    let events = ended(&node, &id, events, Duration::from_secs(150)).await;
    sampler.abort();
    let seen = seen.lock().unwrap().clone();
    for m in NOT_ON_THE_SONDE_BAND {
        assert!(!seen.contains(m), "{m} ran on the sonde band: {seen:?}");
        if let Some(e) = outcome(&events, m) {
            assert_eq!(e["outcome"], "skipped_band", "{e}");
            assert!(e["job"].is_null(), "{e}");
            let evidence = e["evidence"].as_str().unwrap_or_default();
            assert!(evidence.contains("outside the row's range"), "{e}");
        }
    }
    // The sonde rows are tried there.
    for m in ["rs41_decode", "m20_decode", "dfm_decode"] {
        let e = outcome(&events, m).unwrap_or_else(|| panic!("{m}: {events:#?}"));
        assert_eq!(e["outcome"], "no_frame", "{e}");
    }
    assert!(running(&node).is_empty(), "{:?}", running(&node));
}

/// The same band with the RS41 row answering: it is tried first and proves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_on_the_sonde_band_proves_the_rs41_row() {
    let _one = SERIAL.lock().await;
    PROVE_RS41.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().sonde).await;
    let (id, events) = identify(&node, SONDE_HZ).await;
    let events = ended(&node, &id, events, Duration::from_secs(30)).await;
    PROVE_RS41.store(false, Ordering::SeqCst);
    let tried: Vec<&Value> = events.iter().filter(|e| !e["job"].is_null()).collect();
    assert_eq!(tried.len(), 1, "{events:#?}");
    assert_eq!(tried[0]["mode"], "rs41_decode");
    assert_eq!(tried[0]["outcome"], "proved");
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["identify"]["verdict"]["mode"], "rs41_decode", "{ctx}");
    stop_all(&node).await;
}

/// The video row needs 6 MHz at least: a 200 kHz box skips it, never starts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identify_skips_video_decode_on_a_narrow_band() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &fixtures().pocsag_on_aprs).await;
    let (id, events) = identify(&node, APRS_HZ).await;
    let events = ended(&node, &id, events, Duration::from_secs(60)).await;
    let video = outcome(&events, "video_decode").expect("video_decode listed");
    assert_eq!(video["outcome"], "skipped_band", "{video}");
    assert!(video["job"].is_null(), "{video}");
    stop_all(&node).await;
}
