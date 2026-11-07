//! Learned knowledge: an accepted signature is listed by `knowledge_lookup` (tier
//! `learned`) and scored by the layer-2 `learned` classifier on a later capture of the
//! same signal; a proposed or rejected one is not, and a different signal on the band
//! scores under the floor. Run over captures of synthesised 2-FSK at 4800 Bd in a
//! 20 kHz band at 433.92 MHz, whose feature record is layer 1's own measurement.

use std::path::Path;
use std::sync::Arc;

use num_complex::Complex;
use zeta_core::bursts::{CaptureRecord, NewCapture};
use zeta_core::classify::{Band, ClassifierSet, FeatureRecord, Label};
use zeta_core::knowledge::StaticKnowledge;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult};
use zeta_core::signatures::{CONFIDENCE_FEATURES, SignatureRecord};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId};

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
    };
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut registry);
    let node = Node::open(cfg, registry, vec![]).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes()) {
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
    for c in zeta_dsp::classifiers(node.modes()) {
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
