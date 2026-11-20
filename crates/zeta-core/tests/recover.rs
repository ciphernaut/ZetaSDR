//! Recovery of a source whose port heals: the node re-applies the source's gain and pin
//! and relaunches, under their own ids, the jobs that failed with the source's fault
//! during this fault episode (attached jobs and a watch's decoders included), leaving
//! every other record alone. Run over a [`FaultablePort`] the tests fault and heal.

#![cfg(feature = "test-fixtures")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use zeta_core::node::Node;
use zeta_core::ops::{JobKind, Op, OpResult, SourceInfo};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::stream_in::StreamInStage;
use zeta_core::test_fixtures::FaultablePort;
use zeta_core::types::{Hz, JobId, SourceId, StreamId, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource, fixtures};

mod common;
use common::{MapStage, audio, cfg, same, until};

/// What a `FaultablePort` reports while the device is away, as a worker port does.
const UNPLUGGED: &str = "no samples for 1000 ms (device unplugged?)";
/// The tone recording's centre and rate.
const TONE_HZ: u64 = 100_000_000;
const TONE_RATE_HZ: u64 = 48_000;

/// A stage that fails at once for its own reason, not the source's.
struct FailStage(StageSpec);

#[async_trait]
impl Stage for FailStage {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: Value,
        _inputs: Vec<broadcast::Receiver<Frame>>,
        _outputs: Vec<StreamId>,
        _bus: Arc<StreamBus>,
        _cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        Err(StageError::Failed("own reason".into()))
    }
}

/// A stage with no input, publishing nothing until it is cancelled: a job of this alone
/// has no IQ forwarder, so nothing in it polls the port's fault.
struct IdleStage(StageSpec);

#[async_trait]
impl Stage for IdleStage {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: Value,
        _inputs: Vec<broadcast::Receiver<Frame>>,
        _outputs: Vec<StreamId>,
        _bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        cancel.cancelled().await;
        Err(StageError::Cancelled)
    }
}

/// The stand-ins (registered first, so they win over the dsp stages of the same kind),
/// then the dsp stages (`measure`, and the watch's `fft`, `detect` and decoders).
fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(StreamInStage::new()));
    r.register(MapStage::new(
        "audio_src",
        StreamKind::Iq,
        &[StreamKind::Audio],
        |_| vec![(0, audio())],
    ));
    r.register(MapStage::new(
        "denoise",
        StreamKind::Audio,
        &[StreamKind::Audio],
        same,
    ));
    r.register(MapStage::new(
        "vad",
        StreamKind::Audio,
        &[StreamKind::Audio, StreamKind::Event],
        |f| {
            vec![
                (0, f.payload.clone()),
                (
                    1,
                    Payload::Event(serde_json::json!({"protocol": "vad", "speech": true})),
                ),
            ]
        },
    ));
    r.register(MapStage::new(
        "transcribe",
        StreamKind::Audio,
        &[StreamKind::Event],
        |_| {
            vec![(
                0,
                Payload::Event(serde_json::json!({"protocol": "transcript", "text": "stand-in"})),
            )]
        },
    ));
    r.register(Arc::new(FailStage(StageSpec {
        kind: "fail_now",
        inputs: vec![StreamKind::Iq],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({"type": "object"}),
    })));
    r.register(Arc::new(IdleStage(StageSpec {
        kind: "idle_src",
        inputs: vec![],
        outputs: vec![StreamKind::Audio],
        params_schema: schemars::json_schema!({"type": "object"}),
    })));
    zeta_dsp::register_all(&mut r);
    r
}

/// A looping, real-time tone source `id`, tunable 1 MHz either side of its centre.
fn tone(dir: &Path, id: &str) -> Arc<FileSource> {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: Some((Hz(TONE_HZ - 1_000_000), Hz(TONE_HZ + 1_000_000))),
    };
    Arc::new(FileSource::open(SourceId(id.into()), &meta, opts).unwrap())
}

/// A node over a faultable tone source `s` (and the plain sources `others`), started.
async fn node_on(
    dir: &Path,
    resume: bool,
    others: &[&str],
) -> (Arc<Node>, Arc<FaultablePort>, Vec<Arc<FileSource>>) {
    let src = tone(dir, "s");
    let port = FaultablePort::new(src.clone());
    let mut srcs = vec![src];
    let mut ports: Vec<Arc<dyn Port>> = vec![port.clone()];
    for id in others {
        let s = tone(dir, id);
        ports.push(s.clone());
        srcs.push(s);
    }
    let node = Node::open(cfg(dir, resume), registry(), ports)
        .await
        .unwrap();
    for s in &srcs {
        s.start();
    }
    (Arc::new(node), port, srcs)
}

fn s() -> SourceId {
    SourceId("s".into())
}

fn band(bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(TONE_HZ),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

fn one_node(kind: &str, params: Value) -> Graph {
    Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("n".into()),
            kind: kind.into(),
            params,
        }],
        edges: vec![],
    }
}

async fn start(node: &Node, source: &str, kind: &str, params: Value) -> JobId {
    node.job_start(
        kind,
        SourceId(source.into()),
        band(TONE_RATE_HZ),
        one_node(kind, params),
        None,
        false,
    )
    .await
    .unwrap()
}

async fn start_passthrough(node: &Node, source: &str) -> JobId {
    start(
        node,
        source,
        "passthrough",
        serde_json::json!({"kind": "iq"}),
    )
    .await
}

/// A `measure` job through the op, so its graph is the node's expansion.
async fn start_measure(node: &Node) -> JobId {
    let op = Op::JobStart {
        kind: JobKind::Measure,
        source: s(),
        center_hz: Hz(TONE_HZ),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy: None,
        dwell_scale: None,
        elastic: false,
        replace: vec![],
        handler: None,
        needs: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start measure: {r:?}"),
    }
}

fn state(node: &Node, id: &JobId) -> Option<JobState> {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .map(|r| r.state)
}

fn failed_unplugged() -> Option<JobState> {
    Some(JobState::Failed {
        reason: format!("source s: {UNPLUGGED}"),
    })
}

async fn source(node: &Node) -> SourceInfo {
    source_of(node, &s()).await
}

async fn source_of(node: &Node, id: &SourceId) -> SourceInfo {
    match node.apply(Op::SourceList {}).await {
        Ok(OpResult::Sources(v)) => v.into_iter().find(|i| &i.id == id).unwrap(),
        r => panic!("source_list: {r:?}"),
    }
}

/// Fault `port` and wait for every one of `jobs` to fail with the fault.
async fn fault(node: &Node, port: &FaultablePort, jobs: &[&JobId]) {
    port.set_fault(Some(UNPLUGGED.into()));
    for id in jobs {
        until(
            "the job fails with the fault",
            Duration::from_secs(10),
            || state(node, id) != Some(JobState::Running),
        )
        .await;
        assert_eq!(state(node, id), failed_unplugged(), "job {}", id.0);
    }
}

async fn stop_all(node: &Node) {
    let running: Vec<JobId> = node
        .job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running)
        .map(|r| r.id)
        .collect();
    for id in running {
        let _ = node.job_stop(&id).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_heal_restores_pin_gain_and_relaunches_the_failed_jobs_under_their_ids() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _srcs) = node_on(dir.path(), true, &[]).await;
    let pin = band(TONE_RATE_HZ);
    let op = Op::SourcePin {
        source: s(),
        center_hz: pin.center_hz,
        bandwidth_hz: pin.bandwidth_hz,
        replace: vec![],
    };
    assert!(node.apply(op).await.is_ok());
    let op = Op::SourceSetGain {
        source: s(),
        gain_db: Some(20.0),
    };
    assert!(node.apply(op).await.is_ok());
    let a = start_passthrough(&node, "s").await;
    let b = start_measure(&node).await;
    // Let every stage get going before taking the count.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let stages = node.debug_stage_count();
    assert!(stages > 0);

    fault(&node, &port, &[&a, &b]).await;
    assert_eq!(source(&node).await.fault.as_deref(), Some(UNPLUGGED));
    let gains_before = port.gain_calls().len();

    port.heal();
    until("both run again", Duration::from_secs(5), || {
        state(&node, &a) == Some(JobState::Running) && state(&node, &b) == Some(JobState::Running)
    })
    .await;
    let info = source(&node).await;
    assert_eq!(info.pin, Some(pin));
    assert_eq!(info.fault, None);
    let gains = port.gain_calls();
    assert_eq!(gains.len(), gains_before + 1, "{gains:?}");
    assert_eq!(gains.last(), Some(&Some(20.0)));
    until("the stages are back", Duration::from_secs(5), || {
        node.debug_stage_count() == stages
    })
    .await;
    // The same ids, no new records.
    assert_eq!(node.job_list().len(), 2);
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_leaves_stopped_and_self_failed_jobs_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _srcs) = node_on(dir.path(), true, &[]).await;
    let a = start_passthrough(&node, "s").await;
    let c = start_passthrough(&node, "s").await;
    node.job_stop(&c).await.unwrap();
    let d = start(&node, "s", "fail_now", serde_json::json!({})).await;
    until("d fails", Duration::from_secs(5), || {
        state(&node, &d) != Some(JobState::Running)
    })
    .await;
    let own = Some(JobState::Failed {
        reason: "own reason".into(),
    });
    assert_eq!(state(&node, &d), own);

    fault(&node, &port, &[&a]).await;
    port.heal();
    until("a runs again", Duration::from_secs(5), || {
        state(&node, &a) == Some(JobState::Running)
    })
    .await;
    // Give a wrong relaunch the time to show.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state(&node, &c), Some(JobState::Stopped));
    assert_eq!(state(&node, &d), own);
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_failed_in_an_earlier_episode_is_not_recovered() {
    let dir = tempfile::tempdir().unwrap();
    // A record a node before this one left failed with the source's fault.
    let old = JobRecord {
        format: JOB_RECORD_FORMAT,
        id: JobId::new(),
        kind: "passthrough".into(),
        graph: one_node("passthrough", serde_json::json!({"kind": "iq"})),
        source: s(),
        // A band of its own: `b` below is no re-run of it.
        tuning: band(TONE_RATE_HZ - 1),
        state: JobState::Failed {
            reason: format!("source s: {UNPLUGGED}"),
        },
        context: serde_json::json!({}),
        created_ns: Timestamp(1),
        input: None,
        capture_rate_hz: None,
        elastic: false,
        operator_graph: false,
        owner: None,
        audio_policy: None,
        stream: None,
        sinks: Vec::new(),
        handler: None,
        handler_named: false,
        needs: Vec::new(),
    };
    let jobs = dir.path().join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join(format!("{}.json", old.id.0)),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    let (node, port, _srcs) = node_on(dir.path(), false, &[]).await;
    assert_eq!(state(&node, &old.id), failed_unplugged());

    let b = start_passthrough(&node, "s").await;
    fault(&node, &port, &[&b]).await;
    port.heal();
    until("b runs again", Duration::from_secs(5), || {
        state(&node, &b) == Some(JobState::Running)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state(&node, &old.id), failed_unplugged());
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_pruning_keeps_recoverable_records() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _srcs) = node_on(dir.path(), true, &["t"]).await;
    let a = start_passthrough(&node, "s").await;
    let b = start_passthrough(&node, "s").await;
    fault(&node, &port, &[&a, &b]).await;
    // More terminal records than the cap holds, each ending after the two recoverable
    // ones, each on a band of its own (none a re-run of another).
    for i in 0..zeta_core::node::JOB_HISTORY_MAX + 1 {
        let id = node
            .job_start(
                "passthrough",
                SourceId("t".into()),
                band(TONE_RATE_HZ - 1 - i as u64),
                one_node("passthrough", serde_json::json!({"kind": "iq"})),
                None,
                false,
            )
            .await
            .unwrap();
        node.job_stop(&id).await.unwrap();
    }
    let list = node.job_list();
    let terminal = list
        .iter()
        .filter(|r| matches!(r.state, JobState::Stopped | JobState::Failed { .. }))
        .count();
    assert_eq!(terminal, zeta_core::node::JOB_HISTORY_MAX, "they count");
    assert_eq!(state(&node, &a), failed_unplugged());
    assert_eq!(state(&node, &b), failed_unplugged());

    port.heal();
    until("both run again", Duration::from_secs(5), || {
        state(&node, &a) == Some(JobState::Running) && state(&node, &b) == Some(JobState::Running)
    })
    .await;
    stop_all(&node).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attached_job_comes_back_with_its_input() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _srcs) = node_on(dir.path(), true, &[]).await;
    let input = start(&node, "s", "audio_src", serde_json::json!({})).await;
    let op = Op::JobAttach {
        kind: JobKind::Transcribe,
        job: input.clone(),
        language: None,
        stream: None,
    };
    let attached = match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_attach: {r:?}"),
    };
    fault(&node, &port, &[&input]).await;
    until("the attached job ends", Duration::from_secs(5), || {
        state(&node, &attached) != Some(JobState::Running)
    })
    .await;
    assert_eq!(
        state(&node, &attached),
        Some(JobState::Failed {
            reason: format!("input job {} not running", input.0)
        })
    );

    port.heal();
    until("both run again", Duration::from_secs(5), || {
        state(&node, &input) == Some(JobState::Running)
            && state(&node, &attached) == Some(JobState::Running)
    })
    .await;
    // The attached job reads the relaunched input: its transcripts flow again.
    let stream = node
        .job_outputs(&attached)
        .into_iter()
        .rfind(|(k, _)| *k == StreamKind::Event)
        .map(|(_, s)| s)
        .unwrap();
    let mut rx = node.bus().subscribe(&stream).unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
    assert!(matches!(frame, Ok(Ok(_))), "{frame:?}");
    stop_all(&node).await;
}

/// A running job with no IQ forwarder never polls the fault, so it (and a job attached to
/// it) is not waited for: the job that does end is relaunched at once, not after the
/// 10 s bound, and the two are left as they were.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_fault_does_not_wait_for_a_job_with_no_forwarder() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _srcs) = node_on(dir.path(), true, &[]).await;
    let idle = start(&node, "s", "idle_src", serde_json::json!({})).await;
    let op = Op::JobAttach {
        kind: JobKind::Transcribe,
        job: idle.clone(),
        language: None,
        stream: None,
    };
    let attached = match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_attach: {r:?}"),
    };
    let fed = start_passthrough(&node, "s").await;
    let (fed_before, idle_before) = (node.job_outputs(&fed), node.job_outputs(&idle));

    port.set_fault(Some(UNPLUGGED.into()));
    port.heal_held();
    until(
        "the fed job is relaunched well inside the 10 s bound",
        Duration::from_secs(5),
        || state(&node, &fed) == Some(JobState::Running) && node.job_outputs(&fed) != fed_before,
    )
    .await;
    assert_eq!(port.fault(), None);
    assert_eq!(state(&node, &idle), Some(JobState::Running));
    assert_eq!(state(&node, &attached), Some(JobState::Running));
    assert_eq!(node.job_outputs(&idle), idle_before);
    stop_all(&node).await;
}

/// `source_list` reading the fault opens the episode on its own: here no job runs, so
/// nothing else can have opened it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_list_shows_the_episode_fault_then_clears() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _srcs) = node_on(dir.path(), true, &[]).await;
    assert_eq!(source(&node).await.fault, None);
    assert_eq!(node.debug_fault_episode(&s()), None);
    port.set_fault(Some(UNPLUGGED.into()));
    // Nothing has looked at the port yet.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(node.debug_fault_episode(&s()), None);
    assert_eq!(source(&node).await.fault.as_deref(), Some(UNPLUGGED));
    assert!(node.debug_fault_episode(&s()).is_some());
    port.heal();
    until("the episode closes", Duration::from_secs(5), || {
        node.debug_fault_episode(&s()).is_none()
    })
    .await;
    assert_eq!(source(&node).await.fault, None);
}

/// A port that is a [`FaultablePort`] but cannot heal (no `healed`, no worker), as a
/// KiwiSDR, another node or an in-process device is.
struct NoHeal(Arc<FaultablePort>);

#[async_trait]
impl Port for NoHeal {
    fn id(&self) -> &SourceId {
        self.0.id()
    }
    fn capabilities(&self) -> &zeta_core::types::Capabilities {
        self.0.capabilities()
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, zeta_core::port::PortError> {
        self.0.tune(t).await
    }
    fn current(&self) -> Option<Tuning> {
        self.0.current()
    }
    fn subscribe(&self) -> zeta_core::port::IqReceiver {
        self.0.subscribe()
    }
    fn fault(&self) -> Option<String> {
        self.0.fault()
    }
    async fn idle(&self) {
        self.0.idle().await
    }
}

/// A port that cannot heal opens no episode: the records that fail with its fault are
/// pruned as any other, so its history stays at the cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_port_that_cannot_heal_opens_no_episode_and_its_records_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let src = tone(dir.path(), "n");
    let faultable = FaultablePort::new(src.clone());
    let port: Arc<dyn Port> = Arc::new(NoHeal(faultable.clone()));
    let node = Node::open(cfg(dir.path(), true), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    // More than the cap holds, all running at once (none a re-run of another).
    let mut jobs = Vec::new();
    for _ in 0..zeta_core::node::JOB_HISTORY_MAX + 5 {
        jobs.push(start_passthrough(&node, "n").await);
    }
    faultable.set_fault(Some(UNPLUGGED.into()));
    let n = SourceId("n".into());
    assert_eq!(source_of(&node, &n).await.fault.as_deref(), Some(UNPLUGGED));
    until("every job ends", Duration::from_secs(10), || {
        node.job_list().iter().all(|r| r.state != JobState::Running)
    })
    .await;
    assert_eq!(node.debug_fault_episode(&n), None);
    let list = node.job_list();
    assert_eq!(
        list.len(),
        zeta_core::node::JOB_HISTORY_MAX,
        "pruned to the cap"
    );
    let want = JobState::Failed {
        reason: format!("source n: {UNPLUGGED}"),
    };
    assert!(list.iter().all(|r| r.state == want), "{list:#?}");
}

// The watch: a 1.024 Msps recording carrying a 1200-baud POCSAG page every 2 s, 300 kHz
// above the capture's centre (as `tests/watch.rs`'s fixture).

const RATE_HZ: u32 = 1_024_000;
const POCSAG_HZ: u64 = 148_637_500;
const OFFSET_HZ: u64 = 300_000;
const CENTER_HZ: u64 = POCSAG_HZ - OFFSET_HZ;
const CYCLE: usize = 2 * RATE_HZ as usize;
const PERSIST_MS: u64 = 1_500;
/// A re-adopted decoder's signal must be seen again within this: a margin for a loaded
/// host, as `tests/watch.rs`'s re-adoption tests use.
const READOPT_GONE_MS: u64 = 6_000;

struct Pocsag {
    _dir: tempfile::TempDir,
    meta: PathBuf,
}

fn pocsag() -> &'static Pocsag {
    static F: OnceLock<Pocsag> = OnceLock::new();
    F.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let mut iq = zeta_dsp::synth::pocsag_packet_iq(
            1_234_567,
            3,
            "HELLO ZETA",
            1200,
            RATE_HZ,
            OFFSET_HZ as f64,
        );
        assert!(iq.len() < CYCLE);
        iq.resize(CYCLE, Complex::new(0.0, 0.0));
        zeta_dsp::synth::awgn(&mut iq, 20.0, 1);
        let meta = CaptureMeta {
            sample_rate_hz: Hz(RATE_HZ as u64),
            center_hz: Hz(CENTER_HZ),
            start: Timestamp(0),
            annotations: Vec::new(),
        };
        let meta = write_cf32(dir.path(), "pocsag", &meta, &iq).unwrap();
        Pocsag { _dir: dir, meta }
    })
}

fn rx() -> SourceId {
    SourceId("rx".into())
}

fn capture() -> Tuning {
    Tuning {
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(RATE_HZ as u64),
    }
}

/// Every event `job`'s event stream carries from now on, across its relaunches.
struct Events {
    seen: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Events {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Events {
    fn of(node: &Arc<Node>, job: &JobId) -> Events {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (node, job, out) = (Arc::downgrade(node), job.clone(), seen.clone());
        let task = tokio::spawn(async move {
            loop {
                let rx = node.upgrade().and_then(|n| {
                    let s = n
                        .job_outputs(&job)
                        .into_iter()
                        .find(|(k, _)| *k == StreamKind::Event)?
                        .1;
                    n.bus().subscribe(&s)
                });
                let Some(mut rx) = rx else {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                };
                loop {
                    match rx.recv().await {
                        Ok(f) => {
                            if let Payload::Event(v) = f.payload {
                                out.lock().unwrap().push(v);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        });
        Events { seen, task }
    }

    fn watch(&self, state: &str) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["protocol"] == "watch" && e["state"] == state)
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

fn decoding(node: &Node, watch: &JobId) -> Vec<Value> {
    node.job_context(watch)
        .and_then(|c| c["watch"]["decoding"].as_array().cloned())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watch_and_its_decoder_come_back_and_nothing_is_lost() {
    zeta_core::node::watch::test_timing(0, 0);
    let dir = tempfile::tempdir().unwrap();
    let opts = FileOpts {
        realtime: true,
        block_samples: 16_384,
        repeat: true,
        tune_range: Some((Hz(CENTER_HZ - 2_000_000), Hz(CENTER_HZ + 2_000_000))),
    };
    let src = Arc::new(FileSource::open(rx(), &pocsag().meta, opts).unwrap());
    let port = FaultablePort::new(src.clone());
    let node = Node::open(cfg(dir.path(), true), registry(), vec![port.clone()])
        .await
        .unwrap();
    for c in zeta_dsp::classifiers(node.modes_handle()) {
        node.add_classifier(c);
    }
    src.start();
    let node = Arc::new(node);
    let mut graph = JobKind::Watch
        .expand(
            &rx(),
            capture(),
            Hz(RATE_HZ as u64),
            None,
            None,
            None,
            &node.modes(),
        )
        .unwrap();
    let detect = graph.nodes.iter_mut().find(|n| n.kind == "detect").unwrap();
    detect.params = serde_json::json!({"persist_ms": PERSIST_MS, "gone_ms": READOPT_GONE_MS});
    let watch = node
        .job_start(
            "watch",
            rx(),
            capture(),
            graph,
            Some(Hz(RATE_HZ as u64)),
            false,
        )
        .await
        .unwrap();
    let events = Events::of(&node, &watch);
    until("a decoding event", Duration::from_secs(40), || {
        !events.watch("decoding").is_empty()
    })
    .await;
    let job = JobId(events.watch("decoding")[0]["job"].as_str().unwrap().into());
    until("watch.decoding lists it", Duration::from_secs(5), || {
        decoding(&node, &watch)
            .iter()
            .any(|d| d["job"] == job.0.as_str())
    })
    .await;

    port.set_fault(Some(UNPLUGGED.into()));
    let failed = Some(JobState::Failed {
        reason: format!("source rx: {UNPLUGGED}"),
    });
    for id in [&watch, &job] {
        until("the job fails", Duration::from_secs(10), || {
            state(&node, id) != Some(JobState::Running)
        })
        .await;
        assert_eq!(state(&node, id), failed, "job {}", id.0);
    }
    let lost_before = events.watch("lost").len();
    let identifying_before = events.watch("identifying").len();

    port.heal();
    until("both run again", Duration::from_secs(10), || {
        state(&node, &watch) == Some(JobState::Running)
            && state(&node, &job) == Some(JobState::Running)
    })
    .await;
    // Re-adopted, and kept once its signal is seen again (past `gone_ms`).
    tokio::time::sleep(Duration::from_millis(READOPT_GONE_MS + 3_000)).await;
    let listed = decoding(&node, &watch);
    assert_eq!(listed.len(), 1, "{listed:#?}");
    assert_eq!(listed[0]["job"], job.0.as_str());
    assert_eq!(state(&node, &job), Some(JobState::Running));
    assert_eq!(state(&node, &watch), Some(JobState::Running));
    assert_eq!(
        events.watch("lost").len(),
        lost_before,
        "{:#?}",
        events.all()
    );
    // No second identify: the re-adopted decoder's signal is its own.
    assert_eq!(
        events.watch("identifying").len(),
        identifying_before,
        "{:#?}",
        events.all()
    );
    stop_all(&node).await;
}

/// A mix has no port: a fault leaves it running, silent, and the relaunched job joins it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fault_leaves_a_mix_running_and_it_picks_the_relaunched_job_up() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _srcs) = node_on(dir.path(), true, &[]).await;
    let demod = start(&node, "s", "audio_src", serde_json::json!({})).await;
    let mix = match node.apply(Op::MixStart { sources: None }).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("mix_start: {r:?}"),
    };
    let inputs = |n: u64| node.job_context(&mix).unwrap()["mix"]["inputs"] == n;
    until("the mix lists the job", Duration::from_secs(2), || {
        inputs(1)
    })
    .await;

    fault(&node, &port, &[&demod]).await;
    until("the mix drops the job", Duration::from_secs(2), || {
        inputs(0)
    })
    .await;
    assert_eq!(state(&node, &mix), Some(JobState::Running));

    port.heal();
    until("the job comes back", Duration::from_secs(5), || {
        state(&node, &demod) == Some(JobState::Running)
    })
    .await;
    until("the mix picks it up", Duration::from_secs(2), || inputs(1)).await;
    assert_eq!(state(&node, &mix), Some(JobState::Running));
    assert_eq!(
        node.job_list().iter().filter(|r| r.kind == "mix").count(),
        1
    );
    stop_all(&node).await;
}

/// A self-sweep scan reads no fault (it is a blind run), so a held fault's heal does
/// not wait for it: the rigid job that failed of the fault is relaunched at once,
/// not after the 10 s bound, and the scan runs throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_heal_does_not_wait_for_a_self_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let src = tone(dir.path(), "s");
    let port = FaultablePort::sweeping(
        src.clone(),
        zeta_core::types::SweepCaps {
            min_hz: Hz(TONE_HZ - 1_000_000),
            max_hz: Hz(TONE_HZ + 1_000_000),
            bin_hz_min: Hz(100),
            rows_per_s_max: 200,
            calibrated: false,
        },
    );
    let node = Node::open(cfg(dir.path(), true), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let fed = start_passthrough(&node, "s").await;
    let op = Op::JobStart {
        kind: JobKind::Scan,
        source: s(),
        center_hz: Hz(TONE_HZ),
        bandwidth_hz: Hz(1_000_000),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy: None,
        dwell_scale: None,
        elastic: false,
        replace: vec![],
        handler: None,
        needs: vec![],
    };
    let scan = match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start scan: {r:?}"),
    };
    assert_eq!(node.lease_sweepers(&s()), vec![scan.clone()]);
    fault(&node, &port, &[&fed]).await;
    assert_eq!(state(&node, &scan), Some(JobState::Running));
    let fed_before = node.job_outputs(&fed);
    // The heal finds the port still faulted: every job that polls the fault is waited for.
    port.heal_held();
    until(
        "the rigid job is relaunched well inside the 10 s bound",
        Duration::from_secs(5),
        || {
            assert_eq!(state(&node, &scan), Some(JobState::Running));
            state(&node, &fed) == Some(JobState::Running) && node.job_outputs(&fed) != fed_before
        },
    )
    .await;
    assert_eq!(state(&node, &scan), Some(JobState::Running));
    assert_eq!(node.lease_sweepers(&s()), vec![scan]);
    stop_all(&node).await;
}

/// A [`FaultablePort`] that heals during `open`: at the first gain `open` restores
/// (before it resumes the jobs) it bumps its healed counter and yields, so a heal watcher
/// already running would recover the source then. It counts the node's
/// [`Port::heal_settled`] calls, which a recovery makes.
struct HealsDuringOpen {
    inner: Arc<FaultablePort>,
    healed_once: std::sync::atomic::AtomicBool,
    settled: std::sync::atomic::AtomicU32,
}

#[async_trait]
impl Port for HealsDuringOpen {
    fn id(&self) -> &SourceId {
        self.inner.id()
    }
    fn capabilities(&self) -> &zeta_core::types::Capabilities {
        self.inner.capabilities()
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, zeta_core::port::PortError> {
        self.inner.tune(t).await
    }
    fn current(&self) -> Option<Tuning> {
        self.inner.current()
    }
    fn subscribe(&self) -> zeta_core::port::IqReceiver {
        self.inner.subscribe()
    }
    fn fault(&self) -> Option<String> {
        self.inner.fault()
    }
    async fn idle(&self) {
        self.inner.idle().await
    }
    fn gain(&self) -> Option<zeta_core::port::GainInfo> {
        self.inner.gain()
    }
    async fn set_gain(
        &self,
        db: Option<f64>,
    ) -> Result<zeta_core::port::GainInfo, zeta_core::port::PortError> {
        let r = self.inner.set_gain(db).await;
        if !self
            .healed_once
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.inner.heal();
            tokio::task::yield_now().await;
        }
        r
    }
    fn healed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.inner.healed()
    }
    fn heal_settled(&self) {
        self.settled
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.heal_settled();
    }
}

/// A heal reported while `open` runs is recovered once `open` has resumed the jobs, not
/// during it: the heal watcher starts after the resume. (One thread, so nothing runs
/// beside `open` but what its awaits let in.)
#[tokio::test]
async fn a_heal_during_open_is_recovered_after_the_resume() {
    let dir = tempfile::tempdir().unwrap();
    // A first node leaves a gain to restore and a job to resume.
    let (node, _port, srcs) = node_on(dir.path(), true, &[]).await;
    let op = Op::SourceSetGain {
        source: s(),
        gain_db: Some(20.0),
    };
    assert!(node.apply(op).await.is_ok());
    let job = start_passthrough(&node, "s").await;
    drop(node);
    drop(srcs);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let src = tone(dir.path(), "s");
    let probe = Arc::new(HealsDuringOpen {
        inner: FaultablePort::new(src.clone()),
        healed_once: false.into(),
        settled: 0.into(),
    });
    let port: Arc<dyn Port> = probe.clone();
    let node = Node::open(cfg(dir.path(), true), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    // Open resumed the job and has not recovered the source yet.
    assert_eq!(state(&node, &job), Some(JobState::Running));
    assert_eq!(
        probe.settled.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "recovered during open"
    );
    assert_eq!(probe.inner.gain_calls(), vec![Some(20.0)]);
    // The watcher, started after the resume, recovers it now: the gain again.
    until("the heal is recovered", Duration::from_secs(5), || {
        probe.settled.load(std::sync::atomic::Ordering::SeqCst) == 1
    })
    .await;
    until("the gain is set again", Duration::from_secs(5), || {
        probe.inner.gain_calls() == vec![Some(20.0), Some(20.0)]
    })
    .await;
    assert_eq!(state(&node, &job), Some(JobState::Running));
    stop_all(&node).await;
}

/// A port added to a running node (after `open`) is watched from its `add_port`: its
/// heal relaunches the job that failed of its fault.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_port_added_after_open_has_its_heals_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path(), true), registry(), vec![])
        .await
        .unwrap();
    let src = tone(dir.path(), "s");
    let port = FaultablePort::new(src.clone());
    node.add_port(port.clone()).unwrap();
    src.start();
    let job = start_passthrough(&node, "s").await;
    fault(&node, &port, &[&job]).await;
    port.heal();
    until("the job runs again", Duration::from_secs(5), || {
        state(&node, &job) == Some(JobState::Running)
    })
    .await;
    stop_all(&node).await;
}
