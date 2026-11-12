//! Atomic replace: `job_start` and `source_ensure` name the holders that may be
//! stopped to make room, and the node stops them under the source's gate right
//! before the request, so nothing can take the radio in between.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, SourceInfo};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::StreamKind;
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, StreamId};

/// A radio that tunes to exactly the band asked for. With `stall` set, the next
/// `current()` blocks its thread for 50 ms with `stalling` set: a launch reads
/// the port's band right after its lease, so a test can act in that window.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
    stall: AtomicBool,
    stalling: AtomicBool,
}

impl Radio {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            id: SourceId(id.into()),
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
            tx: broadcast::channel(1).0,
            stall: AtomicBool::new(false),
            stalling: AtomicBool::new(false),
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
        // A real tune takes a while: long enough for a competing request to try.
        tokio::time::sleep(Duration::from_millis(2)).await;
        *self.cur.lock().unwrap() = Some(t);
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        if self.stall.swap(false, Ordering::SeqCst) {
            self.stalling.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            self.stalling.store(false, Ordering::SeqCst);
        }
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

/// Stands in for the dsp `fft` stage a `spectrum` or `scan` job runs; once
/// cancelled it takes `.1` to end.
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

const RADIO: &str = "radio0";

fn start_on(source: &str, kind: JobKind, t: Tuning, elastic: bool, replace: &[&JobId]) -> Op {
    Op::JobStart {
        kind,
        source: SourceId(source.into()),
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
        replace: replace.iter().map(|j| (*j).clone()).collect(),
    }
}

fn start(kind: JobKind, t: Tuning, replace: &[&JobId]) -> Op {
    start_on(RADIO, kind, t, false, replace)
}

fn ensure(t: Tuning, replace: &[&JobId]) -> Op {
    Op::SourceEnsure {
        source: SourceId(RADIO.into()),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        replace: replace.iter().map(|j| (*j).clone()).collect(),
        widen: false,
    }
}

async fn started(node: &Node, op: Op) -> JobId {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    }
}

async fn source(node: &Node, id: &str) -> SourceInfo {
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v.into_iter().find(|s| s.id.0 == id).expect("listed"),
        r => panic!("source_list: {r:?}"),
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

fn streams(node: &Node, job: &JobId) -> usize {
    node.bus()
        .list()
        .into_iter()
        .filter(|s| s.provenance.job.as_ref() == Some(job))
        .count()
}

async fn open(dir: &Path, radios: &[Arc<Radio>]) -> Node {
    let ports: Vec<Arc<dyn Port>> = radios.iter().map(|r| r.clone() as Arc<dyn Port>).collect();
    Node::open(cfg(dir), registry(), ports).await.unwrap()
}

/// Whether `want` lies inside `cur`.
fn fits(want: &Tuning, cur: &Tuning) -> bool {
    let half = |t: &Tuning| t.bandwidth_hz.0 / 2;
    want.center_hz.0 - half(want) >= cur.center_hz.0 - half(cur)
        && want.center_hz.0 + half(want) <= cur.center_hz.0 + half(cur)
}

/// Fails the test instead of hanging it when the node deadlocks.
async fn within<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("no deadlock")
}

#[tokio::test]
async fn replace_stops_the_named_holder_and_starts() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let a = started(
        &node,
        start(JobKind::Passthrough, band(100_000_000, 48_000), &[]),
    )
    .await;
    let b_band = band(433_000_000, 48_000);
    let b = within(started(&node, start(JobKind::Passthrough, b_band, &[&a]))).await;
    assert_eq!(record(&node, &a).state, JobState::Stopped);
    assert_eq!(streams(&node, &a), 0);
    assert_eq!(record(&node, &b).state, JobState::Running);
    let s = source(&node, RADIO).await;
    assert_eq!(s.holders, vec![b]);
    assert_eq!(s.current, Some(b_band));
}

#[tokio::test]
async fn source_ensure_replace_stops_the_named_holder_and_tunes() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let a = started(
        &node,
        start(JobKind::Passthrough, band(100_000_000, 48_000), &[]),
    )
    .await;
    let want = band(433_000_000, 48_000);
    let got = within(node.apply(ensure(want, &[&a]))).await.unwrap();
    assert_eq!(got, OpResult::Tuning(want));
    assert_eq!(record(&node, &a).state, JobState::Stopped);
    let s = source(&node, RADIO).await;
    assert!(s.holders.is_empty(), "{s:?}");
    assert_eq!(s.current, Some(want));
}

#[tokio::test]
async fn replace_refuses_an_elastic_or_foreign_holder() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO), Radio::new("radio1")]).await;
    let elastic = started(
        &node,
        start_on(
            RADIO,
            JobKind::Spectrum,
            band(100_000_000, 9_600),
            true,
            &[],
        ),
    )
    .await;
    let rigid = started(
        &node,
        start(JobKind::Passthrough, band(100_000_000, 9_600), &[]),
    )
    .await;
    let foreign = started(
        &node,
        start_on(
            "radio1",
            JobKind::Passthrough,
            band(100_000_000, 9_600),
            false,
            &[],
        ),
    )
    .await;
    let far = band(433_000_000, 48_000);
    for id in [&elastic, &foreign] {
        let want = format!("job {} does not hold source {RADIO}", id.0);
        for op in [
            start(JobKind::Passthrough, far, &[&rigid, id]),
            ensure(far, &[&rigid, id]),
        ] {
            match within(node.apply(op)).await {
                Err(OpError::Other { message }) => assert_eq!(message, want),
                other => panic!("expected other, got {other:?}"),
            }
        }
    }
    let unknown = JobId::new();
    for op in [
        start(JobKind::Passthrough, far, &[&rigid, &unknown]),
        ensure(far, &[&rigid, &unknown]),
    ] {
        match within(node.apply(op)).await {
            Err(OpError::Unknown { field, value, .. }) => {
                assert_eq!(field, "job");
                assert_eq!(value, unknown.0);
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }
    // Nothing was stopped, the elastic job included.
    for id in [&elastic, &rigid, &foreign] {
        assert_eq!(record(&node, id).state, JobState::Running, "{id:?}");
    }
    let s = source(&node, RADIO).await;
    assert_eq!(s.holders, vec![rigid]);
    assert_eq!(s.elastic, vec![elastic]);
    assert_eq!(node.job_list().len(), 3);
}

#[tokio::test]
async fn replace_leaves_named_jobs_stopped_when_another_holder_remains() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let capture = band(100_000_000, 2_000_000);
    let a = started(&node, start(JobKind::Passthrough, capture, &[])).await;
    let c = started(
        &node,
        start(JobKind::Passthrough, band(100_500_000, 100_000), &[]),
    )
    .await;
    match within(node.apply(start(
        JobKind::Passthrough,
        band(433_000_000, 48_000),
        &[&a],
    )))
    .await
    {
        Err(OpError::Conflict {
            holders,
            current,
            options,
            ..
        }) => {
            assert_eq!(holders, vec![c.clone()]);
            assert_eq!(current, capture);
            assert_eq!(
                options.last().unwrap(),
                &format!("retry with replace: [{}]", c.0)
            );
        }
        other => panic!("expected conflict, got {other:?}"),
    }
    assert_eq!(record(&node, &a).state, JobState::Stopped);
    assert_eq!(record(&node, &c).state, JobState::Running);
    let s = source(&node, RADIO).await;
    assert_eq!(s.holders, vec![c]);
    assert_eq!(s.current, Some(capture));
}

#[tokio::test]
async fn conflict_options_name_the_replace_retry() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let capture = band(100_000_000, 2_000_000);
    let a = started(&node, start(JobKind::Passthrough, capture, &[])).await;
    let c = started(
        &node,
        start(JobKind::Passthrough, band(100_500_000, 100_000), &[]),
    )
    .await;
    let far = band(433_000_000, 48_000);
    for op in [start(JobKind::Passthrough, far, &[]), ensure(far, &[])] {
        match node.apply(op).await {
            Err(OpError::Conflict { options, .. }) => assert_eq!(
                options,
                vec![
                    "stop one of the holders".to_string(),
                    "choose a band inside current".to_string(),
                    format!("retry with replace: [{}, {}]", a.0, c.0),
                ]
            ),
            other => panic!("expected conflict, got {other:?}"),
        }
    }
    node.job_stop(&a).await.unwrap();
    node.job_stop(&c).await.unwrap();
    // A stepped scan time-shares the source but gives way: no conflict, nothing to replace.
    let scan = started(
        &node,
        start(JobKind::Scan, band(100_000_000, 40_000_000), &[]),
    )
    .await;
    let b = started(&node, start(JobKind::Passthrough, far, &[])).await;
    assert_eq!(record(&node, &scan).state, JobState::Running);
    assert_eq!(source(&node, RADIO).await.holders, vec![b.clone()]);
    node.job_stop(&b).await.unwrap();
    node.job_stop(&scan).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replace_stops_a_scan_holder() {
    for round in 0..10 {
        let dir = tempfile::tempdir().unwrap();
        // No dwell: on another worker the scan is always retuning, so it waits on
        // the gate the replace holds while the replace stops it.
        let cfg = NodeConfig {
            source_positions: Default::default(),
            sets_dir: None,
            burst_policy: Default::default(),
            scan_dwell_ms: 0,
            ..cfg(dir.path())
        };
        let ports: Vec<Arc<dyn Port>> = vec![Radio::new(RADIO)];
        let node = Node::open(cfg, registry(), ports).await.unwrap();
        let scan = started(
            &node,
            start(JobKind::Scan, band(100_000_000, 40_000_000), &[]),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        let want = band(433_000_000, 48_000);
        let b = within(started(&node, start(JobKind::Passthrough, want, &[&scan]))).await;
        assert_eq!(
            record(&node, &scan).state,
            JobState::Stopped,
            "round {round}"
        );
        let s = source(&node, RADIO).await;
        assert_eq!(s.holders, vec![b], "round {round}");
        assert_eq!(s.current, Some(want), "round {round}");
    }
}

#[tokio::test]
async fn replace_by_a_scan_stops_the_holder_and_scans() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let a = started(
        &node,
        start(JobKind::Passthrough, band(100_000_000, 48_000), &[]),
    )
    .await;
    let elastic = started(
        &node,
        start_on(
            RADIO,
            JobKind::Spectrum,
            band(100_000_000, 9_600),
            true,
            &[],
        ),
    )
    .await;
    let scan_band = band(100_000_000, 40_000_000);
    let scan = within(started(&node, start(JobKind::Scan, scan_band, &[&a]))).await;
    assert_eq!(record(&node, &a).state, JobState::Stopped);
    assert_eq!(record(&node, &elastic).state, JobState::Stopped);
    assert_eq!(record(&node, &scan).state, JobState::Running);
    let s = source(&node, RADIO).await;
    assert_eq!(s.holders, vec![scan.clone()]);
    assert!(s.elastic.is_empty(), "{s:?}");
    within(node.job_stop(&scan)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replace_and_a_competing_start_never_both_hold_incompatible_bands() {
    for round in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        // A's stage takes 50 ms to end once cancelled, so B's replace holds the
        // source's gate that long, and D (sent 10 ms after B) is queued on the gate
        // while A stops: were the gate let go between A's stop and B's tune and
        // lease, D would take the radio first.
        let reg = registry_lingering(Duration::from_millis(50));
        let ports: Vec<Arc<dyn Port>> = vec![Radio::new(RADIO)];
        let node = Arc::new(Node::open(cfg(dir.path()), reg, ports).await.unwrap());
        let a = started(
            &node,
            start(JobKind::Spectrum, band(100_000_000, 48_000), &[]),
        )
        .await;
        let (n1, n2) = (node.clone(), node.clone());
        let b_op = start(JobKind::Passthrough, band(433_000_000, 48_000), &[&a]);
        let d_op = start(JobKind::Passthrough, band(868_000_000, 48_000), &[]);
        let b = tokio::spawn(async move { n1.apply(b_op).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let d = tokio::spawn(async move { n2.apply(d_op).await });
        let (b, d) = within(async { (b.await.unwrap(), d.await.unwrap()) }).await;
        // The radio was never free between the stop and the replacing start.
        assert!(b.is_ok(), "round {round}: {b:?}");
        assert!(
            matches!(d, Err(OpError::Conflict { .. })),
            "round {round}: {d:?}"
        );
        assert_eq!(record(&node, &a).state, JobState::Stopped, "round {round}");
        let s = source(&node, RADIO).await;
        let cur = s.current.expect("tuned");
        let holders = node.lease_holders(&SourceId(RADIO.into()));
        assert_eq!(holders.len(), 1, "round {round}: {holders:?}");
        for (j, t) in &holders {
            assert!(
                fits(t, &cur),
                "round {round}: {j:?} at {t:?} outside {cur:?}"
            );
            assert_eq!(record(&node, j).state, JobState::Running, "round {round}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replace_racing_a_graph_put_restart_leaves_the_job_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = Arc::new(open(dir.path(), std::slice::from_ref(&radio)).await);
    let a = started(
        &node,
        start(JobKind::Passthrough, band(100_000_000, 48_000), &[]),
    )
    .await;
    let graph = node
        .job_list()
        .into_iter()
        .find(|r| r.id == a)
        .unwrap()
        .graph;
    // The restart stops A, leases again and then reads the port's band: stalled
    // there, its new lease is held but A is not yet recorded running.
    radio.stall.store(true, Ordering::SeqCst);
    let (n, id) = (node.clone(), a.clone());
    let put = tokio::spawn(async move { n.job_graph_put(&id, graph).await });
    within(async {
        // Spinning, not sleeping: the stall blocks a worker thread, timers included.
        while !radio.stalling.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        node.lease_holders(&SourceId(RADIO.into()))
            .iter()
            .any(|(j, _)| j == &a)
    );
    let want = band(433_000_000, 48_000);
    let b = within(started(&node, start(JobKind::Passthrough, want, &[&a]))).await;
    // The restart is refused: the replace stopped A first.
    match within(put).await.unwrap() {
        Err(zeta_core::node::NodeError::Stopped(id)) => assert_eq!(id, a),
        other => panic!("expected stopped, got {other:?}"),
    }
    assert_eq!(record(&node, &a).state, JobState::Stopped);
    assert_eq!(streams(&node, &a), 0);
    assert_eq!(record(&node, &b).state, JobState::Running);
    let s = source(&node, RADIO).await;
    assert_eq!(s.holders, vec![b]);
    assert_eq!(s.current, Some(want));
}

fn duplicate(r: Result<OpResult, OpError>) -> (JobId, String) {
    match r {
        Err(OpError::Duplicate { job, message }) => (job, message),
        other => panic!("expected a duplicate, got {other:?}"),
    }
}

#[tokio::test]
async fn an_identical_rigid_start_is_a_duplicate_naming_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let b = band(100_000_000, 48_000);
    let a = started(&node, start(JobKind::Passthrough, b, &[])).await;
    let (job, message) = duplicate(node.apply(start(JobKind::Passthrough, b, &[])).await);
    assert_eq!(job, a);
    assert_eq!(
        message,
        format!(
            "an identical passthrough already runs on radio0 at 100000000/48000 Hz as job {}",
            a.0
        )
    );
    let running = node
        .job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running)
        .count();
    assert_eq!(running, 1);
}

#[tokio::test]
async fn a_different_deviation_is_not_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = registry();
    zeta_dsp::register_all(&mut reg);
    let ports: Vec<Arc<dyn Port>> = vec![Radio::new(RADIO)];
    let node = Node::open(cfg(dir.path()), reg, ports).await.unwrap();
    let b = band(100_000_000, 200_000);
    let fm = |dev: Option<u64>| Op::JobStart {
        kind: JobKind::FmDemod,
        source: SourceId(RADIO.into()),
        center_hz: b.center_hz,
        bandwidth_hz: b.bandwidth_hz,
        deviation_hz: dev,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        elastic: false,
        replace: vec![],
    };
    let a = started(&node, fm(None)).await;
    let second = started(&node, fm(Some(2500))).await;
    assert_ne!(a, second);
    let (job, _) = duplicate(node.apply(fm(Some(5000))).await);
    assert_eq!(job, a);
}

#[tokio::test]
async fn an_elastic_start_is_never_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let b = band(100_000_000, 48_000);
    let a = started(&node, start_on(RADIO, JobKind::Spectrum, b, true, &[])).await;
    let c = started(&node, start_on(RADIO, JobKind::Spectrum, b, true, &[])).await;
    assert_ne!(a, c);
}

#[tokio::test]
async fn a_stopped_identical_job_is_not_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let b = band(100_000_000, 48_000);
    let a = started(&node, start(JobKind::Passthrough, b, &[])).await;
    node.apply(Op::JobStop { job: a.clone() }).await.unwrap();
    let c = started(&node, start(JobKind::Passthrough, b, &[])).await;
    assert_ne!(a, c);
}

#[tokio::test]
async fn a_duplicate_is_refused_before_replace_stops_anything() {
    // Spec §4.3: the duplicate check runs before the replacements, under the same
    // gate, so a refused start stops nothing; a duplicate that `replace` names is
    // about to stop and is no duplicate.
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &[Radio::new(RADIO)]).await;
    let x = started(
        &node,
        start(JobKind::Passthrough, band(100_000_000, 1_000_000), &[]),
    )
    .await;
    let b = band(100_200_000, 48_000);
    let y = started(&node, start(JobKind::Passthrough, b, &[])).await;
    let (job, _) = duplicate(node.apply(start(JobKind::Passthrough, b, &[&x])).await);
    assert_eq!(job, y);
    assert_eq!(record(&node, &x).state, JobState::Running);
    assert_eq!(record(&node, &y).state, JobState::Running);
    let z = started(&node, start(JobKind::Passthrough, b, &[&y])).await;
    assert_ne!(z, y);
    assert_eq!(record(&node, &y).state, JobState::Stopped);
    assert_eq!(record(&node, &x).state, JobState::Running);
    assert_eq!(record(&node, &z).state, JobState::Running);
}
