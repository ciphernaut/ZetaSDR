//! The `detect` stage on a file source: `fft → detect` over a synthesised FSK burst.
#![cfg(feature = "test-fixtures")]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use serde_json::{Value, json};
use zeta_core::graph::{Edge, Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Hz, SourceId, StreamId, StreamKind};
use zeta_dsp::fixtures::{Impairments, SynthSpec, awgn, synthesise};
use zeta_source_file::{FileOpts, FileSource};

const RATE: u32 = 6_000_000;
const CENTER_HZ: u64 = 403_000_000;
const OFFSET_HZ: f64 = 1_000_000.0;
const SILENCE_S: f64 = 3.0;

/// Write `iq` as a ci8 SigMF pair (an eighth of cf32's size); the meta path.
fn write_ci8(dir: &std::path::Path, iq: &[Complex<f32>]) -> std::path::PathBuf {
    let q = |v: f32| (v * 128.0).round().clamp(-128.0, 127.0) as i8 as u8;
    let data: Vec<u8> = iq.iter().flat_map(|s| [q(s.re), q(s.im)]).collect();
    std::fs::write(dir.join("burst.sigmf-data"), data).unwrap();
    let meta = json!({
        "global": {"core:datatype": "ci8", "core:sample_rate": RATE, "core:version": "1.0.0"},
        "captures": [{"core:sample_start": 0, "core:frequency": CENTER_HZ}],
        "annotations": [],
    });
    let path = dir.join("burst.sigmf-meta");
    std::fs::write(&path, meta.to_string()).unwrap();
    path
}

/// The 4800-baud FSK burst (synthesised at its own 48 kHz) interpolated to [`RATE`],
/// mixed to [`OFFSET_HZ`] and followed by [`SILENCE_S`] of silence, under white noise.
fn capture() -> Vec<Complex<f32>> {
    let spec = SynthSpec::Fsk {
        symbol_rate_baud: 4800,
        deviation_hz: 2400.0,
        order: 2,
        bits: "pattern:aa".into(),
        repeat: 3,
    };
    let narrow_rate = spec.rate_hz();
    let narrow = synthesise(&spec, &Impairments::default(), narrow_rate, CENTER_HZ);
    let ratio = RATE as f64 / narrow_rate as f64;
    let burst_len = ((narrow.len() - 1) as f64 * ratio) as usize;
    let total = burst_len + (SILENCE_S * RATE as f64) as usize;
    let step = Complex::from_polar(1.0f64, 2.0 * std::f64::consts::PI * OFFSET_HZ / RATE as f64);
    let mut lo = Complex::new(1.0f64, 0.0);
    let mut iq = vec![Complex::new(0.0f32, 0.0); total];
    for (n, out) in iq.iter_mut().enumerate().take(burst_len) {
        let x = n as f64 / ratio;
        let (i, frac) = (x as usize, (x.fract()) as f32);
        let s = narrow[i] * (1.0 - frac) + narrow[i + 1] * frac;
        let m = Complex::new(lo.re as f32, lo.im as f32);
        *out = s * m * 0.3;
        lo *= step;
        if n % 1024 == 0 {
            lo /= lo.norm();
        }
    }
    awgn(&mut iq, 10.0, 17);
    iq
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detect_publishes_new_and_gone_for_a_synthesised_burst() {
    let dir = tempfile::tempdir().unwrap();
    let meta = {
        let iq = capture();
        write_ci8(dir.path(), &iq)
    };
    let opts = FileOpts {
        // Paced like a radio, so the fft stage keeps up and every row arrives.
        realtime: true,
        block_samples: 60_000,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    };
    let node = Node::open(cfg, reg, vec![port]).await.unwrap();

    let OpResult::Job(job) = node
        .apply(Op::JobStart {
            kind: JobKind::Spectrum,
            source: SourceId("file0".into()),
            center_hz: Hz(CENTER_HZ),
            bandwidth_hz: Hz(RATE as u64),
            deviation_hz: None,
            sideband: None,
            elastic: false,
            replace: vec![],
        })
        .await
        .unwrap()
    else {
        panic!("expected a job id")
    };
    let bus = node.bus();
    let before: HashSet<StreamId> = bus.list().into_iter().map(|s| s.id).collect();
    let (fft, detect) = (NodeId("fft".into()), NodeId("detect".into()));
    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![
            GraphNode {
                id: fft.clone(),
                kind: "fft".into(),
                params: json!({"size": 4096, "window": "hann", "rows_per_s": 20}),
            },
            GraphNode {
                id: detect.clone(),
                kind: "detect".into(),
                params: json!({"persist_ms": 1000, "gone_ms": 2000}),
            },
        ],
        edges: vec![Edge {
            from: fft,
            to: detect,
            kind: StreamKind::Event,
        }],
    };
    let put = node
        .apply(Op::GraphPut {
            job: job.clone(),
            graph,
        })
        .await;
    assert!(matches!(put, Ok(OpResult::Ok)), "{put:?}");

    // The relaunched job's two event streams: the fft's rows and detect's signals.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let streams: Vec<StreamId> = loop {
        let s: Vec<StreamId> = bus
            .list()
            .into_iter()
            .filter(|s| {
                s.kind == StreamKind::Event
                    && s.provenance.job.as_ref() == Some(&job)
                    && !before.contains(&s.id)
            })
            .map(|s| s.id)
            .collect();
        if s.len() == 2 {
            break s;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job streams after graph put: {s:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let mut rxs: Vec<_> = streams.iter().map(|s| bus.subscribe(s).unwrap()).collect();
    src.start();

    let mut signals: Vec<Value> = Vec::new();
    let collect = async {
        loop {
            for rx in rxs.iter_mut() {
                while let Ok(f) = rx.try_recv() {
                    if let Payload::Event(v) = f.payload
                        && v["protocol"] == "signal"
                    {
                        eprintln!("signal: {v}");
                        signals.push(v);
                    }
                }
            }
            if signals.iter().any(|v| v["state"] == "gone") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(20), collect)
        .await
        .unwrap_or_else(|_| panic!("no gone signal within 20 s: {signals:?}"));

    let states: Vec<&str> = signals.iter().filter_map(|v| v["state"].as_str()).collect();
    assert_eq!(states, ["new", "gone"], "{signals:?}");
    let want = CENTER_HZ + OFFSET_HZ as u64;
    for v in &signals {
        let c = v["center_hz"].as_u64().unwrap();
        let bw = v["bandwidth_hz"].as_u64().unwrap();
        assert!(c.abs_diff(want) <= 6_000, "centre {c}: {v}");
        assert!((5_000..=30_000).contains(&bw), "bandwidth {bw}: {v}");
        let summary = format!(
            "{} {:.4} MHz {:.1} kHz {:.1} dB",
            v["state"].as_str().unwrap(),
            c as f64 / 1e6,
            bw as f64 / 1e3,
            v["snr_db"].as_f64().unwrap()
        );
        assert_eq!(v["summary"], summary.as_str());
    }
    let (new, gone) = (&signals[0], &signals[1]);
    assert_eq!(new["first_ns"], gone["first_ns"]);
    let span_ms =
        (gone["last_ns"].as_u64().unwrap() - gone["first_ns"].as_u64().unwrap()) / 1_000_000;
    assert!(
        (2_500..=3_500).contains(&span_ms),
        "burst seen for {span_ms} ms"
    );
    let silent_ms =
        (gone["t_ns"].as_u64().unwrap() - gone["last_ns"].as_u64().unwrap()) / 1_000_000;
    assert!(
        (2_000..=2_200).contains(&silent_ms),
        "gone after {silent_ms} ms"
    );
}
