//! Learned knowledge: an accepted signature is listed by `knowledge_lookup` (tier
//! `learned`) and scored by the layer-2 `learned` classifier on a later capture of the
//! same signal; a proposed or rejected one is not, and a different signal on the band
//! scores under the floor. Run over captures of synthesised 2-FSK at 4800 Bd in a
//! 20 kHz band at 433.92 MHz, whose feature record is layer 1's own measurement.
//!
//! Accepting a framed signature writes its `learned_<id>` row into the operator's modes
//! directory and reloads the rows (run over the `framing` synthesiser family's capture
//! as a file source); a proved built-in row (POCSAG, by `capture_classify` and by
//! `identify`) supersedes an accepted signature on its band, a learned row never does.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use num_complex::Complex;
use serde_json::{Value, json};
use zeta_core::bursts::{CaptureRecord, NewCapture};
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::classify::{Band, ClassifierSet, FeatureRecord, Label, Outcome};
use zeta_core::job::JobState;
use zeta_core::knowledge::StaticKnowledge;
use zeta_core::modes::{DecoderKind, ModeState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, ModeInfo, Op, OpResult};
use zeta_core::port::Port;
use zeta_core::signatures::{CONFIDENCE_FEATURES, NewSignature, SignatureRecord, SignatureState};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Baud, Hz, JobId, SourceId, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource};

const RATE_HZ: u32 = 48_000;
const CENTER_HZ: u64 = 433_920_000;
const WIDTH_HZ: u64 = 20_000;
/// The PSK capture's rate and band: a 100 kHz signal on the same centre.
const WIDE_RATE_HZ: u32 = 250_000;
const WIDE_WIDTH_HZ: u64 = 100_000;

/// A node with zeta-dsp's classifiers and the built-in seed signatures (the learned
/// provider and classifier are the node's own).
async fn open(dir: &Path) -> Node {
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
        source_positions: Default::default(),
        sets_dir: None,
    };
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut registry);
    let node = Node::open(cfg, registry, vec![]).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    node.add_knowledge(Arc::new(StaticKnowledge::builtin()));
    node
}

/// A dense train of 2-FSK packets at 4800 Bd (50 ms on, 10 ms off) over a 20 dB noise
/// floor; `seed` picks the noise, so two windows of the same signal differ.
fn fsk_iq(seed: u64) -> Vec<Complex<f32>> {
    let spec = zeta_dsp::synth::SynthSpec::Fsk {
        symbol_rate_baud: 4_800,
        deviation_hz: 2_400.0,
        order: 2,
        bits: "14ed58c6d3ff2d75a5650138a8d447f00ec47321d420c5d08bec3850735c".into(),
        repeat: 16,
        gap_ms: 10,
    };
    let mut iq = zeta_dsp::synth::synthesise(&spec, &Default::default(), RATE_HZ, CENTER_HZ);
    zeta_dsp::synth::awgn(&mut iq, 20.0, seed);
    iq
}

/// Continuous QPSK at 64 kBd (about 86 kHz occupied) at [`WIDE_RATE_HZ`].
fn psk_iq() -> Vec<Complex<f32>> {
    let spec = zeta_dsp::synth::SynthSpec::Psk {
        symbol_rate_baud: 64_000,
        order: 4,
        bits: "pattern:14ed58c6d3ff2d75a5650138a8d447f00ec47321d420c5d08bec3850735c".into(),
        repeat: 1,
        gap_ms: 0,
    };
    let mut iq = zeta_dsp::synth::synthesise(&spec, &Default::default(), WIDE_RATE_HZ, CENTER_HZ);
    zeta_dsp::synth::awgn(&mut iq, 20.0, 5);
    iq
}

/// Layer 1's measurement of `iq` (sampled at `rate_hz`, centred on the band) over a
/// band `width_hz` wide.
async fn measured(node: &Node, iq: &[Complex<f32>], rate_hz: u32, width_hz: u64) -> FeatureRecord {
    let mut set = ClassifierSet::default();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        set.register(c);
    }
    let band = Band {
        offset_hz: 0,
        bandwidth_hz: Hz(width_hz),
    };
    set.measure(iq, Hz(rate_hz as u64), Hz(CENTER_HZ), band)
        .await
        .expect("layer 1 measures the band")
        .features
}

/// Write one window of `iq` by the job `set`, its feature record layer 1's measurement.
async fn write(
    node: &Node,
    set: &str,
    iq: &[Complex<f32>],
    rate_hz: u32,
    width_hz: u64,
) -> CaptureRecord {
    let features = measured(node, iq, rate_hz, width_hz).await;
    let new = NewCapture {
        source: SourceId("rx".into()),
        set: JobId(set.into()),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(width_hz),
        sample_rate_hz: Hz(rate_hz as u64),
        start_ns: 1,
        end_ns: 1_000_000_001,
        features: serde_json::to_value(features).unwrap(),
        signal: None,
        job: JobId(set.into()),
        watch: None,
        allocations: serde_json::json!([]),
    };
    node.bursts().write(new, iq).unwrap()
}

async fn propose(node: &Node, capture: &str) -> SignatureRecord {
    let op = Op::SignaturePropose {
        capture: capture.into(),
        name: Some("test link".into()),
        framing: None,
    };
    match node.apply_from(op, "cli").await.unwrap() {
        OpResult::Signature(rec) => *rec,
        r => panic!("signature: {r:?}"),
    }
}

async fn accept(node: &Node, id: &str) -> SignatureRecord {
    let op = Op::SignatureAccept {
        id: id.into(),
        name: None,
    };
    match node.apply_from(op, "cli").await.unwrap() {
        OpResult::Signature(rec) => *rec,
        r => panic!("signature: {r:?}"),
    }
}

/// The `learned` labels `capture_classify` gives the capture `id`.
async fn learned(node: &Node, id: &str) -> Vec<Label> {
    let op = Op::CaptureClassify { capture: id.into() };
    match node.apply(op).await.unwrap() {
        OpResult::Classification { labels, .. } => labels
            .into_iter()
            .filter(|l| l.layer == 2 && l.name.starts_with("learned "))
            .collect(),
        r => panic!("classification: {r:?}"),
    }
}

#[tokio::test]
async fn an_accepted_signature_labels_a_later_capture_learned() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let first = write(&node, "01FIRST", &fsk_iq(41), RATE_HZ, WIDTH_HZ).await;
    let rec = propose(&node, &first.id).await;
    let rec = accept(&node, &rec.id).await;
    assert_eq!(rec.matches, 0);
    let later = write(&node, "01LATER", &fsk_iq(42), RATE_HZ, WIDTH_HZ).await;
    let labels = learned(&node, &later.id).await;
    assert_eq!(labels.len(), 1, "{labels:?}");
    let l = &labels[0];
    assert_eq!(l.name, "learned test link", "{l:?}");
    // The confidence is the record's (features only) times the score.
    let score = l.confidence / CONFIDENCE_FEATURES;
    assert!((0.7 - 1e-4..=1.0 + 1e-4).contains(&score), "{l:?}");
    assert!(
        (l.confidence - CONFIDENCE_FEATURES * score).abs() < 0.01,
        "{l:?}"
    );
    assert!(
        l.evidence.starts_with(&format!("learned {}:", rec.id)),
        "{l:?}"
    );
    assert!(l.evidence.contains("centre"), "{l:?}");
    let after = node.signatures().get(&rec.id).unwrap();
    assert_eq!(after.matches, 1, "{after:?}");
    assert!(after.last_matched_ns.is_some(), "{after:?}");
}

#[tokio::test]
async fn a_proposed_or_rejected_signature_is_not_scored() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let first = write(&node, "01FIRST", &fsk_iq(41), RATE_HZ, WIDTH_HZ).await;
    let rec = propose(&node, &first.id).await;
    let later = write(&node, "01LATER", &fsk_iq(42), RATE_HZ, WIDTH_HZ).await;
    assert!(learned(&node, &later.id).await.is_empty());
    node.apply(Op::SignatureReject { id: rec.id.clone() })
        .await
        .unwrap();
    assert!(learned(&node, &later.id).await.is_empty());
    let after = node.signatures().get(&rec.id).unwrap();
    assert_eq!(after.matches, 0, "{after:?}");
    assert_eq!(after.last_matched_ns, None, "{after:?}");
}

#[tokio::test]
async fn knowledge_lookup_lists_learned_signatures_with_their_tier() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let first = write(&node, "01FIRST", &fsk_iq(41), RATE_HZ, WIDTH_HZ).await;
    let rec = propose(&node, &first.id).await;
    let lookup = |center_hz: u64| Op::KnowledgeLookup {
        region: "AU".into(),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(12_500),
    };
    let signatures = |r: OpResult| match r {
        OpResult::Knowledge { signatures, .. } => signatures,
        r => panic!("knowledge: {r:?}"),
    };
    // A proposal is not knowledge yet.
    let before = signatures(node.apply(lookup(CENTER_HZ)).await.unwrap());
    assert!(
        before.iter().all(|s| s.tier.as_deref() != Some("learned")),
        "{before:?}"
    );
    accept(&node, &rec.id).await;
    let sigs = signatures(node.apply(lookup(CENTER_HZ)).await.unwrap());
    let learned: Vec<_> = sigs
        .iter()
        .filter(|s| s.tier.as_deref() == Some("learned"))
        .collect();
    assert_eq!(learned.len(), 1, "{sigs:?}");
    let s = learned[0];
    assert_eq!(s.name, "test link");
    assert_eq!((s.min_hz, s.max_hz), (rec.min_hz, rec.max_hz));
    assert_eq!(s.bandwidth_hz, rec.bandwidth_hz);
    assert_eq!(s.modulation, rec.modulation);
    assert_eq!(s.symbol_rate_baud, rec.symbol_rate_baud);
    assert_eq!(s.order, rec.order);
    assert_eq!(s.burst.as_ref(), Some(&rec.burst));
    assert_eq!(s.confidence, Some(rec.confidence));
    // A features-only record has no decoder and no framing.
    assert_eq!(s.decoder, None);
    assert_eq!((&s.sync, &s.encoding, &s.crc), (&None, &None, &None));
    // Serialised, every field is written.
    let v = serde_json::to_value(s).unwrap();
    for k in ["sync", "encoding", "crc", "tier", "confidence", "burst"] {
        assert!(v.get(k).is_some(), "{k}: {v}");
    }
    // Off the band: not listed.
    let far = signatures(node.apply(lookup(CENTER_HZ + 1_000_000)).await.unwrap());
    assert!(
        far.iter().all(|s| s.tier.as_deref() != Some("learned")),
        "{far:?}"
    );
    // The seed still loads, with no tier.
    let aprs = signatures(node.apply(lookup(144_800_000)).await.unwrap());
    let aprs = aprs.iter().find(|s| s.name == "APRS").expect("APRS");
    assert_eq!(aprs.tier, None);
    assert_eq!(aprs.confidence, None);
    assert_eq!(aprs.burst, None);
}

#[tokio::test]
async fn a_different_signal_on_the_band_scores_under_the_floor() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let first = write(&node, "01FIRST", &fsk_iq(41), RATE_HZ, WIDTH_HZ).await;
    let rec = propose(&node, &first.id).await;
    accept(&node, &rec.id).await;
    let psk = write(&node, "01PSK", &psk_iq(), WIDE_RATE_HZ, WIDE_WIDTH_HZ).await;
    // On the centre, but well over 1.5 times the signature's width.
    let width = psk.features["bandwidth_hz"].as_u64().unwrap();
    assert!(
        width as f64 > 1.5 * rec.bandwidth_hz.0 as f64,
        "{} vs {rec:?}",
        psk.features
    );
    let labels = learned(&node, &psk.id).await;
    assert!(labels.is_empty(), "{labels:?}");
    assert_eq!(node.signatures().get(&rec.id).unwrap().matches, 0);
}

/// The real-time file-source tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The framing fixture's symbol rate, and the width of the bands signatures are on.
const BAUD: u32 = 4_800;
const BAND_HZ: u64 = 12_500;
/// A well-known POCSAG centre, and the rate of the POCSAG file source.
const POCSAG_HZ: u64 = 148_637_500;
const POCSAG_RATE_HZ: u32 = 2_048_000;

/// The framing of the `framing` fixture: a 16-bit sync, a fixed 272-bit frame, a
/// CRC-16/CCITT.
fn framing() -> Value {
    json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}, "crc": "crc16-ccitt"})
}

/// `iq` as a cf32 SigMF pair `<stem>.sigmf-*` in `dir`, sampled at `rate_hz` and
/// centred at `center_hz`.
fn write_file(
    dir: &Path,
    stem: &str,
    iq: &[Complex<f32>],
    rate_hz: u32,
    center_hz: u64,
) -> PathBuf {
    let meta = CaptureMeta {
        sample_rate_hz: Hz(rate_hz as u64),
        center_hz: Hz(center_hz),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    write_cf32(dir, stem, &meta, iq).expect("write fixture")
}

/// The files, written once: the `framing` family's capture (8 frames of 32
/// pseudo-random bytes at 4800 Bd fsk, 2400 Hz deviation, 20 dB) at 48 kHz on
/// [`CENTER_HZ`], and the 1200-baud POCSAG page at 2.048 MHz on [`POCSAG_HZ`].
struct Files {
    _dir: tempfile::TempDir,
    frames: PathBuf,
    pocsag: PathBuf,
}

fn files() -> &'static Files {
    static F: OnceLock<Files> = OnceLock::new();
    F.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let params = json!({"family": "framing", "modulation": "fsk", "symbol_rate_baud": BAUD,
            "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32, "seed": 7,
            "framing": framing(), "repeat": 1, "snr_db": 20.0});
        let (spec, imp) =
            zeta_dsp::synth::parse_params(params.as_object().unwrap().clone()).unwrap();
        let iq = zeta_dsp::synth::synthesise(&spec, &imp, RATE_HZ, CENTER_HZ);
        let frames = write_file(dir.path(), "frames", &iq, RATE_HZ, CENTER_HZ);
        let iq = zeta_dsp::synth::pocsag_packet_iq(
            1_234_567,
            3,
            "HELLO ZETA",
            1200,
            POCSAG_RATE_HZ,
            0.0,
        );
        let pocsag = write_file(dir.path(), "pocsag", &iq, POCSAG_RATE_HZ, POCSAG_HZ);
        Files {
            _dir: dir,
            frames,
            pocsag,
        }
    })
}

/// A node with `modes_dir` as its operator directory, over a looping real-time file
/// source `rx` of `file` (none for `None`), with zeta-dsp's classifiers.
async fn open_in(dir: &Path, modes_dir: &Path, file: Option<&Path>) -> Node {
    let cfg = NodeConfig {
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 4.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: 20,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: Some(modes_dir.to_path_buf()),
        source_positions: Default::default(),
        sets_dir: None,
    };
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut registry);
    let src = file.map(|f| {
        let opts = FileOpts {
            realtime: true,
            block_samples: 2_400,
            repeat: true,
            tune_range: None,
        };
        Arc::new(FileSource::open(SourceId("rx".into()), f, opts).unwrap())
    });
    let mut ports: Vec<Arc<dyn Port>> = Vec::new();
    if let Some(s) = &src {
        ports.push(s.clone());
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

/// A feature record of a `width_hz` band at `center_hz`, 2-level at `baud` (none for
/// `None`).
fn features(center_hz: u64, width_hz: u64, baud: Option<u32>) -> FeatureRecord {
    FeatureRecord {
        bandwidth_hz: Hz(width_hz),
        center_hz: Hz(center_hz),
        symbol_rate_baud: baud.map(Baud),
        snr_db: 20.0,
        burst: true,
        duty: None,
        bursts: None,
        burst_ms: None,
        period_ms: None,
        order: baud.map(|_| 2),
        tone_spacing_hz: None,
        symbol_rate_line: None,
    }
}

/// Propose `features` (an `fsk` signal) by hand, straight into the node's store, with
/// `framing` and `captures`.
fn propose_by_hand(
    node: &Node,
    features: FeatureRecord,
    framing: Option<Value>,
    captures: Vec<String>,
) -> SignatureRecord {
    let new = NewSignature {
        name: Some("test link".into()),
        features,
        modulation: "fsk".into(),
        framing,
        crc_frames: 0,
        evidence: "by hand".into(),
        source: SourceId("rx".into()),
        station: None,
        captures,
        jobs: vec![],
        allocation: None,
        proposed_by: "cli".into(),
    };
    node.signatures().propose(new, 1).unwrap()
}

/// A framed signature of the framing fixture's band, proposed by hand.
fn propose_framed(node: &Node, captures: Vec<String>) -> SignatureRecord {
    let f = features(CENTER_HZ, BAND_HZ, Some(BAUD));
    propose_by_hand(node, f, Some(framing()), captures)
}

/// The row id a signature's row is written under.
fn row_id(rec: &SignatureRecord) -> String {
    format!("learned_{}", rec.id.to_lowercase())
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn job_state(node: &Node, id: &JobId) -> JobState {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
        .state
}

async fn stop_all(node: &Node) {
    for r in node.job_list() {
        if r.state == JobState::Running {
            node.job_stop(&r.id).await.unwrap();
        }
    }
}

/// Start `kind` on `rx` at `center_hz`, [`BAND_HZ`] wide.
async fn start(node: &Node, kind: JobKind, center_hz: u64) -> JobId {
    let op = Op::JobStart {
        kind,
        source: SourceId("rx".into()),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(BAND_HZ),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start: {r:?}"),
    }
}

/// The `modes_list` entry of `id`.
async fn listed(node: &Node, id: &str) -> Option<ModeInfo> {
    let op = Op::ModesList {
        family: None,
        state: None,
    };
    match node.apply(op).await.unwrap() {
        OpResult::Modes { modes } => modes.into_iter().find(|m| m.id.0 == id),
        r => panic!("modes: {r:?}"),
    }
}

/// The first event of the job `id`'s last event output that is not forwarded `bits`.
async fn first_frame(node: &Node, id: &JobId) -> Value {
    let events = || -> Vec<_> {
        node.job_outputs(id)
            .into_iter()
            .filter(|(k, _)| *k == StreamKind::Event)
            .map(|(_, s)| s)
            .collect()
    };
    until("the job's event outputs", Duration::from_secs(5), || {
        !events().is_empty()
    })
    .await;
    let stream = events().pop().expect("the job's frame output");
    let mut rx = node.bus().subscribe(&stream).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(f) => {
                    if let Payload::Event(v) = f.payload
                        && v["protocol"] != "bits"
                    {
                        return v;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("stream closed: {e}"),
            }
        }
    })
    .await
    .expect("a frame within 10 s")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepting_a_framed_signature_writes_the_learned_row_and_it_decodes() {
    let _one = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let modes_dir = dir.path().join("modes");
    let node = open_in(dir.path(), &modes_dir, Some(&files().frames)).await;
    let rec = propose_framed(&node, vec![]);
    let rec = accept(&node, &rec.id).await;
    let id = row_id(&rec);
    let path = modes_dir.join(format!("{id}.json"));
    assert_eq!(rec.state, SignatureState::Accepted);
    assert_eq!(rec.decoder.as_deref(), Some(id.as_str()), "{rec:?}");
    let file = path.display().to_string();
    assert_eq!(rec.decoder_file.as_deref(), Some(file.as_str()), "{rec:?}");
    assert_eq!(rec.decoder_note, None, "{rec:?}");
    assert_eq!(node.signatures().get(&rec.id).unwrap(), rec);
    // The written row is the generated one.
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["id"], id.as_str());
    assert_eq!(written["name"], "test link");
    assert_eq!(written["protocol"], "frame");
    assert_eq!(written["decoder"]["framing"], framing());
    assert_eq!(written["decoder"]["parser"], "opaque");
    assert_eq!(written["band"]["centers_hz"], json!([CENTER_HZ]));
    assert_eq!(written["band"]["bandwidth_hz"], BAND_HZ);
    assert_eq!(written["modulation"], json!(["fsk", "fm"]));
    let notes = format!(
        "generated from signature {}; edit freely (a parser, a subject); not overwritten",
        rec.id
    );
    assert_eq!(written["notes"], notes.as_str());
    // It is listed ready at once, and decodes the signal.
    let info = listed(&node, &id).await.expect("listed");
    assert_eq!(info.state, ModeState::Ready, "{info:?}");
    assert_eq!(info.decoder_kind, DecoderKind::Frame);
    let job = start(&node, JobKind::from_name(&id).unwrap(), CENTER_HZ).await;
    let first = first_frame(&node, &job).await;
    assert_eq!(first["protocol"], "frame", "{first}");
    assert_eq!(first["crc_ok"], true, "{first}");
    assert_eq!(job_state(&node, &job), JobState::Running);
    stop_all(&node).await;
}

#[tokio::test]
async fn an_existing_learned_row_is_kept_as_is() {
    let dir = tempfile::tempdir().unwrap();
    let modes_dir = dir.path().join("modes");
    let node = open_in(dir.path(), &modes_dir, None).await;
    let rec = propose_framed(&node, vec![]);
    let id = row_id(&rec);
    let path = modes_dir.join(format!("{id}.json"));
    let mut row = serde_json::to_value(zeta_core::signatures::learned_row(&rec, &id)).unwrap();
    row["name"] = "edited".into();
    std::fs::create_dir_all(&modes_dir).unwrap();
    std::fs::write(&path, serde_json::to_string_pretty(&row).unwrap()).unwrap();
    let rec = accept(&node, &rec.id).await;
    assert_eq!(rec.state, SignatureState::Accepted);
    assert_eq!(rec.decoder.as_deref(), Some(id.as_str()), "{rec:?}");
    let file = path.display().to_string();
    assert_eq!(rec.decoder_file.as_deref(), Some(file.as_str()), "{rec:?}");
    let note = format!("row {id} already present: kept as is");
    assert_eq!(rec.decoder_note.as_deref(), Some(note.as_str()), "{rec:?}");
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(kept["name"], "edited");
    // The operator's file, written after `open`, is listed at once.
    let info = listed(&node, &id).await.expect("listed before a restart");
    assert_eq!(info.state, ModeState::Ready, "{info:?}");
    assert_eq!(info.name, "edited");
    // After a restart the operator's row still loads.
    drop(node);
    let node = open_in(dir.path(), &modes_dir, None).await;
    let m = node.modes().get(&id).cloned().expect("the row loads");
    assert_eq!(m.state, ModeState::Ready, "{m:?}");
    assert_eq!(m.row.name, "edited");
}

#[tokio::test]
async fn accept_without_a_writable_modes_directory_accepts_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    std::fs::write(&plain, b"a file, not a directory").unwrap();
    let node = open_in(dir.path(), &plain.join("modes"), None).await;
    let cap = write(&node, "01SET", &fsk_iq(41), RATE_HZ, WIDTH_HZ).await;
    let rec = propose_framed(&node, vec![cap.id.clone()]);
    let rec = accept(&node, &rec.id).await;
    assert_eq!(rec.state, SignatureState::Accepted);
    assert_eq!((&rec.decoder, &rec.decoder_file), (&None, &None), "{rec:?}");
    let note = rec.decoder_note.clone().unwrap_or_default();
    let error = note.strip_prefix("row not written: ").unwrap_or_default();
    assert!(!error.is_empty(), "{rec:?}");
    assert!(node.bursts().get(&cap.id).unwrap().pinned, "{rec:?}");
    assert_eq!(node.signatures().get(&rec.id).unwrap(), rec);
}

/// `modes_dir: None` is no operator directory: a framed accept writes nothing (never
/// under `HOME` or `XDG_CONFIG_HOME`) and says so.
#[tokio::test]
async fn a_node_without_a_modes_dir_writes_no_learned_row() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let rec = propose_framed(&node, vec![]);
    let rec = accept(&node, &rec.id).await;
    assert_eq!(rec.state, SignatureState::Accepted);
    assert_eq!((&rec.decoder, &rec.decoder_file), (&None, &None), "{rec:?}");
    let note = rec.decoder_note.as_deref();
    assert_eq!(note, Some("no modes directory: row not written"), "{rec:?}");
    assert!(listed(&node, &row_id(&rec)).await.is_none());
    // A reload reads the embedded rows only, as `open` does.
    let before = node.modes().ids();
    assert!(node.reload_modes().unwrap().is_empty());
    assert_eq!(node.modes().ids(), before);
}

#[tokio::test]
async fn a_features_only_signature_writes_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let modes_dir = dir.path().join("modes");
    let node = open_in(dir.path(), &modes_dir, None).await;
    let first = write(&node, "01FIRST", &fsk_iq(41), RATE_HZ, WIDTH_HZ).await;
    let rec = propose(&node, &first.id).await;
    let rec = accept(&node, &rec.id).await;
    assert_eq!(rec.state, SignatureState::Accepted);
    let decoder = (&rec.decoder, &rec.decoder_file, &rec.decoder_note);
    assert_eq!(decoder, (&None, &None, &None), "{rec:?}");
    assert!(!modes_dir.exists(), "nothing written");
}

/// Two seconds of the POCSAG page at 48 kHz over a noise floor.
fn pocsag_iq() -> Vec<Complex<f32>> {
    let mut iq = zeta_dsp::synth::pocsag_packet_iq(1_234_567, 3, "HELLO ZETA", 1200, RATE_HZ, 0.0);
    iq.resize(2 * RATE_HZ as usize, Complex::new(0.0, 0.0));
    zeta_dsp::synth::awgn(&mut iq, 20.0, 3);
    iq
}

#[tokio::test]
async fn a_proved_classify_supersedes_an_overlapping_learned_signature() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_in(dir.path(), &dir.path().join("modes"), None).await;
    let iq = pocsag_iq();
    let measured = {
        let mut set = ClassifierSet::default();
        for c in zeta_dsp::classifiers(node.modes_handle()) {
            set.register(c);
        }
        let band = Band {
            offset_hz: 0,
            bandwidth_hz: Hz(BAND_HZ),
        };
        set.measure(&iq, Hz(RATE_HZ as u64), Hz(POCSAG_HZ), band)
            .await
            .expect("layer 1 measures the band")
            .features
    };
    let new = NewCapture {
        source: SourceId("rx".into()),
        set: JobId("01PAGE".into()),
        center_hz: Hz(POCSAG_HZ),
        bandwidth_hz: Hz(BAND_HZ),
        sample_rate_hz: Hz(RATE_HZ as u64),
        start_ns: 1,
        end_ns: 2_000_000_001,
        features: serde_json::to_value(&measured).unwrap(),
        signal: None,
        job: JobId("01PAGE".into()),
        watch: None,
        allocations: json!([]),
    };
    let cap = node.bursts().write(new, &iq).unwrap();
    // A features-only signature of the page on 148.6375 MHz, 12.5 kHz wide.
    let f = FeatureRecord {
        center_hz: Hz(POCSAG_HZ),
        bandwidth_hz: Hz(BAND_HZ),
        ..measured
    };
    let rec = propose_by_hand(&node, f, None, vec![cap.id.clone()]);
    let rec = accept(&node, &rec.id).await;
    assert_eq!(rec.decoder, None);
    let op = Op::CaptureClassify {
        capture: cap.id.clone(),
    };
    let OpResult::Classification { tried, .. } = node.apply(op).await.unwrap() else {
        panic!("classification")
    };
    let pocsag = tried.iter().find(|t| t.mode == "pocsag_decode");
    assert_eq!(
        pocsag.map(|t| t.outcome),
        Some(Outcome::Proved),
        "{tried:?}"
    );
    let after = node.signatures().get(&rec.id).unwrap();
    assert_eq!(after.state, SignatureState::Superseded, "{after:?}");
    let by = after.provenance.superseded_by.as_deref();
    assert_eq!(by, Some("pocsag_decode"), "{after:?}");
    assert!(after.provenance.superseded_ns.is_some(), "{after:?}");
    assert!(node.bursts().get(&cap.id).unwrap().pinned, "still pinned");
    // The scorer no longer labels the signal.
    assert!(node.signatures().accepted().is_empty());
    let labels = learned(&node, &cap.id).await;
    assert!(labels.is_empty(), "{labels:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_identify_proof_supersedes_and_a_learned_row_never_does() {
    let _one = SERIAL.lock().await;
    {
        let dir = tempfile::tempdir().unwrap();
        let modes_dir = dir.path().join("modes");
        let node = open_in(dir.path(), &modes_dir, Some(&files().pocsag)).await;
        let f = features(POCSAG_HZ, BAND_HZ, None);
        let rec = propose_by_hand(&node, f, None, vec![]);
        let rec = accept(&node, &rec.id).await;
        let id = start(&node, JobKind::Identify, POCSAG_HZ).await;
        until("identify ends", Duration::from_secs(40), || {
            job_state(&node, &id) != JobState::Running
        })
        .await;
        let ctx = node.job_context(&id).unwrap();
        assert_eq!(ctx["identify"]["verdict"]["mode"], "pocsag_decode", "{ctx}");
        until("the signature superseded", Duration::from_secs(5), || {
            node.signatures().get(&rec.id).unwrap().state == SignatureState::Superseded
        })
        .await;
        let after = node.signatures().get(&rec.id).unwrap();
        let by = after.provenance.superseded_by.as_deref();
        assert_eq!(by, Some("pocsag_decode"), "{after:?}");
        stop_all(&node).await;
    }
    // A learned row's frames leave another accepted signature on its band accepted.
    let dir = tempfile::tempdir().unwrap();
    let modes_dir = dir.path().join("modes");
    let node = open_in(dir.path(), &modes_dir, Some(&files().frames)).await;
    let framed = propose_framed(&node, vec![]);
    let framed = accept(&node, &framed.id).await;
    let f = features(CENTER_HZ, BAND_HZ, None);
    let other = propose_by_hand(&node, f, None, vec![]);
    let other = accept(&node, &other.id).await;
    let row = framed.decoder.clone().expect("a learned row");
    let job = start(&node, JobKind::from_name(&row).unwrap(), CENTER_HZ).await;
    assert_eq!(first_frame(&node, &job).await["crc_ok"], true);
    // The job's end drains its watchers: every event they saw is handled by then.
    node.job_stop(&job).await.unwrap();
    until("the job ends", Duration::from_secs(5), || {
        job_state(&node, &job) != JobState::Running
    })
    .await;
    for id in [&framed.id, &other.id] {
        let r = node.signatures().get(id).unwrap();
        assert_eq!(r.state, SignatureState::Accepted, "{r:?}");
        assert_eq!(r.provenance.superseded_by, None, "{r:?}");
    }
}
