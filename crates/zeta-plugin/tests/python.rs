//! The Python SDK (`sdk/python/zeta_plugin`) and its `gain` example, run for real through
//! the host: probed, run as a stage, and run inside a node job put there by `graph put`
//! with zeta-core untouched. Every test is skipped when `python3` is not on `PATH`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use serde_json::json;
use tokio::sync::broadcast;
use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{IqBlock, Port};
use zeta_core::stage::{Backend, PassthroughStage, Stage, StageError, StageRegistry};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{
    ChannelId, Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};
use zeta_plugin::host::{PluginCommand, probe};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// The repository root.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// `python3`'s absolute path (the host does no `PATH` lookup), from `PATH`.
fn python3() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set when python3 runs");
    std::env::split_paths(&path)
        .map(|d| d.join("python3"))
        .find(|p| p.is_file())
        .expect("python3 ran, so it is on PATH")
}

/// `python3 -I <script>`.
fn py(script: &Path) -> PluginCommand {
    PluginCommand {
        program: python3(),
        args: vec!["-I".into(), script.display().to_string()],
    }
}

fn gain() -> PluginCommand {
    py(&repo().join("examples/python/gain.py"))
}

fn prov() -> Provenance {
    Provenance {
        source: SourceId("src".into()),
        job: Some(JobId("j1".into())),
        channel: Some(ChannelId("c1".into())),
        span: Span {
            start_ns: Timestamp(1_000),
            end_ns: Timestamp(2_000),
        },
    }
}

fn iq_frame(samples: &[Complex<f32>]) -> Frame {
    Frame::new(
        StreamId("in".into()),
        prov(),
        Payload::Iq(IqBlock {
            t0: Timestamp(1_000),
            sample_rate_hz: Hz(48_000),
            center_hz: Hz(100_000_000),
            samples: samples.iter().copied().collect(),
        }),
    )
}

fn close(a: Complex<f32>, b: Complex<f32>) -> bool {
    (a - b).norm() <= 1e-6
}

#[tokio::test]
async fn python_gain_plugin_scales_iq() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: python3 not found");
        return;
    }
    let stage = probe("gain", gain()).await.unwrap();
    assert_eq!(stage.spec().kind, "gain");
    assert_eq!(stage.spec().inputs, vec![StreamKind::Iq]);
    assert_eq!(stage.spec().outputs, vec![StreamKind::Iq]);

    let bus = Arc::new(StreamBus::new(64));
    let out = bus.register(StreamKind::Iq, prov());
    let mut rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(16);
    let input = iq_frame(&[Complex::new(1.0, 2.0), Complex::new(-0.5, 0.25)]);
    tx.send(input.clone()).unwrap();
    drop(tx);
    let (b, o) = (bus.clone(), out.clone());
    let run = tokio::spawn(async move {
        stage
            .run(
                json!({"factor": 0.5}),
                vec![rx_in],
                vec![o],
                b,
                Default::default(),
                zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
            )
            .await
    });

    let got = tokio::time::timeout(Duration::from_secs(10), rx_out.recv())
        .await
        .expect("a frame within 10 s")
        .unwrap();
    assert_eq!(got.stream, out);
    assert_eq!(got.provenance, input.provenance);
    let (Payload::Iq(a), Payload::Iq(b)) = (&got.payload, &input.payload) else {
        panic!("iq expected");
    };
    let want = [Complex::new(0.5, 1.0), Complex::new(-0.25, 0.125)];
    assert_eq!(a.samples.len(), want.len());
    for (g, w) in a.samples.iter().zip(want) {
        assert!(close(*g, w), "{g} != {w}");
    }
    assert_eq!(
        (a.t0, a.sample_rate_hz, a.center_hz),
        (b.t0, b.sample_rate_hz, b.center_hz)
    );
    let r = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap();
    assert!(r.is_ok(), "{r:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_plugin_runs_in_a_job_via_graph_put() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: python3 not found");
        return;
    }
    let gain = probe("gain", gain()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let meta = fixtures::write_tone(dir.path());
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    let mut reg = StageRegistry::new();
    reg.register(Arc::new(PassthroughStage::new()));
    reg.register(Arc::new(gain));
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
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
    src.start();

    let OpResult::Job(job) = node
        .apply(Op::JobStart {
            kind: JobKind::Passthrough,
            source: SourceId("file0".into()),
            center_hz: Hz(100_000_000),
            bandwidth_hz: Hz(48_000),
            deviation_hz: None,
            sideband: None,
            dwell_ms: None,
            average: None,
            bits: None,
            method: None,
            bin_hz: None,
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

    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("gain".into()),
            kind: "gain".into(),
            params: json!({"factor": 0.5}),
        }],
        edges: vec![],
    };
    let put = node
        .apply(Op::GraphPut {
            job: job.clone(),
            graph,
        })
        .await;
    assert!(matches!(put, Ok(OpResult::Ok)), "{put:?}");

    // The relaunched job has two iq streams: the source's, feeding the root, and the
    // gain node's output.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let streams: Vec<StreamId> = loop {
        let s: Vec<StreamId> = bus
            .list()
            .into_iter()
            .filter(|s| {
                s.kind == StreamKind::Iq
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
    let (mut rx0, mut rx1) = (
        bus.subscribe(&streams[0]).unwrap(),
        bus.subscribe(&streams[1]).unwrap(),
    );

    // Blocks of both streams by start time, until three blocks were seen on both.
    let mut seen: [HashMap<u64, Arc<[Complex<f32>]>>; 2] = Default::default();
    let both = |seen: &[HashMap<u64, Arc<[Complex<f32>]>>; 2]| {
        seen[0]
            .keys()
            .filter(|k| seen[1].contains_key(k))
            .copied()
            .collect::<Vec<u64>>()
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while both(&seen).len() < 3 {
            let (i, r) = tokio::select! {
                r = rx0.recv() => (0, r),
                r = rx1.recv() => (1, r),
            };
            match r {
                Ok(f) => {
                    let Payload::Iq(b) = &f.payload else {
                        panic!("iq expected")
                    };
                    seen[i].insert(f.provenance.span.start_ns.0, b.samples.clone());
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("job stream ended: {e}"),
            }
        }
    })
    .await
    .expect("three gain blocks within 2 s");

    // The tone fixture has magnitude 0.5 throughout, so the gain output is the stream at
    // 0.25.
    let mean = |s: &[Complex<f32>]| s.iter().map(|c| c.norm()).sum::<f32>() / s.len() as f32;
    let k0 = both(&seen)[0];
    let (src_i, out_i) = if mean(&seen[0][&k0]) > mean(&seen[1][&k0]) {
        (0, 1)
    } else {
        (1, 0)
    };
    let mut checked = 0;
    for k in both(&seen) {
        let (a, o) = (&seen[src_i][&k], &seen[out_i][&k]);
        assert_eq!(a.len(), 4800);
        assert_eq!(o.len(), a.len());
        assert!((mean(a) - 0.5).abs() < 1e-3, "source mean {}", mean(a));
        for (x, y) in a.iter().zip(o.iter()) {
            assert!(close(*y, x * 0.5), "{y} != 0.5 * {x}");
        }
        checked += 1;
    }
    eprintln!("python gain in a job: {checked} blocks of 4800 samples equal 0.5 x the source's");
    let rec = node.job_list().into_iter().find(|r| r.id == job).unwrap();
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.graph.nodes[0].kind, "gain");
    node.apply(Op::JobStop { job }).await.unwrap();
}

#[tokio::test]
async fn python_exception_becomes_stage_failure() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: python3 not found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("raiser.py");
    let sdk = repo().join("sdk/python");
    std::fs::write(
        &script,
        format!(
            "import sys\n\
             sys.path.insert(0, {sdk:?})\n\
             import zeta_plugin\n\
             def on_frame(params, frame): raise ValueError(\"bad block\")\n\
             zeta_plugin.run(\"raiser\", [\"iq\"], [\"iq\"], {{\"type\": \"object\", \"additionalProperties\": False}}, on_frame)\n",
            sdk = sdk.display().to_string()
        ),
    )
    .unwrap();
    let stage = probe("raiser", py(&script)).await.unwrap();

    let bus = Arc::new(StreamBus::new(64));
    let out = bus.register(StreamKind::Iq, prov());
    let (tx, rx_in) = broadcast::channel(16);
    tx.send(iq_frame(&[Complex::new(1.0, 0.0)])).unwrap();
    let run = tokio::spawn(async move {
        stage
            .run(
                json!({}),
                vec![rx_in],
                vec![out],
                bus,
                Default::default(),
                ctx(),
            )
            .await
    });
    let r = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("stage ends")
        .unwrap();
    drop(tx);
    let Err(StageError::Failed(m)) = r else {
        panic!("expected Failed, got {r:?}");
    };
    for want in ["plugin raiser", "ValueError: bad block"] {
        assert!(m.contains(want), "{want:?} not in {m:?}");
    }
}

/// The SDK's own codec tests: encode and decode against hand-built protocol 1 bytes.
#[test]
fn python_sdk_unit_tests_pass() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: python3 not found");
        return;
    }
    let sdk = repo().join("sdk/python");
    let out = Command::new(python3())
        .args(["-I", "-m", "unittest", "discover", "-s"])
        .arg(&sdk)
        .args(["-p", "test_*.py", "-v"])
        .current_dir(&sdk)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    eprintln!("{err}");
    assert!(out.status.success(), "python unittest failed:\n{err}");
    assert!(err.contains("OK"), "{err}");
}

/// The SDK tests' hand-built iq, audio and event frames are byte for byte what the host's
/// encoder writes for the same frames.
#[test]
fn python_sdk_fixtures_are_the_hosts_bytes() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipped: python3 not found");
        return;
    }
    let sdk = repo().join("sdk/python");
    let out = Command::new(python3())
        .args(["-I", "-c"])
        .arg(format!(
            "import sys; sys.path.insert(0, {sdk:?}); import test_sdk as t; \
             print(t.IQ.hex(), t.AUDIO.hex(), t.AUDIO_STEREO.hex(), t.EVENT.hex())",
            sdk = sdk.display().to_string()
        ))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hex = String::from_utf8(out.stdout).unwrap();
    let theirs: Vec<&str> = hex.split_whitespace().collect();
    let prov = |job: Option<&str>, channel: Option<&str>| Provenance {
        source: SourceId("src".into()),
        job: job.map(|j| JobId(j.into())),
        channel: channel.map(|c| ChannelId(c.into())),
        span: Span {
            start_ns: Timestamp(10),
            end_ns: Timestamp(20),
        },
    };
    let ours = [
        Frame::new(
            StreamId("s1".into()),
            prov(None, Some("c1")),
            Payload::Iq(IqBlock {
                t0: Timestamp(10),
                sample_rate_hz: Hz(48_000),
                center_hz: Hz(7_100_000),
                samples: [Complex::new(1.0, 2.0), Complex::new(-0.5, 0.25)]
                    .into_iter()
                    .collect(),
            }),
        ),
        Frame::new(
            StreamId("s1".into()),
            prov(Some("j1"), None),
            Payload::Audio {
                sample_rate_hz: Hz(12_000),
                channels: 1,
                samples: [0.25f32, -0.5].into_iter().collect(),
            },
        ),
        Frame::new(
            StreamId("s1".into()),
            prov(Some("j1"), None),
            Payload::Audio {
                sample_rate_hz: Hz(12_000),
                channels: 2,
                samples: [0.25f32, -0.5, 0.5, 0.25].into_iter().collect(),
            },
        ),
        Frame::new(
            StreamId("s1".into()),
            prov(None, None),
            Payload::Event(json!({"label": "ft8", "freq_hz": 7_074_000})),
        ),
    ];
    assert_eq!(theirs.len(), ours.len());
    for (t, f) in theirs.iter().zip(&ours) {
        let bytes = zeta_plugin::wire::encode_frame(f, zeta_plugin::wire::PROTOCOL_MIN).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(*t, hex);
    }
}

fn ctx() -> zeta_core::stage::StageContext {
    zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into()))
}
