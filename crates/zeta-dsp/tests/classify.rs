//! End to end: file source -> node ring -> `Op::Classify` -> layered labels.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;
use std::time::Duration;

use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::{Hz, Selection, SourceId, Span, Timestamp};
use zeta_dsp::fixtures::{aprs_packet_iq, write_fixture};
use zeta_source_file::{FileOpts, FileSource};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classify_op_returns_layer1_and_layer2_labels() {
    let dir = tempfile::tempdir().unwrap();
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    let meta = write_fixture(dir.path(), "aprs", &iq, 48_000, 144_800_000);
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
    };
    let source = SourceId("file0".into());
    let src = Arc::new(FileSource::open(source.clone(), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 60.0,
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

    let selection = Selection {
        source,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(2_000_000_000),
        },
        center_hz: Hz(144_800_000),
        bandwidth_hz: Hz(12_500),
    };
    let Ok(OpResult::Labels(labels)) = node.apply(Op::Classify { selection }).await else {
        panic!("expected labels")
    };
    eprintln!("labels: {labels:?}");
    assert!(labels.iter().any(|l| l.layer == 1), "{labels:?}");
    assert!(
        labels
            .iter()
            .any(|l| l.layer == 2 && l.name == "APRS" && l.confidence == 1.0),
        "{labels:?}"
    );
}
