//! The `capture_*` ops over a node's burst store: list (narrowed by a signal band),
//! show (with the meta), classify (the window run through the classifiers) and delete.

use std::path::Path;

use std::sync::Arc;

use num_complex::Complex;
use zeta_core::bursts::NewCapture;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpError, OpResult, SignalBand};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId};

const RATE_HZ: u32 = 48_000;
const POCSAG_HZ: u64 = 148_637_500;

async fn open(dir: &Path) -> Node {
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
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
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    node
}

fn new_capture(center_hz: u64, bandwidth_hz: u64, start_ns: u64) -> NewCapture {
    NewCapture {
        source: SourceId("rx".into()),
        set: JobId("set".into()),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
        sample_rate_hz: Hz(RATE_HZ as u64),
        start_ns,
        end_ns: start_ns + 1_000_000_000,
        features: serde_json::json!({}),
        signal: None,
        job: JobId("cap".into()),
        watch: None,
        allocations: serde_json::json!([]),
    }
}

fn tone(n: usize) -> Vec<Complex<f32>> {
    vec![Complex::new(0.1, 0.0); n]
}

#[tokio::test]
async fn capture_list_filters_by_overlapping_band_and_show_carries_the_meta() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let a = node
        .bursts()
        .write(new_capture(433_920_000, 10_000, 1), &tone(480))
        .unwrap();
    let b = node
        .bursts()
        .write(new_capture(148_637_500, 12_500, 2), &tone(480))
        .unwrap();
    let OpResult::Captures(all) = node.apply(Op::CaptureList { signal: None }).await.unwrap()
    else {
        panic!("captures")
    };
    assert_eq!(all.len(), 2);
    let signal = Some(SignalBand {
        center_hz: Hz(433_925_000),
        bandwidth_hz: Hz(10_000),
    });
    let OpResult::Captures(some) = node.apply(Op::CaptureList { signal }).await.unwrap() else {
        panic!("captures")
    };
    assert_eq!(some.iter().map(|r| &r.id).collect::<Vec<_>>(), [&a.id]);
    let OpResult::Capture { record, meta } = node
        .apply(Op::CaptureShow {
            capture: b.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("capture")
    };
    assert_eq!(record.id, b.id);
    assert_eq!(meta["global"]["core:sample_rate"], RATE_HZ);
    assert_eq!(meta["captures"][0]["core:frequency"], 148_637_500u64);
    let v = serde_json::to_value(Op::CaptureList { signal: None }).unwrap();
    assert!(v["signal"].is_null());
}

#[tokio::test]
async fn capture_classify_runs_the_classifiers_over_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut iq = zeta_dsp::synth::pocsag_packet_iq(1_234_567, 3, "HELLO ZETA", 1200, RATE_HZ, 0.0);
    // Two seconds (the rows' shortest span is one) over a noise floor, so the measured
    // occupied band is the signal's and not the whole window.
    iq.resize(2 * RATE_HZ as usize, Complex::new(0.0, 0.0));
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    for x in iq.iter_mut() {
        let mut uniform = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        };
        *x += Complex::new(uniform(), uniform()) * 0.6;
    }
    let rec = node
        .bursts()
        .write(new_capture(POCSAG_HZ, 12_500, 1), &iq)
        .unwrap();
    let OpResult::Classification { labels, tried, .. } = node
        .apply(Op::CaptureClassify { capture: rec.id })
        .await
        .unwrap()
    else {
        panic!("classification")
    };
    assert!(
        labels.iter().any(|l| l.name == "POCSAG"),
        "{labels:?} {tried:?}"
    );
}

#[tokio::test]
async fn capture_delete_refuses_a_pinned_capture() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let rec = node
        .bursts()
        .write(new_capture(433_920_000, 10_000, 1), &tone(480))
        .unwrap();
    node.bursts().pin(&rec.id).unwrap();
    let err = node
        .apply(Op::CaptureDelete {
            capture: rec.id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        err,
        OpError::Other {
            message: format!("capture {} is pinned", rec.id)
        }
    );
    assert!(node.bursts().get(&rec.id).is_some());
}

#[tokio::test]
async fn an_unknown_capture_is_unknown_naming_the_field() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let rec = node
        .bursts()
        .write(new_capture(433_920_000, 10_000, 1), &tone(480))
        .unwrap();
    for op in [
        Op::CaptureShow {
            capture: "nope".into(),
        },
        Op::CaptureClassify {
            capture: "nope".into(),
        },
        Op::CaptureDelete {
            capture: "nope".into(),
        },
    ] {
        let Err(OpError::Unknown {
            field,
            value,
            known,
        }) = node.apply(op).await
        else {
            panic!("unknown")
        };
        assert_eq!((field.as_str(), value.as_str()), ("capture", "nope"));
        assert_eq!(known, std::slice::from_ref(&rec.id));
    }
    let r = node
        .apply(Op::CaptureDelete { capture: rec.id })
        .await
        .unwrap();
    assert_eq!(r, OpResult::Ok);
    assert!(node.bursts().get("nope").is_none());
}

/// `classify` (over a source's ring) and `capture_classify` (a stored window) both answer
/// `origins`, scored on the measured layer-1 label and from the capture's own source.
#[tokio::test]
async fn classify_and_capture_classify_return_origins_scored_on_the_measured_label() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    // A pure tone 1 kHz above the centre, in a 6 kHz record band.
    let iq: Vec<Complex<f32>> = (0..2 * RATE_HZ as usize)
        .map(|k| {
            let ph = 2.0 * std::f64::consts::PI * (k as f64 * 1_000.0 / RATE_HZ as f64);
            Complex::from_polar(0.5, ph as f32)
        })
        .collect();
    let rec = node
        .bursts()
        .write(new_capture(433_920_000, 6_000, 1), &iq)
        .unwrap();
    let OpResult::Classification {
        labels, origins, ..
    } = node
        .apply(Op::CaptureClassify { capture: rec.id })
        .await
        .unwrap()
    else {
        panic!("classification")
    };
    let l1 = labels.iter().find(|l| l.layer == 1).expect("layer 1");
    let origins = origins.expect("capture_classify scores origins");
    // The capture's source is gone from the node: it reads as local, from the record.
    assert_eq!(origins.source, SourceId("rx".into()));
    assert_eq!(origins.band.center_hz, Hz(433_920_000));
    assert_eq!(origins.band.bandwidth_hz, Hz(6_000));
    assert_eq!(origins.modulation, vec![l1.name.clone()]);
}

#[tokio::test]
async fn capture_classify_reads_the_records_band_not_the_whole_file() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    // A pure tone 1 kHz above the centre, 6 kHz record band inside a 48 kHz file.
    let iq: Vec<Complex<f32>> = (0..2 * RATE_HZ as usize)
        .map(|k| {
            let ph = 2.0 * std::f64::consts::PI * (k as f64 * 1_000.0 / RATE_HZ as f64);
            Complex::from_polar(0.5, ph as f32)
        })
        .collect();
    let rec = node
        .bursts()
        .write(new_capture(433_920_000, 6_000, 1), &iq)
        .unwrap();
    let OpResult::Classification { labels, .. } = node
        .apply(Op::CaptureClassify { capture: rec.id })
        .await
        .unwrap()
    else {
        panic!("classification")
    };
    let l1 = labels.iter().find(|l| l.layer == 1).expect("layer 1");
    assert_eq!(l1.name, "carrier", "{labels:?}");
}
