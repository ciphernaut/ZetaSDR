//! The iNTERCEPT signal catalogue as a knowledge provider and a layer-2 classifier.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;

use num_complex::Complex;
use zeta_core::classify::{Band, Classifier, Label};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult};
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::Hz;
use zeta_dsp::catalogue::{CatalogueClassifier, CatalogueKnowledge, catalogue};
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
        state_dir: dir.path().join("state"),
        ring_seconds: 10.0,
        backends: vec![Backend::Cpu],
        resume: false,
    };
    let node = Node::open(cfg, StageRegistry::new(), vec![]).await.unwrap();
    node.add_knowledge(Arc::new(CatalogueKnowledge));
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
