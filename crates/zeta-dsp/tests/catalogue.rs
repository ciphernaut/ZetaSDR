//! The iNTERCEPT signal catalogue as a knowledge provider and a layer-2 classifier.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;

use num_complex::Complex;
use zeta_core::classify::{Band, Classifier, FeatureRecord, Label, Measurement};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult};
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::Hz;
use zeta_dsp::catalogue::{CatalogueClassifier, CatalogueKnowledge, catalogue, catalogue_labels};
use zeta_dsp::fixtures::{awgn, nbfm_modulate, tone, wfm_modulate};

const WFM_RATE: u64 = 240_000;

async fn run(iq: &[Complex<f32>], rate_hz: u64, center_hz: u64) -> Vec<Label> {
    let labels = CatalogueClassifier
        .classify(
            iq,
            Hz(rate_hz),
            Hz(center_hz),
            Band::whole(Hz(rate_hz)),
            None,
        )
        .await;
    eprintln!("labels at {center_hz}: {labels:?}");
    labels
}

#[test]
fn catalogue_parses_594_entries_with_fm_broadcast() {
    let c = catalogue();
    assert_eq!(c.len(), 594);
    let fm = c
        .iter()
        .find(|e| e.id == "fm-broadcast")
        .expect("fm-broadcast");
    assert_eq!(fm.name, "FM Broadcast Radio");
    assert_eq!(fm.frequency_ranges, [(Hz(87_500_000), Hz(108_000_000))]);
    assert_eq!(fm.bandwidth_min_hz, Some(Hz(150_000)));
    assert_eq!(fm.bandwidth_max_hz, Some(Hz(250_000)));
    assert!(
        fm.modulations.iter().any(|m| m == "WFM"),
        "{:?}",
        fm.modulations
    );
    // Entries without a bandwidth range or modulations are kept.
    assert!(c.iter().any(|e| e.bandwidth_max_hz.is_none()));
    assert!(c.iter().any(|e| e.modulations.is_empty()));
}

#[tokio::test]
async fn wfm_fixture_at_104_9_mhz_is_fm_broadcast_radio() {
    let audio = tone(48_000, 48_000, 1_000.0, 0.9);
    let mut iq = wfm_modulate(&audio, 48_000, WFM_RATE as u32, 75_000.0, 0.0, true);
    awgn(&mut iq, 20.0, 21);
    let labels = run(&iq, WFM_RATE, 104_900_000).await;
    assert!(!labels.is_empty() && labels.len() <= 3, "{labels:?}");
    assert!(labels.iter().all(|l| l.layer == 2), "{labels:?}");
    let top = &labels[0];
    assert_eq!(top.name, "FM Broadcast Radio", "{labels:?}");
    assert!(top.confidence >= 0.9, "{top:?}");
    assert!(
        top.evidence.contains("catalogue fm-broadcast"),
        "{}",
        top.evidence
    );
}

#[tokio::test]
async fn catalogue_uses_a_handed_measurement_without_measuring() {
    // No IQ at all: measuring would find nothing to label, so labels whose
    // evidence carries the marker bandwidth (123 457 Hz, never measured from
    // a fixture) and the handed layer-1 name came from the measurement.
    let m = Measurement {
        features: FeatureRecord {
            bandwidth_hz: Hz(123_457),
            center_hz: Hz(104_900_000),
            symbol_rate_baud: None,
            snr_db: 20.0,
            burst: false,
            duty: None,
            bursts: None,
            burst_ms: None,
            period_ms: None,
            order: None,
            tone_spacing_hz: None,
            symbol_rate_line: None,
        },
        label: Label {
            layer: 1,
            name: "wfm".into(),
            confidence: 0.9,
            evidence: String::new(),
        },
    };
    let band = Band::whole(Hz(WFM_RATE));
    let (labels, own, _) = CatalogueClassifier
        .classify_measured(
            &[],
            Hz(WFM_RATE),
            Hz(104_900_000),
            band,
            None,
            Some(&m),
            &[],
        )
        .await;
    assert!(own.is_none());
    let top = labels.first().expect("labels from the handed measurement");
    assert_eq!(top.name, "FM Broadcast Radio", "{labels:?}");
    assert!(
        top.evidence.contains("123457 Hz wide, layer-1 wfm"),
        "{}",
        top.evidence
    );
    // A handed noise label names nothing.
    let noise = Measurement {
        label: Label {
            name: "noise".into(),
            ..m.label.clone()
        },
        ..m
    };
    let (labels, _, _) = CatalogueClassifier
        .classify_measured(
            &[],
            Hz(WFM_RATE),
            Hz(104_900_000),
            band,
            None,
            Some(&noise),
            &[],
        )
        .await;
    assert!(labels.is_empty(), "{labels:?}");
}

#[tokio::test]
async fn wfm_fixture_at_104_9_mhz_is_not_cbet() {
    // CBET is a 2 kHz entry in 88-108 MHz modulated FM: a 150 kHz band is
    // far off its bandwidth, and the modulation match alone must not name it.
    let audio = tone(48_000, 48_000, 1_000.0, 0.9);
    let mut iq = wfm_modulate(&audio, 48_000, WFM_RATE as u32, 75_000.0, 0.0, true);
    awgn(&mut iq, 20.0, 21);
    let labels = run(&iq, WFM_RATE, 104_900_000).await;
    assert!(
        labels
            .iter()
            .all(|l| !l.evidence.contains("catalogue cbet")),
        "{labels:?}"
    );
    for layer1 in ["wfm", "fm"] {
        let direct = catalogue_labels(104_900_000, 150_000, layer1);
        assert!(
            direct.iter().all(|l| l.name != "CBET"),
            "{layer1}: {direct:?}"
        );
    }
}

#[tokio::test]
async fn catalogue_does_not_match_nbfm_in_the_broadcast_band() {
    // The plan-2 12.5 kHz channel fixture: 1 kHz tone, 2.5 kHz deviation.
    let audio = tone(48_000, 48_000, 1_000.0, 1.0);
    let mut iq = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, 0.0);
    awgn(&mut iq, 20.0, 3);
    let labels = run(&iq, 48_000, 100_000_000).await;
    assert!(
        labels.iter().all(|l| l.name != "FM Broadcast Radio"),
        "{labels:?}"
    );
}

#[tokio::test]
async fn knowledge_lookup_returns_catalogue_signatures() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = NodeConfig {
        source_positions: Default::default(),
        burst_policy: Default::default(),
        state_dir: dir.path().join("state"),
        ring_seconds: 10.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: false,
        bank_factory: None,
        modes_dir: None,
    };
    let node = Node::open(cfg, stages(), vec![]).await.unwrap();
    node.add_knowledge(Arc::new(CatalogueKnowledge::new(node.modes())));
    let op = Op::KnowledgeLookup {
        region: "AU".into(),
        center_hz: Hz(104_900_000),
        bandwidth_hz: Hz(200_000),
    };
    let Ok(OpResult::Knowledge { signatures, .. }) = node.apply(op).await else {
        panic!("expected knowledge");
    };
    let fm = signatures
        .iter()
        .find(|s| s.name == "FM Broadcast Radio")
        .unwrap_or_else(|| panic!("{signatures:?}"));
    assert_eq!(fm.modulation, "WFM");
    assert_eq!((fm.min_hz, fm.max_hz), (Hz(87_500_000), Hz(108_000_000)));
    assert_eq!(fm.bandwidth_hz, Hz(250_000));
}

#[test]
fn equal_scores_prefer_the_narrowest_containing_frequency_range() {
    // At 144.8 MHz a 12.5 kHz fm channel scores 1.0 against both the 2 m
    // amateur band (144-146 MHz) and POCSAG (138-175 MHz); the narrower
    // allocation is the more specific answer.
    let labels = catalogue_labels(144_800_000, 12_500, "fm");
    eprintln!("labels: {labels:?}");
    assert!(!labels.is_empty(), "{labels:?}");
    assert_eq!(labels[0].name, "Amateur Radio (2m Band)", "{labels:?}");
    assert!(labels.iter().all(|l| l.confidence <= labels[0].confidence));
}

#[test]
fn equal_scores_are_not_left_in_catalogue_order() {
    // At 433.92 MHz a 3 kHz am signal scores 0.7 against a Denso key fob
    // (431.75-436.09 MHz, listed first) and the ISM weather sensors
    // (430.84-435.17 MHz); among equal scores the narrower containing range
    // comes first, whatever the catalogue order.
    let labels = catalogue_labels(433_920_000, 3_000, "am");
    eprintln!("labels: {labels:?}");
    assert!(!labels.is_empty(), "{labels:?}");
    let widths: Vec<u64> = labels
        .iter()
        .filter(|l| (l.confidence - labels[0].confidence).abs() < 1e-6)
        .map(|l| {
            let e = catalogue().iter().find(|e| e.name == l.name).unwrap();
            e.frequency_ranges
                .iter()
                .filter(|&&(lo, hi)| (lo.0..=hi.0).contains(&433_920_000))
                .map(|&(lo, hi)| hi.0 - lo.0)
                .min()
                .unwrap()
        })
        .collect();
    assert!(
        widths.windows(2).all(|w| w[0] <= w[1]),
        "{widths:?} {labels:?}"
    );
}

async fn lookup(center_hz: u64, bandwidth_hz: u64) -> Vec<zeta_core::knowledge::Signature> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = NodeConfig {
        source_positions: Default::default(),
        burst_policy: Default::default(),
        state_dir: dir.path().join("state"),
        ring_seconds: 10.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: false,
        bank_factory: None,
        modes_dir: None,
    };
    let node = Node::open(cfg, stages(), vec![]).await.unwrap();
    node.add_knowledge(Arc::new(CatalogueKnowledge::new(node.modes())));
    let op = Op::KnowledgeLookup {
        region: "AU".into(),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    };
    let Ok(OpResult::Knowledge { signatures, .. }) = node.apply(op).await else {
        panic!("expected knowledge");
    };
    signatures
}

#[tokio::test]
async fn signatures_name_their_decoder_job_kind() {
    for (center, bw, kind) in [
        (1_090_000_000, 2_400_000, "adsb_decode"),
        (144_800_000, 12_500, "aprs_decode"),
        (131_550_000, 10_000, "acars_decode"),
    ] {
        let sigs = lookup(center, bw).await;
        assert!(
            sigs.iter().any(|s| s.decoder.as_deref() == Some(kind)),
            "{kind}: {sigs:?}"
        );
    }
    let sigs = lookup(104_900_000, 200_000).await;
    let fm = sigs
        .iter()
        .find(|s| s.name == "FM Broadcast Radio")
        .unwrap();
    assert_eq!(fm.decoder, None);
}

/// The DSP stages, so the graph rows validate.
fn stages() -> StageRegistry {
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    reg
}

#[test]
fn rows_cover_every_catalogue_id_the_old_table_did() {
    let modes = zeta_core::modes::ModeRegistry::load(&stages(), None);
    let row_for = |id: &str| {
        modes
            .all()
            .iter()
            .find(|m| m.row.catalogue.iter().any(|c| c == id))
            .map(|m| m.row.id.0.clone())
    };
    for (ids, mode) in [
        (
            &["pocsag", "pocsag-2", "pager-pocsag-1200"][..],
            "pocsag_decode",
        ),
        (
            &["ads-b", "automatic-dependent-surveillance-broadcast-ads-b"][..],
            "adsb_decode",
        ),
        (
            &[
                "acars",
                "aircraft-communications-addressing-and-reporting-system-acar",
            ][..],
            "acars_decode",
        ),
        (&["radio-data-system-rds"][..], "rds_decode"),
        (
            &["aprs", "automatic-packet-reporting-system-aprs"][..],
            "aprs_decode",
        ),
    ] {
        for id in ids {
            assert_eq!(row_for(id).as_deref(), Some(mode), "{id}");
        }
    }
    assert_eq!(row_for("fm-broadcast"), None);
}
