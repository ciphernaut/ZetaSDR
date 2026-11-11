//! Job runs: a supervisor ends only the run it was spawned for, so overlapping
//! launches of one job id never leave a run behind (its stages running, its
//! streams registered) after the job stops; `debug_stage_count` shows any.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult, SourceInfo};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::StreamKind;
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, StreamId};

/// A radio that tunes to exactly the band asked for and records every tune; with
/// `fail` set every tune is refused, with `hold` set every tune first waits for a
/// permit from `permits` (so a test decides when the source's gate is let go).
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tunes: Mutex<Vec<Tuning>>,
    fail: AtomicBool,
    hold: AtomicBool,
    permits: tokio::sync::Semaphore,
    tx: broadcast::Sender<IqBlock>,
    /// Called on every `idle` (under the source's gate), when set.
    on_idle: OnIdle,
}

type OnIdle = Mutex<Option<Box<dyn Fn() + Send + Sync>>>;

impl Radio {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            id: SourceId("radio0".into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(20_000_000)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: false,
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
            },
            cur: Mutex::new(None),
            tunes: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            permits: tokio::sync::Semaphore::new(0),
            tx: broadcast::channel(1).0,
            on_idle: Mutex::new(None),
        })
    }
}

#[async_trait]
impl Port for Radio {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(PortError::Io("tune refused".into()));
        }
        if self.hold.load(Ordering::SeqCst) {
            self.permits.acquire().await.expect("open").forget();
        }
        self.tunes.lock().unwrap().push(t);
        *self.cur.lock().unwrap() = Some(t);
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
    async fn idle(&self) {
        if let Some(f) = &*self.on_idle.lock().unwrap() {
            f();
        }
    }
}

/// Stands in for the dsp `fft` stage a `spectrum` job runs (core cannot depend on it):
/// one event per IQ frame, ending when its input closes or, `.1` after, it is cancelled.
struct FakeFft(StageSpec, Duration);

#[async_trait]
impl Stage for FakeFft {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: serde_json::Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        loop {
            let frame = tokio::select! {
                _ = cancel.cancelled() => {
                    tokio::time::sleep(self.1).await;
                    return Err(StageError::Cancelled);
                }
                r = inputs[0].recv() => match r {
                    Ok(f) => f,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            };
            if let Payload::Iq(b) = &frame.payload {
                let ev = serde_json::json!({"center_hz": b.center_hz.0});
                bus.publish(Frame::new(
                    outputs[0].clone(),
                    frame.provenance.clone(),
                    Payload::Event(ev),
                ));
            }
        }
    }
}

fn registry() -> StageRegistry {
    registry_lingering(Duration::ZERO)
}

/// [`registry`] whose `fft` stage takes `linger` to end once cancelled.
fn registry_lingering(linger: Duration) -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(FakeFft(
        StageSpec {
            kind: "fft",
            inputs: vec![StreamKind::Iq],
            outputs: vec![StreamKind::Event],
            params_schema: schemars::json_schema!({}),
        },
        linger,
    )));
    r
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
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
    }
}

fn band(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

fn start(kind: JobKind, t: Tuning, elastic: bool) -> Op {
    Op::JobStart {
        kind,
        source: SourceId("radio0".into()),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        elastic,
        replace: vec![],
    }
}

async fn started(node: &Node, op: Op) -> JobId {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    }
}

async fn source(node: &Node) -> SourceInfo {
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v.into_iter().next().expect("one source"),
        r => panic!("source_list: {r:?}"),
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

/// The ids of the streams `job` has registered, sorted.
fn streams(node: &Node, job: &JobId) -> Vec<StreamId> {
    let mut v: Vec<StreamId> = node
        .bus()
        .list()
        .into_iter()
        .filter(|s| s.provenance.job.as_ref() == Some(job))
        .map(|s| s.id)
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

async fn open(dir: &Path, radio: &Arc<Radio>) -> Node {
    Node::open(cfg(dir), registry(), vec![radio.clone()])
        .await
        .unwrap()
}

/// Rounds of two concurrent `graph_put`s in [`concurrent_graph_puts_leave_one_run`].
const ROUNDS: usize = 100;

/// Poll `f` every 10 ms for up to `ms`; whether it held.
async fn wait_until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while !f() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

/// Two `graph_put`s of one running job at once, over and over: the stages linger
/// after cancel, so while one put's stop waits on the old run the other's stop
/// finds the job already `Stopped`, returns at once and launches. Once the job
/// stops, no run is left: no stage task, no stream, no lease. Throughout, an old run's
/// supervisor never takes the radio from the new one: the port is never idled while
/// the job is recorded `Running`, and each round ends with the job holding its lease.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_graph_puts_leave_one_run() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = Arc::new(
        Node::open(
            cfg(dir.path()),
            registry_lingering(Duration::from_millis(20)),
            vec![radio.clone()],
        )
        .await
        .unwrap(),
    );
    let id = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 1_000_000), false),
    )
    .await;
    let g = match node.apply(Op::GraphGet { job: id.clone() }).await.unwrap() {
        OpResult::Graph(g) => g,
        r => panic!("graph_get: {r:?}"),
    };
    // Idled under the gate, so no lease can be granted meanwhile: a `Running` record
    // then is a run whose lease another run's supervisor released.
    let idled_under_a_run = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let (node, id, n) = (Arc::downgrade(&node), id.clone(), idled_under_a_run.clone());
        *radio.on_idle.lock().unwrap() = Some(Box::new(move || {
            if let Some(node) = node.upgrade()
                && record(&node, &id).state == JobState::Running
            {
                n.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }
    let rt = tokio::runtime::Handle::current();
    for round in 0..ROUNDS {
        // Two OS threads released together by a spin barrier, so both puts read the
        // job `Running` before either stops it as often as the scheduler allows.
        let ready = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let put = || {
            let (node, id, g, ready, rt) = (
                node.clone(),
                id.clone(),
                g.clone(),
                ready.clone(),
                rt.clone(),
            );
            std::thread::spawn(move || {
                ready.fetch_add(1, Ordering::SeqCst);
                while ready.load(Ordering::SeqCst) < 2 {
                    std::hint::spin_loop();
                }
                rt.block_on(node.apply(Op::GraphPut { job: id, graph: g }))
            })
        };
        let (a, b) = (put(), put());
        let (a, b) = tokio::task::spawn_blocking(move || (a.join().unwrap(), b.join().unwrap()))
            .await
            .unwrap();
        // One wins; the other may be refused while the winner's run is live.
        assert!(a.is_ok() || b.is_ok(), "round {round}: {a:?} {b:?}");
        assert_eq!(record(&node, &id).state, JobState::Running, "round {round}");
        let holders: Vec<JobId> = node
            .lease_holders(&radio.id)
            .into_iter()
            .map(|(j, _)| j)
            .collect();
        assert_eq!(holders, vec![id.clone()], "round {round}");
    }
    assert_eq!(
        idled_under_a_run.load(Ordering::SeqCst),
        0,
        "the port was idled while the job was recorded running"
    );
    radio.on_idle.lock().unwrap().take();
    node.apply(Op::JobStop { job: id.clone() }).await.unwrap();
    assert!(
        wait_until(1_000, || node.debug_stage_count() == 0).await,
        "{} stage tasks left",
        node.debug_stage_count()
    );
    assert!(streams(&node, &id).is_empty(), "{:?}", streams(&node, &id));
    assert!(node.lease_holders(&source(&node).await.id).is_empty());
}

/// As `elastic.rs::stop_during_a_relaunch_is_not_undone`, and no stage task of the
/// stopped job outlives it.
#[tokio::test]
async fn stop_during_a_relaunch_leaves_no_stage() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = Arc::new(open(dir.path(), &radio).await);
    let elastic = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 9_600), true),
    )
    .await;
    assert!(node.debug_stage_count() > 0);
    radio.hold.store(true, Ordering::SeqCst);
    let ensure = |c: u64| {
        let node = node.clone();
        tokio::spawn(async move {
            node.apply(Op::SourceEnsure {
                source: SourceId("radio0".into()),
                center_hz: Hz(c),
                bandwidth_hz: Hz(9_600),
                replace: vec![],
            })
            .await
        })
    };
    // The first tune holds the gate; the second queues behind it.
    let first = ensure(200_000_000);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = ensure(300_000_000);
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The first tune ends: the second takes the gate, and the first's relaunch of the
    // elastic job queues on it.
    radio.permits.add_permits(1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    node.job_stop(&elastic).await.unwrap();
    radio.permits.add_permits(1);
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    assert_eq!(record(&node, &elastic).state, JobState::Stopped);
    assert!(streams(&node, &elastic).is_empty());
    assert!(node.lease_elastic(&SourceId("radio0".into())).is_empty());
    assert!(wait_until(1_000, || node.debug_stage_count() == 0).await);
}

/// A second stop of a job whose first stop is still waiting on its lingering stages
/// waits too: when either returns, the run's lease is released and its stages are
/// gone, so a launch that follows (a `graph_put`'s restart) never meets the old run's
/// supervisor releasing the new lease.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_stop_returns_after_the_run_released() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = Arc::new(
        Node::open(
            cfg(dir.path()),
            registry_lingering(Duration::from_millis(200)),
            vec![radio.clone()],
        )
        .await
        .unwrap(),
    );
    let id = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 1_000_000), false),
    )
    .await;
    let first = {
        let (node, id) = (node.clone(), id.clone());
        tokio::spawn(async move { node.job_stop(&id).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!first.is_finished(), "the first stop waits on the stages");
    node.job_stop(&id).await.unwrap();
    assert!(node.lease_holders(&radio.id).is_empty());
    assert_eq!(node.debug_stage_count(), 0);
    assert!(streams(&node, &id).is_empty());
    first.await.unwrap().unwrap();
}
