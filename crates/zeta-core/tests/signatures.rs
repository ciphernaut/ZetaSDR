//! The learned-signature store over a node: `signature_propose` by hand from a capture,
//! `signature_list`, `signature_show`, `signature_accept` (pinning the captures) and
//! `signature_reject`, and the store's reload.

use std::path::Path;
use std::sync::Arc;

use num_complex::Complex;
use zeta_core::bursts::{CaptureRecord, NewCapture};
use zeta_core::classify::FeatureRecord;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::signatures::{SIGNATURES_DIR, SignatureRecord, SignatureState, SignatureStore};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Baud, Hz, JobId, SourceId};

const RATE_HZ: u32 = 48_000;
const CENTER_HZ: u64 = 433_920_000;
const WIDTH_HZ: u64 = 20_000;

async fn open(dir: &Path) -> Node {
    let cfg = NodeConfig {
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
    node
}

/// The feature record a burst capture of the test link carries.
fn features() -> FeatureRecord {
    FeatureRecord {
        bandwidth_hz: Hz(WIDTH_HZ),
        center_hz: Hz(CENTER_HZ),
        symbol_rate_baud: Some(Baud(4_800)),
        snr_db: 20.0,
        burst: true,
        duty: Some(0.8),
        bursts: Some(16),
        burst_ms: Some(50.0),
        period_ms: Some(60.0),
        order: Some(2),
        tone_spacing_hz: Some(Hz(4_800)),
        symbol_rate_line: None,
    }
}

fn new_capture(set: &str, features: serde_json::Value) -> NewCapture {
    NewCapture {
        source: SourceId("rx".into()),
        set: JobId(set.into()),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(WIDTH_HZ),
        sample_rate_hz: Hz(RATE_HZ as u64),
        start_ns: 1,
        end_ns: 1_000_000_001,
        features,
        signal: None,
        job: JobId(set.into()),
        watch: None,
        allocations: serde_json::json!([]),
    }
}

/// A dense train of 2-FSK packets (layer 1 labels it `fsk`) over a 20 dB noise floor.
fn fsk_iq() -> Vec<Complex<f32>> {
    let spec = zeta_dsp::synth::SynthSpec::Fsk {
        symbol_rate_baud: 4_800,
        deviation_hz: 2_400.0,
        order: 2,
        bits: "14ed58c6d3ff2d75a5650138a8d447f00ec47321d420c5d08bec3850735c".into(),
        repeat: 16,
        gap_ms: 10,
    };
    let mut iq = zeta_dsp::synth::synthesise(&spec, &Default::default(), RATE_HZ, CENTER_HZ);
    zeta_dsp::synth::awgn(&mut iq, 20.0, 41);
    iq
}

/// Two fsk windows written by one job (one set), with the test link's features.
fn write_set(node: &Node, set: &str) -> Vec<CaptureRecord> {
    let f = serde_json::to_value(features()).unwrap();
    (0..2)
        .map(|_| {
            node.bursts()
                .write(new_capture(set, f.clone()), &fsk_iq())
                .unwrap()
        })
        .collect()
}

async fn propose(
    node: &Node,
    capture: &str,
    framing: Option<serde_json::Value>,
) -> Result<SignatureRecord, OpError> {
    match node
        .apply_from(
            Op::SignaturePropose {
                capture: capture.into(),
                name: None,
                framing,
            },
            "cli",
        )
        .await?
    {
        OpResult::Signature(rec) => Ok(*rec),
        r => panic!("signature: {r:?}"),
    }
}

async fn list(node: &Node, state: Option<SignatureState>) -> Vec<SignatureRecord> {
    match node.apply(Op::SignatureList { state }).await.unwrap() {
        OpResult::Signatures(v) => v,
        r => panic!("signatures: {r:?}"),
    }
}

#[tokio::test]
async fn propose_by_hand_from_a_capture_lists_a_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let set = write_set(&node, "01SET");
    let rec = propose(&node, &set[0].id, None).await.unwrap();
    assert_eq!(rec.state, SignatureState::Proposed);
    assert!((rec.confidence - 0.3).abs() < 1e-6, "{}", rec.confidence);
    assert_eq!(rec.name, "fsk 20.0 kHz at 433.920 MHz");
    assert_eq!(rec.modulation, "fsk");
    assert_eq!((rec.min_hz, rec.max_hz), (Hz(433_910_000), Hz(433_930_000)));
    assert_eq!(rec.bandwidth_hz, Hz(WIDTH_HZ));
    assert_eq!(rec.symbol_rate_baud, Some(Baud(4_800)));
    assert_eq!(rec.framing, None);
    let mut captures = rec.provenance.captures.clone();
    captures.sort();
    let mut want: Vec<String> = set.iter().map(|c| c.id.clone()).collect();
    want.sort();
    assert_eq!(captures, want);
    assert_eq!(rec.provenance.jobs, [JobId("01SET".into())]);
    assert_eq!(rec.provenance.source, SourceId("rx".into()));
    assert_eq!(rec.provenance.proposed_by, "cli");
    assert_eq!(rec.provenance.station, None);
    assert_eq!(rec.provenance.allocation, None);
    assert_eq!(
        list(&node, Some(SignatureState::Proposed)).await,
        std::slice::from_ref(&rec)
    );
    assert!(list(&node, Some(SignatureState::Accepted)).await.is_empty());
    let file = dir
        .path()
        .join("state")
        .join(SIGNATURES_DIR)
        .join(format!("{}.json", rec.id));
    assert!(file.is_file(), "{}", file.display());
    // An unknown capture, and one without a feature record, propose nothing.
    let Err(OpError::Unknown { field, .. }) = propose(&node, "nope", None).await else {
        panic!("unknown")
    };
    assert_eq!(field, "capture");
    let bare = node
        .bursts()
        .write(new_capture("01BARE", serde_json::json!({})), &fsk_iq())
        .unwrap();
    assert_eq!(
        propose(&node, &bare.id, None).await.unwrap_err(),
        OpError::Other {
            message: format!("capture {} has no feature record", bare.id)
        }
    );
    assert_eq!(list(&node, None).await.len(), 1);
}

#[tokio::test]
async fn a_framing_lifts_the_confidence_and_a_bad_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let set = write_set(&node, "01SET");
    let framing = serde_json::json!({
        "sync": {"hex": "2dd4", "bits": 16},
        "length": {"fixed_bits": 272},
        "crc": "crc16-ccitt",
    });
    let rec = propose(&node, &set[0].id, Some(framing.clone()))
        .await
        .unwrap();
    assert!((rec.confidence - 0.6).abs() < 1e-6, "{}", rec.confidence);
    assert_eq!(rec.framing, Some(framing));
    let bad =
        serde_json::json!({"sync": {"hex": "2dd4", "bits": 70}, "length": {"fixed_bits": 272}});
    let Err(OpError::Other { message }) = propose(&node, &set[0].id, Some(bad)).await else {
        panic!("refused")
    };
    assert!(message.starts_with("framing: sync.bits"), "{message}");
    assert_eq!(list(&node, None).await.len(), 1);
}

#[tokio::test]
async fn accept_pins_the_captures_and_reject_ends_a_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let set = write_set(&node, "01SET");
    let other = write_set(&node, "01OTHER");
    let first = propose(&node, &set[0].id, None).await.unwrap();
    let second = propose(&node, &other[0].id, None).await.unwrap();
    let accept = Op::SignatureAccept {
        id: first.id.clone(),
        name: Some("test link".into()),
    };
    let OpResult::Signature(rec) = node.apply_from(accept.clone(), "cli").await.unwrap() else {
        panic!("signature")
    };
    assert_eq!(rec.state, SignatureState::Accepted);
    assert_eq!(rec.name, "test link");
    assert_eq!(rec.provenance.accepted_by.as_deref(), Some("cli"));
    assert!(rec.provenance.accepted_ns.is_some());
    let OpResult::Captures(captures) = node.apply(Op::CaptureList { signal: None }).await.unwrap()
    else {
        panic!("captures")
    };
    for id in &rec.provenance.captures {
        let c = captures.iter().find(|c| &c.id == id).expect("listed");
        assert!(c.pinned, "{id} pinned");
    }
    assert!(captures.iter().filter(|c| c.pinned).count() == rec.provenance.captures.len());
    assert_eq!(
        node.apply_from(accept, "cli").await.unwrap_err(),
        OpError::Other {
            message: format!("signature {} is accepted, not proposed", first.id)
        }
    );
    let OpResult::Signature(rejected) = node
        .apply(Op::SignatureReject {
            id: second.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("signature")
    };
    assert_eq!(rejected.state, SignatureState::Rejected);
    let OpResult::Signature(shown) = node
        .apply(Op::SignatureShow {
            id: second.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("signature")
    };
    assert_eq!(shown, rejected);
    let Err(OpError::Unknown {
        field,
        value,
        known,
    }) = node.apply(Op::SignatureShow { id: "nope".into() }).await
    else {
        panic!("unknown")
    };
    assert_eq!((field.as_str(), value.as_str()), ("id", "nope"));
    let mut known = known;
    known.sort();
    let mut ids = vec![first.id.clone(), second.id.clone()];
    ids.sort();
    assert_eq!(known, ids);
}

/// `op` from the CLI transport: the record, or the error.
async fn decided(node: &Node, op: Op) -> Result<SignatureRecord, OpError> {
    match node.apply_from(op, "cli").await? {
        OpResult::Signature(rec) => Ok(*rec),
        r => panic!("signature: {r:?}"),
    }
}

/// Whether each capture of `ids` is pinned.
fn pinned(node: &Node, ids: &[String]) -> Vec<bool> {
    let bursts = node.bursts();
    ids.iter()
        .map(|id| bursts.get(id).unwrap().pinned)
        .collect()
}

#[tokio::test]
async fn retract_and_delete_unpin_the_captures_no_remaining_record_holds() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let set = write_set(&node, "01SET");
    // Two proposals from one set: both list its two captures.
    let a = propose(&node, &set[0].id, None).await.unwrap();
    let b = propose(&node, &set[1].id, None).await.unwrap();
    let c = propose(&node, &set[1].id, None).await.unwrap();
    assert_eq!(a.provenance.captures, b.provenance.captures);
    let caps = a.provenance.captures.clone();
    let accept = |id: &str| Op::SignatureAccept {
        id: id.into(),
        name: None,
    };
    let retract = |id: &str| Op::SignatureRetract { id: id.into() };
    let delete = |id: &str| Op::SignatureDelete { id: id.into() };
    let accepted = decided(&node, accept(&a.id)).await.unwrap();
    decided(&node, accept(&b.id)).await.unwrap();
    assert_eq!(pinned(&node, &caps), [true, true]);

    // Retract: the record stays, retracted, its provenance kept; b still holds the pins.
    let ra = decided(&node, retract(&a.id)).await.unwrap();
    assert_eq!(ra.state, SignatureState::Retracted);
    assert_eq!(ra.provenance.accepted_ns, accepted.provenance.accepted_ns);
    assert_eq!(ra.provenance.accepted_by.as_deref(), Some("cli"));
    assert_eq!(ra.provenance.captures, caps);
    assert!(ra.provenance.retracted_ns.is_some(), "{ra:?}");
    assert_eq!(pinned(&node, &caps), [true, true], "b still holds them");
    assert_eq!(
        list(&node, Some(SignatureState::Retracted)).await,
        std::slice::from_ref(&ra)
    );

    // Retract takes only an accepted record, accept only a proposed one: each names it.
    let refused = |state: &str, id: &str, not: &str| OpError::Other {
        message: format!("signature {id} is {state}, not {not}"),
    };
    let e = decided(&node, retract(&a.id)).await.unwrap_err();
    assert_eq!(e, refused("retracted", &a.id, "accepted"));
    let e = decided(&node, retract(&c.id)).await.unwrap_err();
    assert_eq!(e, refused("proposed", &c.id, "accepted"));
    let e = decided(&node, accept(&a.id)).await.unwrap_err();
    assert_eq!(e, refused("retracted", &a.id, "proposed"));
    for op in [retract("nope"), delete("nope")] {
        let e = decided(&node, op).await.unwrap_err();
        assert!(
            matches!(&e, OpError::Unknown { field, value, .. } if field == "id" && value == "nope"),
            "{e:?}"
        );
    }

    // Delete: the record and its file go; with a retracted, nothing holds the pins now.
    let file = |id: &str| {
        dir.path()
            .join("state")
            .join(SIGNATURES_DIR)
            .join(format!("{id}.json"))
    };
    assert!(file(&b.id).is_file());
    let rb = decided(&node, delete(&b.id)).await.unwrap();
    assert_eq!(
        (rb.id.as_str(), rb.state),
        (b.id.as_str(), SignatureState::Accepted)
    );
    assert!(!file(&b.id).exists());
    assert_eq!(pinned(&node, &caps), [false, false]);
    // A record in any state deletes: the retracted one and the proposal.
    decided(&node, delete(&a.id)).await.unwrap();
    decided(&node, delete(&c.id)).await.unwrap();
    assert!(list(&node, None).await.is_empty());
    for op in ["signature_retract", "signature_delete"] {
        assert!(logged(dir.path(), op) >= 1, "{op} in ops.log");
    }

    // A restart reads the store as the deletes left it.
    drop(node);
    let node = open(dir.path()).await;
    assert!(list(&node, None).await.is_empty());
    assert!(node.signatures().load_errors().is_empty());
}

/// The `ops.log` entries naming `op`.
fn logged(dir: &Path, op: &str) -> usize {
    std::fs::read_to_string(dir.join("state").join("ops.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|l| l["op"]["op"] == op)
        .count()
}

#[tokio::test]
async fn a_retracted_record_survives_a_restart_and_an_older_record_loads() {
    let dir = tempfile::tempdir().unwrap();
    let (retracted, old) = {
        let node = open(dir.path()).await;
        let set = write_set(&node, "01SET");
        let a = propose(&node, &set[0].id, None).await.unwrap();
        let b = propose(&node, &set[1].id, None).await.unwrap();
        node.apply(Op::SignatureAccept {
            id: a.id.clone(),
            name: None,
        })
        .await
        .unwrap();
        let r = decided(&node, Op::SignatureRetract { id: a.id })
            .await
            .unwrap();
        (r, b)
    };
    // A record never retracted is written as the build before retract wrote it, with no
    // `retracted_ns`, and reads back so.
    let path = dir
        .path()
        .join("state")
        .join(SIGNATURES_DIR)
        .join(format!("{}.json", old.id));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(v["provenance"].get("retracted_ns").is_none(), "{v}");
    let node = open(dir.path()).await;
    assert!(
        node.signatures().load_errors().is_empty(),
        "{:?}",
        node.signatures().load_errors()
    );
    assert_eq!(node.signatures().get(&old.id), Some(old));
    assert_eq!(node.signatures().get(&retracted.id), Some(retracted));
}

#[tokio::test]
async fn the_store_reloads_its_records() {
    let dir = tempfile::tempdir().unwrap();
    let before = {
        let node = open(dir.path()).await;
        let set = write_set(&node, "01SET");
        let a = propose(&node, &set[0].id, None).await.unwrap();
        propose(&node, &set[1].id, None).await.unwrap();
        node.apply(Op::SignatureAccept {
            id: a.id,
            name: None,
        })
        .await
        .unwrap();
        list(&node, None).await
    };
    assert_eq!(before.len(), 2);
    let sigs = dir.path().join("state").join(SIGNATURES_DIR);
    std::fs::write(sigs.join("garbage.json"), b"{ not json").unwrap();
    let node = open(dir.path()).await;
    assert_eq!(list(&node, None).await, before);
    assert!(
        node.signatures()
            .load_errors()
            .iter()
            .any(|e| e.contains("garbage.json")),
        "{:?}",
        node.signatures().load_errors()
    );
    assert!(
        node.load_errors()
            .iter()
            .any(|e| e.contains("garbage.json")),
        "{:?}",
        node.load_errors()
    );
    // The store alone reads the same.
    let store = SignatureStore::open(&dir.path().join("state")).unwrap();
    assert_eq!(store.list(None), before);
}

#[tokio::test]
async fn a_noise_capture_proposes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut iq = vec![Complex::new(0.0f32, 0.0); RATE_HZ as usize];
    zeta_dsp::synth::awgn(&mut iq, 0.0, 7);
    let f = serde_json::to_value(features()).unwrap();
    let rec = node.bursts().write(new_capture("01NOISE", f), &iq).unwrap();
    assert_eq!(
        propose(&node, &rec.id, None).await.unwrap_err(),
        OpError::Other {
            message: format!("capture {} is noise: nothing to propose", rec.id)
        }
    );
    assert!(list(&node, None).await.is_empty());
}
