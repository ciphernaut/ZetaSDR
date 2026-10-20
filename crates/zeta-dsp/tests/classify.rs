//! End to end: file source -> node ring -> `Op::Classify` -> layered labels.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use zeta_core::classify::Label;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::{Hz, Selection, SourceId, Span, Timestamp};
use zeta_dsp::fixtures::{aprs_packet_iq, nbfm_modulate, tone, write_fixture};
use zeta_source_file::{FileOpts, FileSource};

const CENTER: u64 = 144_800_000;

/// A node over `iq` at 48 kHz centred on [`CENTER`], with the DSP classifiers
/// registered and at least 2 s in the ring. Played in real time, so the
/// first 2 s are still in the ring for several `classify` calls.
async fn node_over(dir: &std::path::Path, iq: &[Complex<f32>]) -> Node {
    let meta = write_fixture(dir, "fixture", iq, 48_000, CENTER);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let source = SourceId("file0".into());
    let src = Arc::new(FileSource::open(source.clone(), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 600.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    for c in zeta_dsp::classifiers() {
        node.add_classifier(c);
    }
    src.start();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(ring) = node.ring(&source) {
            let latest = ring.read().unwrap().latest();
            if latest.is_some_and(|t| t.0 >= 2_000_000_000) {
                break;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "ring never filled");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    node
}

/// `Op::Classify` over the first 2 s of `file0`, band `center_hz ± bandwidth_hz/2`.
async fn classify(node: &Node, center_hz: u64, bandwidth_hz: u64) -> Vec<Label> {
    let selection = Selection {
        source: SourceId("file0".into()),
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(2_000_000_000),
        },
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    };
    let labels = match node.apply(Op::Classify { selection }).await {
        Ok(OpResult::Labels(labels)) => labels,
        other => panic!("expected labels, got {other:?}"),
    };
    eprintln!("labels at {center_hz}: {labels:?}");
    labels
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classify_op_returns_layer1_and_layer2_labels() {
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let node = node_over(dir.path(), &iq).await;
    let labels = classify(&node, CENTER, 12_500).await;
    assert!(labels.iter().any(|l| l.layer == 1), "{labels:?}");
    assert!(
        labels
            .iter()
            .any(|l| l.layer == 2 && l.name == "APRS" && l.confidence == 1.0),
        "{labels:?}"
    );
}

/// APRS 12 kHz above centre plus an FM tone 12 kHz below it, 10 dB stronger.
fn aprs_beside_tone() -> Vec<Complex<f32>> {
    let aprs = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 12_000.0);
    let audio = tone(48_000, aprs.len(), 1_000.0, 1.0);
    let fm = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, -12_000.0);
    aprs.iter().zip(&fm).map(|(a, t)| a + t * 3.16).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classify_honours_selection_band() {
    let dir = tempfile::tempdir().unwrap();
    let node = node_over(dir.path(), &aprs_beside_tone()).await;
    let above = classify(&node, CENTER + 12_000, 12_500).await;
    assert!(above.iter().any(|l| l.layer == 1), "{above:?}");
    assert!(
        above.iter().any(|l| l.layer == 2 && l.name == "APRS"),
        "{above:?}"
    );
    let below = classify(&node, CENTER - 12_000, 12_500).await;
    assert!(below.iter().any(|l| l.layer == 1), "{below:?}");
    // The tone below decodes to nothing; only the catalogue names its band.
    assert!(
        !below.iter().any(|l| l.layer == 2 && l.name == "APRS"),
        "{below:?}"
    );
    assert!(
        below
            .iter()
            .filter(|l| l.layer == 2)
            .all(|l| l.evidence.starts_with("catalogue ")),
        "{below:?}"
    );
}
