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
use zeta_core::types::{Claim, Hz, JobId, SourceId, Timestamp};
use zeta_source_worker::port::{Attach, Timings, WorkerConfig, WorkerPort};
use zeta_source_worker::ring::{DEFAULT_RING_BYTES, MIN_SLOTS, SLOT_SAMPLES};

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
    timings_backoff(Duration::from_millis(100))
}

/// [`timings`] with the wait before a respawn at `backoff`.
fn timings_backoff(backoff: Duration) -> Timings {
    Timings {
        hello: Duration::from_secs(3),
        backoff_first: backoff,
        backoff_max: backoff.max(Duration::from_millis(300)),
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
        ring_bytes: DEFAULT_RING_BYTES,
    }
}

/// The ids of the streams the job's current run publishes: a relaunch registers new ones.
async fn streams_of(node: &Node, job: &JobId) -> Vec<String> {
    match node.apply(Op::StreamList {}).await {
        Ok(OpResult::Streams(v)) => v
            .into_iter()
            .filter(|s| s.provenance.job.as_ref() == Some(job))
            .map(|s| s.id.0)
            .collect(),
        r => panic!("stream_list: {r:?}"),
    }
}

fn node_cfg(state: &Path) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
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
    // A wait before the respawn well over the 200 ms the forwarder polls the fault at, so
    // the test can watch the record read `Failed` before the recovery relaunches it.
    let mut crashing = cfg(&script, &rec, "ZETA_FAKE_CRASH_ONCE=1");
    crashing.timings = timings_backoff(Duration::from_millis(800));
    let (port, attach) = WorkerPort::open(crashing, None).await.unwrap();
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

/// A worker back within milliseconds, so its fault could fall between two polls of a
/// job's forwarder: the port keeps reporting it until the node has seen the job end, so
/// the job is relaunched. No pin here: nothing but the relaunch tunes the new worker, and
/// a job that missed the crash would read `Running` on a radio that sends nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crash_shorter_than_the_fault_poll_still_relaunches_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!("file {},crash-after-ms 1500", pocsag().meta.display());
    let rec = dir.path().join("record");
    let mut crashing = cfg(&script, &rec, "ZETA_FAKE_CRASH_ONCE=1");
    crashing.timings = timings_backoff(Duration::from_millis(1));
    let (port, attach) = WorkerPort::open(crashing, None).await.unwrap();
    assert_eq!(attach, Attach::Hello);
    let node = node_over(&dir.path().join("state"), port).await;
    let job = start_passthrough(&node).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stages = node.debug_stage_count();
    let before = streams_of(&node, &job).await;
    assert!(!before.is_empty());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let streams = streams_of(&node, &job).await;
        if state(&node, &job) == Some(JobState::Running) && !streams.is_empty() && streams != before
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out: the job is relaunched, state {:?}, source {:?}",
            state(&node, &job),
            source(&node).await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let info = source(&node).await;
    assert_eq!(info.worker.unwrap().restarts, 1);
    assert_eq!(info.fault, None);
    assert_eq!(info.current, Some(band()));
    until("the stages are back", Duration::from_secs(5), || {
        node.debug_stage_count() == stages
    })
    .await;
    let _ = node.job_stop(&job).await;
}

/// A heal before the port is attached still counts: the watcher's first look takes it,
/// and only that recovery ends the fault the port holds after the hello.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_heal_before_the_port_is_attached_ends_the_held_fault() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!("file {},crash-after-ms 100", pocsag().meta.display());
    let rec = dir.path().join("record");
    let mut crashing = cfg(&script, &rec, "ZETA_FAKE_CRASH_ONCE=1");
    crashing.timings = timings_backoff(Duration::from_millis(1));
    let (port, attach) = WorkerPort::open(crashing, None).await.unwrap();
    assert_eq!(attach, Attach::Hello);
    let healed = port.healed().unwrap();
    until("the worker is back", Duration::from_secs(5), || {
        *healed.borrow() == 1
    })
    .await;
    assert!(port.fault().is_some(), "held until the node has seen it");

    let node = node_over(&dir.path().join("state"), port.clone()).await;
    until("the node ends the fault", Duration::from_secs(5), || {
        port.fault().is_none()
    })
    .await;
    assert_eq!(source(&node).await.fault, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cap_measured_on_a_respawn_reaches_the_source_record() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let rec = dir.path().join("record");
    // The first spawn asserts the mock's cap and crashes once; the respawn (the crash
    // marker beside the record now there) measures one.
    let env = format!(
        "export ZETA_FAKE_CRASH_ONCE=1; [ -e '{}.crashed' ] && export ZETA_FAKE_MEASURED_CAP=20480000;",
        rec.display()
    );
    let (port, attach) = WorkerPort::open(cfg("crash-after-ms 500", &rec, &env), None)
        .await
        .unwrap();
    assert_eq!(attach, Attach::Hello);
    let node = node_over(&state_dir, port).await;
    let cap = || {
        load_source(&state_dir.join("sources"), &f())
            .and_then(|r| r.capabilities)
            .map(|c| c.max_bandwidth_hz)
    };
    assert_eq!(cap(), Some(Claim::Asserted(Hz(20_000_000))));
    until(
        "the measured cap reaches sources/f.json",
        Duration::from_secs(10),
        || cap() == Some(Claim::Measured(Hz(20_480_000))),
    )
    .await;
    assert_eq!(
        source(&node).await.capabilities.max_bandwidth_hz,
        Claim::Measured(Hz(20_480_000))
    );
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

/// A child that outruns the node's reader drops blocks for a full ring, and the count
/// reaches `source_list` without faulting the source. The node copies each block out and
/// releases its slot at once, so nothing the node's subscribers do holds the ring; the
/// child is made to outrun the reader instead: the smallest ring (4 slots) at 20 Msps in
/// blocks of 256 samples, about 78 000 descriptors a second in bursts of a millisecond.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drops_reach_source_list() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("record");
    let mut c = cfg("stream", &rec, "ZETA_FAKE_READ_SAMPLES=256");
    c.ring_bytes = MIN_SLOTS * SLOT_SAMPLES * 8 + 64;
    let (port, _) = WorkerPort::open(c, None).await.unwrap();
    let node = node_over(&dir.path().join("state"), port).await;
    let op = Op::SourcePin {
        source: f(),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(20_000_000),
        replace: vec![],
    };
    assert!(node.apply(op).await.is_ok());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let info = loop {
        let info = source(&node).await;
        let w = info.worker.clone().unwrap();
        if w.transport_dropped > 0 {
            break info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no drops in 5 s: {w:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let w = info.worker.unwrap();
    let transport = w.transport.unwrap();
    assert_eq!(transport.path, zeta_core::port::TransportPath::Shm);
    assert_eq!(
        transport.ring_bytes,
        (MIN_SLOTS * SLOT_SAMPLES * 8 + 64) as u64
    );
    assert_eq!(info.fault, None);
    let json = serde_json::to_value(source(&node).await).unwrap();
    assert!(
        json["worker"]["transport_dropped"].as_u64().unwrap() > 0,
        "{json}"
    );
    assert_eq!(json["worker"]["transport"]["path"], "shm");
}
