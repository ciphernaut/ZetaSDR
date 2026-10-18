//! The plugin host against `zeta-plugin-fake`, one mode per failure.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use num_complex::Complex;
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeta_core::graph::{Graph, GraphError, GraphId, Node, NodeId, validate};
use zeta_core::port::IqBlock;
use zeta_core::stage::{Backend, Stage, StageError, StageRegistry};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{
    ChannelId, Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};
use zeta_plugin::host::{ExternalStage, PluginCommand, probe};

fn fake(mode: &str) -> PluginCommand {
    PluginCommand {
        program: env!("CARGO_BIN_EXE_zeta-plugin-fake").into(),
        args: vec!["--mode".into(), mode.into()],
    }
}

/// `fake(mode)` that appends each process id it starts to `pidfile`.
fn fake_pids(mode: &str, pidfile: &Path) -> PluginCommand {
    let mut c = fake(mode);
    c.args
        .extend(["--pidfile".into(), pidfile.display().to_string()]);
    c
}

/// Every process id in `pidfile` no longer exists (killed and reaped).
fn all_gone(pidfile: &Path) -> bool {
    let pids = std::fs::read_to_string(pidfile).unwrap();
    assert!(!pids.trim().is_empty(), "no pid recorded");
    pids.lines()
        .all(|p| !Path::new(&format!("/proc/{}", p.trim())).exists())
}

fn prov(job: &str) -> Provenance {
    Provenance {
        source: SourceId("src".into()),
        job: Some(JobId(job.into())),
        channel: Some(ChannelId("c1".into())),
        span: Span {
            start_ns: Timestamp(1_000),
            end_ns: Timestamp(2_000),
        },
    }
}

fn iq(i: usize) -> Frame {
    Frame::new(
        StreamId("in".into()),
        prov("j1"),
        Payload::Iq(IqBlock {
            t0: Timestamp(1_000),
            sample_rate_hz: Hz(48_000),
            center_hz: Hz(100_000_000),
            samples: (0..4 + i)
                .map(|k| Complex::new(k as f32 * 0.5, -(i as f32)))
                .collect(),
        }),
    )
}

struct Rig {
    tx: broadcast::Sender<Frame>,
    bus: Arc<StreamBus>,
    out: StreamId,
    rx_out: broadcast::Receiver<Frame>,
    cancel: CancellationToken,
    run: JoinHandle<Result<(), StageError>>,
}

/// Run `stage` on a local input broadcast of `cap` (pre-loaded with `preload` frames) and one
/// bus output of `kind`.
fn start(stage: ExternalStage, kind: StreamKind, cap: usize, preload: usize) -> Rig {
    start_with(stage, kind, cap, preload, json!({}))
}

fn start_with(
    stage: ExternalStage,
    kind: StreamKind,
    cap: usize,
    preload: usize,
    params: serde_json::Value,
) -> Rig {
    let bus = Arc::new(StreamBus::new(64));
    let out = bus.register(kind, prov("j1"));
    let rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(cap);
    for i in 0..preload {
        tx.send(iq(i)).unwrap();
    }
    let cancel = CancellationToken::new();
    let (b, o, c) = (bus.clone(), out.clone(), cancel.clone());
    let run = tokio::spawn(async move { stage.run(params, vec![rx_in], vec![o], b, c).await });
    Rig {
        tx,
        bus,
        out,
        rx_out,
        cancel,
        run,
    }
}

async fn failed(run: JoinHandle<Result<(), StageError>>) -> String {
    match tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .expect("stage ends")
        .unwrap()
    {
        Err(StageError::Failed(m)) => m,
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn echo_plugin_passes_iq_through_unchanged() {
    let stage = probe("fake", fake("echo")).await.unwrap();
    assert_eq!(stage.spec().kind, "fake");
    assert_eq!(stage.spec().inputs, vec![StreamKind::Iq]);
    assert_eq!(stage.spec().outputs, vec![StreamKind::Iq]);
    assert_eq!(stage.backend(), Backend::Cpu);

    let mut rig = start(stage, StreamKind::Iq, 16, 0);
    let sent: Vec<Frame> = (0..3).map(iq).collect();
    for f in &sent {
        rig.tx.send(f.clone()).unwrap();
    }
    for f in &sent {
        let got = tokio::time::timeout(Duration::from_secs(5), rig.rx_out.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.stream, rig.out);
        assert_eq!(got.kind, StreamKind::Iq);
        assert_eq!(got.provenance, f.provenance);
        assert_eq!(got.provenance.job, Some(JobId("j1".into())));
        let (Payload::Iq(a), Payload::Iq(b)) = (&got.payload, &f.payload) else {
            panic!("iq expected");
        };
        assert_eq!(a.samples, b.samples);
        assert_eq!(
            (a.t0, a.sample_rate_hz, a.center_hz),
            (b.t0, b.sample_rate_hz, b.center_hz)
        );
    }
    drop(rig.tx);
    let r = tokio::time::timeout(Duration::from_secs(5), rig.run)
        .await
        .unwrap()
        .unwrap();
    assert!(r.is_ok(), "{r:?}");
}

#[tokio::test]
async fn plugin_crash_mid_frame_fails_stage_with_byte_counts() {
    let stage = probe("fake", fake("crash-mid-frame")).await.unwrap();
    let mut rig = start(stage, StreamKind::Iq, 16, 0);
    rig.tx.send(iq(0)).unwrap();
    let m = failed(rig.run).await;
    for want in ["plugin fake", "body", "12", "8000", "status 3"] {
        assert!(m.contains(want), "{want:?} not in {m:?}");
    }
    assert!(matches!(
        rig.rx_out.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn plugin_wrong_kind_frame_fails_stage() {
    let stage = probe("fake", fake("wrong-kind")).await.unwrap();
    let mut rig = start(stage, StreamKind::Iq, 16, 0);
    rig.tx.send(iq(0)).unwrap();
    let m = failed(rig.run).await;
    assert_eq!(
        m,
        format!(
            "plugin fake sent a audio frame on stream {} declared iq",
            rig.out.0
        )
    );
    assert!(matches!(
        rig.rx_out.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(rig.bus.dropped(), 0);
}

#[tokio::test]
async fn probe_times_out_without_hello() {
    let dir = tempfile::tempdir().unwrap();
    let pids = dir.path().join("pids");
    let t = Instant::now();
    let e = probe("fake", fake_pids("no-hello", &pids))
        .await
        .err()
        .expect("probe fails")
        .to_string();
    assert!(t.elapsed() < Duration::from_secs(6), "{:?}", t.elapsed());
    assert!(e.contains("hello") && e.contains("5000 ms"), "{e}");
    assert!(all_gone(&pids), "no-hello child still alive");
}

#[tokio::test]
async fn probe_rejects_loose_params_schema() {
    let e = probe("fake", fake("loose-schema"))
        .await
        .err()
        .expect("probe fails")
        .to_string();
    assert!(e.contains("additionalProperties"), "{e}");
}

#[tokio::test]
async fn probe_rejects_kind_mismatch() {
    let e = probe("gain", fake("echo"))
        .await
        .err()
        .expect("probe fails")
        .to_string();
    assert!(e.contains("\"gain\"") && e.contains("\"fake\""), "{e}");
}

#[tokio::test]
async fn probe_reports_a_missing_program() {
    let cmd = PluginCommand {
        program: "/nonexistent/zeta-plugin".into(),
        args: vec![],
    };
    let e = probe("fake", cmd)
        .await
        .err()
        .expect("probe fails")
        .to_string();
    assert!(e.contains("/nonexistent/zeta-plugin"), "{e}");
}

#[tokio::test]
async fn plugin_error_message_fails_stage() {
    let stage = probe("fake", fake("error")).await.unwrap();
    let rig = start(stage, StreamKind::Iq, 16, 0);
    assert_eq!(failed(rig.run).await, "plugin fake: boom");
}

#[tokio::test]
async fn lagged_input_is_reported_to_plugin() {
    let stage = probe("fake", fake("lag-report")).await.unwrap();
    assert_eq!(stage.spec().outputs, vec![StreamKind::Event]);
    // Five frames into a capacity-2 input before the stage reads: three are lost.
    let mut rig = start(stage, StreamKind::Event, 2, 5);
    drop(rig.tx);
    let r = tokio::time::timeout(Duration::from_secs(5), rig.run)
        .await
        .unwrap()
        .unwrap();
    assert!(r.is_ok(), "{r:?}");
    let mut events = Vec::new();
    while let Ok(f) = rig.rx_out.try_recv() {
        let Payload::Event(v) = f.payload else {
            panic!("event expected");
        };
        events.push(v);
    }
    assert_eq!(
        events,
        vec![
            json!({"lagged": 3}),
            json!({"samples": 7}),
            json!({"samples": 8})
        ]
    );
}

#[tokio::test]
async fn stalled_plugin_drops_input_and_is_told_never_blocking_the_host() {
    let stage = probe("fake", fake("stall")).await.unwrap();
    // 100 frames of 64 KiB, far more than the writer queue and the pipe hold while the
    // plugin is not reading. The input never lags: the host keeps draining it.
    let mut rig = start(stage, StreamKind::Event, 128, 0);
    let big = |i: usize| {
        let mut f = iq(i);
        if let Payload::Iq(b) = &mut f.payload {
            b.samples = vec![Complex::new(0.0, 0.0); 8192].into();
        }
        f
    };
    for i in 0..100 {
        rig.tx.send(big(i)).unwrap();
    }
    drop(rig.tx);
    let r = tokio::time::timeout(Duration::from_secs(10), rig.run)
        .await
        .unwrap()
        .unwrap();
    assert!(r.is_ok(), "{r:?}");
    let (mut delivered, mut lagged) = (0u64, 0u64);
    while let Ok(f) = rig.rx_out.try_recv() {
        let Payload::Event(v) = f.payload else {
            panic!("event expected");
        };
        if let Some(n) = v.get("lagged") {
            lagged += n.as_u64().unwrap();
        } else {
            assert_eq!(v, json!({"samples": 8192}));
            delivered += 1;
        }
    }
    assert!(lagged >= 1, "nothing was dropped: {delivered} delivered");
    assert_eq!(delivered + lagged, 100, "every frame delivered or reported");
}

#[tokio::test]
async fn cancel_kills_a_plugin_that_ignores_stop() {
    let dir = tempfile::tempdir().unwrap();
    let pids = dir.path().join("pids");
    let stage = probe("fake", fake_pids("ignore-stop", &pids))
        .await
        .unwrap();
    let mut rig = start(stage, StreamKind::Iq, 16, 0);
    rig.tx.send(iq(0)).unwrap();
    // Running for certain: its echo has come back.
    tokio::time::timeout(Duration::from_secs(5), rig.rx_out.recv())
        .await
        .unwrap()
        .unwrap();
    let t = Instant::now();
    rig.cancel.cancel();
    let r = tokio::time::timeout(Duration::from_secs(3), rig.run)
        .await
        .expect("cancel within 3 s")
        .unwrap();
    assert!(matches!(r, Err(StageError::Cancelled)), "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(all_gone(&pids), "ignore-stop child still alive");
}

#[tokio::test]
async fn oversized_run_message_is_refused_before_sending() {
    let stage = probe("fake", fake("echo")).await.unwrap();
    let rig = start_with(
        stage,
        StreamKind::Iq,
        16,
        0,
        json!({"x": "a".repeat(70_000)}),
    );
    let m = failed(rig.run).await;
    assert!(m.starts_with("plugin fake: run message of "), "{m}");
    assert!(m.contains("65536-byte limit"), "{m}");
}

#[tokio::test]
async fn graph_validation_uses_plugin_schema() {
    let mut reg = StageRegistry::new();
    reg.register(Arc::new(probe("fake", fake("echo")).await.unwrap()));
    let graph = |params| Graph {
        id: GraphId::new(),
        nodes: vec![Node {
            id: NodeId("p".into()),
            kind: "fake".into(),
            params,
        }],
        edges: vec![],
    };
    assert!(matches!(
        validate(&graph(json!({"bogus": 1})), &reg),
        Err(GraphError::BadParams { .. })
    ));
    assert_eq!(validate(&graph(json!({})), &reg), Ok(()));
}
