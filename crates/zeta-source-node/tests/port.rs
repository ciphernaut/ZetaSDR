//! `NodePort` against a loopback node serving the looping `tone` source.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, timeout};
use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::{Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, SampleFormat, SourceId};
use zeta_source_node::{NodePort, NodeSourceConfig, open};
use zeta_ws::testing::{TestServer, spawn};

/// The `tone` fixture's centre frequency.
const TONE_CENTER_HZ: u64 = 100_000_000;
/// `|0.5 e^{jwt}|^2`.
const TONE_POWER: f64 = 0.25;

fn cfg(srv: &TestServer, remote: &str) -> NodeSourceConfig {
    NodeSourceConfig {
        id: SourceId("far".to_string()),
        url: srv.url.clone(),
        secret: "s3cret".to_string(),
        remote_source: SourceId(remote.to_string()),
        max_bandwidth_hz: 250_000,
    }
}

async fn open_tone(srv: &TestServer) -> NodePort {
    open(cfg(srv, "tone")).await.expect("opens")
}

fn tone_band() -> Tuning {
    Tuning {
        center_hz: Hz(TONE_CENTER_HZ),
        bandwidth_hz: Hz(48_000),
    }
}

async fn remote_jobs(srv: &TestServer) -> Vec<JobRecord> {
    match srv.node.apply(Op::JobList {}).await.expect("job_list") {
        OpResult::Jobs(jobs) => jobs,
        other => panic!("expected jobs, got {other:?}"),
    }
}

/// Poll the remote job list until `pred` holds or `within` passes.
async fn wait_jobs(srv: &TestServer, within: Duration, pred: impl Fn(&[JobRecord]) -> bool) {
    let deadline = Instant::now() + within;
    loop {
        let jobs = remote_jobs(srv).await;
        if pred(&jobs) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "remote jobs never matched: {jobs:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn open_copies_capabilities_as_remote_push() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = open_tone(&srv).await;
    let remote = match srv.node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(s) => s.into_iter().find(|s| s.id.0 == "tone").unwrap(),
        other => panic!("expected sources, got {other:?}"),
    };
    let caps = port.capabilities();
    assert_eq!(port.id(), &SourceId("far".to_string()));
    assert!(caps.remote);
    assert!(caps.push);
    assert_eq!(caps.formats, vec![SampleFormat::Cf32]);
    assert_eq!(caps.tune_min_hz, remote.capabilities.tune_min_hz);
    assert_eq!(caps.tune_max_hz, remote.capabilities.tune_max_hz);
    // min(remote 48 kHz, configured 250 kHz)
    assert_eq!(*caps.max_bandwidth_hz.value(), Hz(48_000));
    assert_eq!(port.current(), None);
    assert_eq!(port.fault(), None);
    drop(port);
    srv.stop().await;
}

#[tokio::test]
async fn tune_starts_remote_passthrough_and_streams_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = open_tone(&srv).await;
    let got = port.tune(tone_band()).await.expect("tunes");
    assert_eq!(got.bandwidth_hz, Hz(48_000));
    assert_eq!(got.center_hz, Hz(TONE_CENTER_HZ));
    assert_eq!(port.current(), Some(got));

    let jobs = remote_jobs(&srv).await;
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].kind, "passthrough");
    assert_eq!(jobs[0].source, SourceId("tone".to_string()));
    assert_eq!(jobs[0].state, JobState::Running);

    let mut rx = port.subscribe();
    let mut blocks = Vec::new();
    while blocks.len() < 3 {
        let b = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a block within 5 s")
            .expect("receives");
        blocks.push(b);
    }
    for b in &blocks {
        assert_eq!(b.samples.len(), 4800);
        assert_eq!(b.sample_rate_hz, Hz(48_000));
        assert_eq!(b.center_hz, Hz(TONE_CENTER_HZ));
        let p = b
            .samples
            .iter()
            .map(|s| f64::from(s.norm_sqr()))
            .sum::<f64>()
            / b.samples.len() as f64;
        assert!((p - TONE_POWER).abs() < 0.01, "power {p}");
    }
    assert_eq!(blocks[1].t0.0 - blocks[0].t0.0, 100_000_000);
    assert_eq!(blocks[2].t0.0 - blocks[1].t0.0, 100_000_000);
    assert_eq!(port.losses(), (0, 0));
    drop(rx);
    drop(port);
    srv.stop().await;
}

#[tokio::test]
async fn bandwidth_over_cap_is_out_of_range_without_remote_job() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = open_tone(&srv).await;
    // The cap is the advertised max_bandwidth_hz: min(remote 48 kHz, configured 250 kHz).
    let cap = port.capabilities().max_bandwidth_hz.value().0;
    assert_eq!(cap, 48_000);
    let err = port
        .tune(Tuning {
            center_hz: Hz(TONE_CENTER_HZ),
            bandwidth_hz: Hz(300_000),
        })
        .await
        .unwrap_err();
    match err {
        PortError::OutOfRange {
            field,
            value,
            min,
            max,
            suggested,
        } => {
            assert_eq!(field, "bandwidth_hz");
            assert_eq!(value, 300_000);
            assert_eq!(min, 1);
            assert_eq!(max, cap);
            assert_eq!(suggested, Some(cap));
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
    assert!(remote_jobs(&srv).await.is_empty());
    drop(port);
    srv.stop().await;
}

#[tokio::test]
async fn retune_stops_previous_remote_job() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = open_tone(&srv).await;
    port.tune(tone_band()).await.expect("first tune");
    let first = remote_jobs(&srv).await[0].id.clone();
    // A different request (an identical one is a no-op, see below).
    port.tune(Tuning {
        bandwidth_hz: Hz(24_000),
        ..tone_band()
    })
    .await
    .expect("second tune");
    let jobs = remote_jobs(&srv).await;
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    let state =
        |id: &zeta_core::types::JobId| jobs.iter().find(|j| &j.id == id).unwrap().state.clone();
    assert_eq!(state(&first), JobState::Stopped);
    let second = jobs.iter().find(|j| j.id != first).unwrap().id.clone();
    assert_eq!(state(&second), JobState::Running);
    assert_eq!(port.fault(), None, "a retune does not fault the port");
    drop(port);
    srv.stop().await;
}

#[tokio::test]
async fn close_stops_remote_job_before_returning() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = open_tone(&srv).await;
    port.tune(tone_band()).await.expect("tunes");
    port.close().await;
    let jobs = remote_jobs(&srv).await;
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].state, JobState::Stopped);
    assert_eq!(port.current(), None);
    port.close().await;
    drop(port);
    srv.stop().await;
}

#[tokio::test]
async fn drop_stops_remote_job() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = open_tone(&srv).await;
    port.tune(tone_band()).await.expect("tunes");
    drop(port);
    wait_jobs(&srv, Duration::from_secs(2), |jobs| {
        jobs.len() == 1 && jobs[0].state == JobState::Stopped
    })
    .await;
    srv.stop().await;
}

#[tokio::test]
async fn server_stop_faults_port_and_names_url() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let url = srv.url.clone();
    let port = open_tone(&srv).await;
    port.tune(tone_band()).await.expect("tunes");
    let mut rx = port.subscribe();
    timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a block within 5 s")
        .expect("receives");
    srv.stop().await;
    let deadline = Instant::now() + Duration::from_secs(3);
    let fault = loop {
        if let Some(f) = port.fault() {
            break f;
        }
        assert!(Instant::now() < deadline, "no fault within 3 s");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(fault.contains(&url), "{fault}");
    assert!(fault.contains("stream closed"), "{fault}");
    assert!(!fault.contains("s3cret"), "{fault}");
}

#[tokio::test]
async fn open_against_unknown_remote_source_names_known() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let err = match open(cfg(&srv, "nope")).await {
        Ok(_) => panic!("opened an unknown remote source"),
        Err(e) => e,
    };
    match &err {
        PortError::Unsupported(s) => {
            assert!(s.contains("tone"), "{s}");
            assert!(s.contains("nope"), "{s}");
            assert!(s.contains(&srv.url), "{s}");
            assert!(!s.contains("s3cret"), "{s}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
    srv.stop().await;
}

#[test]
fn config_debug_hides_secret() {
    let c = NodeSourceConfig {
        id: SourceId("far".to_string()),
        url: "http://127.0.0.1:1".to_string(),
        secret: "s3cret".to_string(),
        remote_source: SourceId("tone".to_string()),
        max_bandwidth_hz: 250_000,
    };
    let s = format!("{c:?}");
    assert!(s.contains(r#"secret: "***""#), "{s}");
    assert!(!s.contains("s3cret"), "{s}");
}

#[tokio::test]
async fn late_subscriber_after_all_left_still_gets_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = open_tone(&srv).await;
    port.tune(tone_band()).await.expect("tunes");
    let mut rx = port.subscribe();
    timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a block within 5 s")
        .expect("receives");
    drop(rx);
    // Several 100 ms blocks go out with no receiver.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut rx = port.subscribe();
    timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("a block within 2 s after resubscribing")
        .expect("receives");
    assert_eq!(port.fault(), None);
    drop(rx);
    drop(port);
    srv.stop().await;
}

#[tokio::test]
async fn remote_rate_over_cap_is_rejected_and_job_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    // The tone ships 48 kHz whatever band is asked for; a 24 kHz cap must refuse it.
    let port = open(NodeSourceConfig {
        max_bandwidth_hz: 24_000,
        ..cfg(&srv, "tone")
    })
    .await
    .expect("opens");
    assert_eq!(port.capabilities().max_bandwidth_hz.value().0, 24_000);
    let err = port
        .tune(Tuning {
            center_hz: Hz(TONE_CENTER_HZ),
            bandwidth_hz: Hz(24_000),
        })
        .await
        .unwrap_err();
    let msg = err.to_string();
    match err {
        PortError::OutOfRange {
            field,
            value,
            min,
            max,
            suggested,
        } => {
            assert_eq!(field, "bandwidth_hz");
            assert_eq!(value, 24_000);
            assert_eq!(min, 1);
            assert_eq!(max, 24_000);
            assert_eq!(suggested, Some(48_000));
        }
        other => panic!("expected OutOfRange, got {other:?}"),
    }
    assert!(msg.contains("remote ships 48000 Hz"), "{msg}");
    assert!(msg.contains("max_bandwidth_hz=48000"), "{msg}");
    assert_eq!(port.current(), None);
    let jobs = remote_jobs(&srv).await;
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].kind, "passthrough");
    assert_eq!(jobs[0].state, JobState::Stopped);
    drop(port);
    srv.stop().await;
}

#[tokio::test]
async fn identical_tune_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = open_tone(&srv).await;
    let a = port.tune(tone_band()).await.expect("first tune");
    let b = port.tune(tone_band()).await.expect("second tune");
    assert_eq!(a, b);
    let jobs = remote_jobs(&srv).await;
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].state, JobState::Running);
    drop(port);
    srv.stop().await;
}

/// A local node whose only source is `port`, with the passthrough stage.
async fn local_node(dir: &std::path::Path, port: Arc<dyn Port>) -> Node {
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    let cfg = NodeConfig {
        source_positions: Default::default(),
        burst_policy: Default::default(),
        state_dir: dir.join("local"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: false,
        bank_factory: None,
        modes_dir: None,
    };
    Node::open(cfg, registry, vec![port]).await.expect("node")
}

fn passthrough_graph() -> Graph {
    Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("p".into()),
            kind: "passthrough".into(),
            params: serde_json::json!({"kind": "iq"}),
        }],
        edges: vec![],
    }
}

fn running_passthroughs(jobs: &[JobRecord]) -> usize {
    jobs.iter()
        .filter(|j| j.kind == "passthrough" && j.state == JobState::Running)
        .count()
}

#[tokio::test]
async fn job_stop_releases_remote_lease_after_grace() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = Arc::new(open_tone(&srv).await);
    let node = local_node(dir.path(), port.clone()).await;
    let far = SourceId("far".to_string());
    let job = node
        .job_start(
            "monitor",
            far.clone(),
            tone_band(),
            passthrough_graph(),
            None,
            false,
        )
        .await
        .expect("job starts");
    assert_eq!(running_passthroughs(&remote_jobs(&srv).await), 1);
    node.job_stop(&job).await.expect("job stops");
    // Inside the grace the remote job still runs; after it, it is released.
    assert!(port.current().is_some(), "released before the grace");
    wait_jobs(&srv, Duration::from_secs(4), |jobs| {
        running_passthroughs(jobs) == 0
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(1);
    while port.current().is_some() {
        assert!(Instant::now() < deadline, "current not cleared");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // A new job tunes from scratch and streams again.
    let mut rx = port.subscribe();
    node.job_start(
        "monitor",
        far,
        tone_band(),
        passthrough_graph(),
        None,
        false,
    )
    .await
    .expect("second job starts");
    assert_eq!(running_passthroughs(&remote_jobs(&srv).await), 1);
    timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a block within 5 s")
        .expect("receives");
    assert!(port.current().is_some());
    drop(rx);
    drop(node);
    port.close().await;
    srv.stop().await;
}

#[tokio::test]
async fn restart_within_grace_keeps_the_remote_job() {
    let dir = tempfile::tempdir().unwrap();
    let srv = spawn(dir.path()).await;
    let port = Arc::new(open_tone(&srv).await);
    let node = local_node(dir.path(), port.clone()).await;
    let far = SourceId("far".to_string());
    let job = node
        .job_start(
            "monitor",
            far.clone(),
            tone_band(),
            passthrough_graph(),
            None,
            false,
        )
        .await
        .expect("job starts");
    node.job_stop(&job).await.expect("job stops");
    node.job_start(
        "monitor",
        far,
        tone_band(),
        passthrough_graph(),
        None,
        false,
    )
    .await
    .expect("restarts within the grace");
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    let jobs = remote_jobs(&srv).await;
    assert_eq!(jobs.len(), 1, "one remote job across the restart: {jobs:?}");
    assert_eq!(running_passthroughs(&jobs), 1);
    assert!(port.current().is_some());
    drop(node);
    port.close().await;
    srv.stop().await;
}
