//! End to end: file source -> node ring -> `Op::Classify` -> layered labels.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use zeta_core::classify::{Classification, Label, Outcome};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::{Hz, Selection, SourceId, Span, Timestamp};
use zeta_dsp::fixtures::{aprs_packet_iq, nbfm_modulate, pocsag_packet_iq, tone, write_fixture};
use zeta_source_file::{FileOpts, FileSource};

const CENTER: u64 = 144_800_000;

/// A node over `iq` at 48 kHz centred on [`CENTER`], with the DSP classifiers
/// registered and at least 2 s in the ring. Played in real time, so the
/// first 2 s are still in the ring for several `classify` calls.
async fn node_over(dir: &std::path::Path, iq: &[Complex<f32>]) -> Node {
    node_over_with(dir, iq, None).await
}

async fn node_over_with(
    dir: &std::path::Path,
    iq: &[Complex<f32>],
    modes_dir: Option<std::path::PathBuf>,
) -> Node {
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
        source_positions: Default::default(),
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 600.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
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
    classify_span(node, center_hz, bandwidth_hz, 2_000_000_000)
        .await
        .labels
}

/// `Op::Classify` over the first `span_ns` of `file0`: the labels and what was tried.
async fn classify_span(
    node: &Node,
    center_hz: u64,
    bandwidth_hz: u64,
    span_ns: u64,
) -> Classification {
    let selection = Selection {
        source: SourceId("file0".into()),
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(span_ns),
        },
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    };
    let c = match node.apply(Op::Classify { selection }).await {
        Ok(OpResult::Classification {
            labels,
            tried,
            allocations,
            origins,
        }) => Classification {
            labels,
            tried,
            allocations,
            origins,
        },
        other => panic!("expected a classification, got {other:?}"),
    };
    eprintln!("classification at {center_hz}: {c:?}");
    c
}

/// The outcome `tried` lists for `mode`, if it lists it.
fn outcome(c: &Classification, mode: &str) -> Option<Outcome> {
    c.tried.iter().find(|t| t.mode == mode).map(|t| t.outcome)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classify_reports_what_it_tried() {
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let node = node_over(dir.path(), &iq).await;
    let c = classify_span(&node, CENTER, 12_500, 2_000_000_000).await;
    assert_eq!(outcome(&c, "aprs_decode"), Some(Outcome::Proved), "{c:?}");
    assert_eq!(
        outcome(&c, "pocsag_decode"),
        Some(Outcome::NoFrame),
        "{c:?}"
    );
    let aprs = c
        .labels
        .iter()
        .find(|l| l.layer == 2 && l.name == "APRS")
        .unwrap_or_else(|| panic!("{c:?}"));
    assert_eq!(aprs.confidence, 1.0);
    assert!(
        aprs.evidence.starts_with("decode aprs_decode: "),
        "{aprs:?}"
    );
    // Highest score first.
    assert!(
        c.tried.windows(2).all(|w| w[0].score >= w[1].score),
        "{c:?}"
    );
    assert_eq!(c.tried[0].mode, "aprs_decode", "{c:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_selection_reports_skipped_span_and_a_layer1_label() {
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let node = node_over(dir.path(), &iq).await;
    let c = classify_span(&node, CENTER, 12_500, 500_000_000).await;
    assert!(!c.tried.is_empty(), "{c:?}");
    assert!(
        c.tried.iter().all(|t| t.outcome == Outcome::SkippedSpan),
        "{c:?}"
    );
    assert_eq!(c.labels.iter().filter(|l| l.layer == 1).count(), 1, "{c:?}");
    assert!(
        !c.labels.iter().any(|l| l.evidence.starts_with("decode ")),
        "{c:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn program_rows_are_skipped_quick() {
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let node = node_over(dir.path(), &iq).await;
    let c = classify_span(&node, CENTER, 12_500, 2_000_000_000).await;
    // A program row in range (adsb_decode has no range) is tried but has no quick attempt.
    assert_eq!(
        outcome(&c, "adsb_decode"),
        Some(Outcome::SkippedQuick),
        "{c:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wide_rows_are_skipped_on_a_narrow_box() {
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let node = node_over(dir.path(), &iq).await;
    let c = classify_span(&node, CENTER, 12_500, 2_000_000_000).await;
    assert_eq!(
        outcome(&c, "rds_decode"),
        Some(Outcome::SkippedBand),
        "{c:?}"
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ten_khz_box_still_tries_aprs() {
    // Narrower than the row's 12 500 Hz: fitted, and extracted, as 12 500 Hz.
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let node = node_over(dir.path(), &iq).await;
    let c = classify_span(&node, CENTER, 10_000, 2_000_000_000).await;
    assert_eq!(outcome(&c, "aprs_decode"), Some(Outcome::Proved), "{c:?}");
    assert_eq!(
        outcome(&c, "pocsag_decode"),
        Some(Outcome::NoFrame),
        "{c:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_40_khz_box_decodes_on_its_own_channel() {
    // A box between 12 500 Hz and the RDS gate runs the quick attempts on the
    // selection's own 40 kHz channel, as before the rows.
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let node = node_over(dir.path(), &iq).await;
    let c = classify_span(&node, CENTER, 40_000, 2_000_000_000).await;
    assert_eq!(outcome(&c, "aprs_decode"), Some(Outcome::Proved), "{c:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_packet_10_khz_off_a_40_khz_box_centre_is_proved_on_the_row_width_slice() {
    // POCSAG 10 kHz above the centre of a 40 kHz box: the box's own channel fails, the
    // 12 500 Hz slice at the measured centre decodes.
    let dir = tempfile::tempdir().unwrap();
    let mut iq = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 10_000.0);
    // A noise floor, so the measured occupied band is the signal's and not the whole box.
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut uniform = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f32 / (1u64 << 53) as f32 - 0.5
    };
    for x in iq.iter_mut() {
        *x += Complex::new(uniform(), uniform()) * 0.6;
    }
    let node = node_over(dir.path(), &iq).await;
    let c = classify_span(&node, CENTER, 40_000, 2_000_000_000).await;
    assert_eq!(outcome(&c, "pocsag_decode"), Some(Outcome::Proved), "{c:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sonde_rows_do_not_fill_the_tried_list_outside_their_range() {
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let node = node_over(dir.path(), &iq).await;
    let c = classify_span(&node, CENTER, 12_500, 2_000_000_000).await;
    for sonde in ["rs41_decode", "dfm_decode", "m20_decode"] {
        assert_eq!(outcome(&c, sonde), None, "{c:?}");
    }
    assert!(outcome(&c, "pocsag_decode").is_some(), "{c:?}");
    assert!(outcome(&c, "aprs_decode").is_some(), "{c:?}");
}
