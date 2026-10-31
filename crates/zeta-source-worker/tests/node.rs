//! A `Node` over a `WorkerPort` whose worker is the fake binary: the node recovers its
//! jobs when the worker crashes and is respawned, a device absent at startup attaches
//! faulted when an earlier open cached its capabilities, and dropping the node ends the
//! worker. These are the node's half of `zeta soapy-worker`: the real subcommand needs a
//! device, the fake speaks the same protocol.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use num_complex::Complex;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig, load_source};
use zeta_core::ops::{Op, OpResult, SourceInfo};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId, Timestamp};
use zeta_source_worker::port::{Attach, Timings, WorkerConfig, WorkerPort};

const FAKE: &str = env!("CARGO_BIN_EXE_zeta-fake-worker");

/// The POCSAG recording: a 1200-baud page every 2 s, 50 kHz above the centre. The rate is
/// kept low so the worker's pipe and the node's ring stay light on a loaded test host.
const RATE_HZ: u32 = 256_000;
const CENTER_HZ: u64 = 148_587_500;
const OFFSET_HZ: f64 = 50_000.0;
const CYCLE: usize = 2 * RATE_HZ as usize;

/// The supervisor's clock shortened as `tests/supervisor.rs` does, so a crash and its
/// respawn take a fraction of a second.
fn timings() -> Timings {
    Timings {
        hello: Duration::from_secs(3),
        backoff_first: Duration::from_millis(100),
        backoff_max: Duration::from_millis(300),
        alive_reset: Duration::from_secs(2),
        shutdown_grace: Duration::from_millis(500),
    }
}

struct Pocsag {
    _dir: tempfile::TempDir,
    meta: PathBuf,
}

fn pocsag() -> &'static Pocsag {
    static F: OnceLock<Pocsag> = OnceLock::new();
    F.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let mut iq =
            zeta_dsp::synth::pocsag_packet_iq(1_234_567, 3, "HELLO ZETA", 1200, RATE_HZ, OFFSET_HZ);
        assert!(iq.len() < CYCLE);
        iq.resize(CYCLE, Complex::new(0.0, 0.0));
        zeta_dsp::synth::awgn(&mut iq, 20.0, 1);
        let meta = CaptureMeta {
            sample_rate_hz: Hz(u64::from(RATE_HZ)),
            center_hz: Hz(CENTER_HZ),
            start: Timestamp(0),
            annotations: Vec::new(),
        };
        let meta = write_cf32(dir.path(), "pocsag", &meta, &iq).unwrap();
        Pocsag { _dir: dir, meta }
    })
}

fn f() -> SourceId {
    SourceId("f".into())
}

fn band() -> Tuning {
    Tuning {
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(u64::from(RATE_HZ)),
    }
}

/// The fake running `script` as source `f`, recording to `record`. The fake's
/// environment is scrubbed by the supervisor, so `env` (`NAME=VALUE` pairs) is set by a
/// `/bin/sh -c` in front of it.
fn cfg(script: &str, record: &Path, env: &str) -> WorkerConfig {
    let mut args = vec![
        "-c".to_string(),
        format!("{env} exec \"$0\" \"$@\""),
        FAKE.to_string(),
    ];
    args.extend(
        [
            "--id",
            "f",
            "--script",
            script,
            "--record",
            &record.display().to_string(),
        ]
        .map(String::from),
    );
    WorkerConfig {
        id: f(),
        program: PathBuf::from("/bin/sh"),
        args,
        timings: timings(),
    }
}

fn node_cfg(state: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: state.to_path_buf(),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r
}

async fn node_over(state: &Path, port: Arc<WorkerPort>) -> Node {
    Node::open(node_cfg(state), registry(), vec![port as Arc<dyn Port>])
        .await
        .unwrap()
}

async fn source(node: &Node) -> SourceInfo {
    match node.apply(Op::SourceList {}).await {
        Ok(OpResult::Sources(v)) => v.into_iter().find(|i| i.id == f()).unwrap(),
        r => panic!("source_list: {r:?}"),
    }
}

fn state(node: &Node, id: &JobId) -> Option<JobState> {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .map(|r| r.state)
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

async fn start_passthrough(node: &Node) -> JobId {
    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("n".into()),
            kind: "passthrough".into(),
            params: serde_json::json!({"kind": "iq"}),
        }],
        edges: vec![],
    };
    node.job_start("passthrough", f(), band(), graph, None, false)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_over_a_crashing_fake_worker_recovers_its_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!("file {},crash-after-ms 1500", pocsag().meta.display());
    let rec = dir.path().join("record");
    let (port, attach) = WorkerPort::open(cfg(&script, &rec, "ZETA_FAKE_CRASH_ONCE=1"), None)
        .await
        .unwrap();
    assert_eq!(attach, Attach::Hello);
    let node = node_over(&dir.path().join("state"), port).await;
    let op = Op::SourcePin {
        source: f(),
        center_hz: band().center_hz,
        bandwidth_hz: band().bandwidth_hz,
        replace: vec![],
    };
    assert!(node.apply(op).await.is_ok());
    let job = start_passthrough(&node).await;
    // Let every stage get going before taking the count, well inside the 1.5 s.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stages = node.debug_stage_count();
    assert!(stages > 0);
    assert_eq!(source(&node).await.worker.unwrap().restarts, 0);

    until(
        "the job fails with the crash",
        Duration::from_secs(10),
        || state(&node, &job) != Some(JobState::Running),
    )
    .await;
    assert_eq!(
        state(&node, &job),
        Some(JobState::Failed {
            reason: "source f: soapy worker for f died: signal 6 (SIGABRT)".into()
        })
    );

    until("the job runs again", Duration::from_secs(5), || {
        state(&node, &job) == Some(JobState::Running)
    })
    .await;
    let info = source(&node).await;
    assert_eq!(info.worker.unwrap().restarts, 1);
    assert_eq!(info.pin, Some(band()));
    assert_eq!(info.fault, None);
    until("the stages are back", Duration::from_secs(5), || {
        node.debug_stage_count() == stages
    })
    .await;
    let _ = node.job_stop(&job).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_with_the_device_absent_attaches_faulted_when_seen_before() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let rec = dir.path().join("record");
    // A first start with the device present caches its capabilities.
    {
        let (port, attach) = WorkerPort::open(cfg("stream", &rec, ""), None)
            .await
            .unwrap();
        assert_eq!(attach, Attach::Hello);
        let _node = node_over(&state_dir, port).await;
    }
    let cached = load_source(&state_dir.join("sources"), &f()).and_then(|r| r.capabilities);
    assert!(cached.is_some(), "the first open cached no capabilities");

    let (port, attach) = WorkerPort::open(cfg("not-found", &rec, ""), cached)
        .await
        .unwrap();
    assert_eq!(
        attach,
        Attach::NotFound {
            args: "fake".into()
        }
    );
    let node = node_over(&state_dir, port).await;
    assert_eq!(
        source(&node).await.fault.as_deref(),
        Some("device not found: fake")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_one_shot_source_list_leaves_no_worker_behind() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("record");
    let (port, _) = WorkerPort::open(cfg("stream", &rec, ""), None)
        .await
        .unwrap();
    let node = node_over(&dir.path().join("state"), port).await;
    let pid = source(&node).await.worker.unwrap().pid.unwrap();
    assert!(alive(pid));
    // A one-shot `source list` applies the op and drops its node.
    drop(node);
    // The shutdown grace (500 ms here) plus a margin for the node's tasks to wind down.
    until(
        "the worker ends with the node",
        Duration::from_millis(1500),
        || !alive(pid),
    )
    .await;
}
