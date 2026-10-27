//! Elastic leases: a `spectrum` job started with `elastic` follows the source's capture
//! instead of holding it. Rigid requests ignore it, every retune relaunches it under the
//! same id, and a scan on its source stops it.

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
}

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
            },
            cur: Mutex::new(None),
            tunes: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            permits: tokio::sync::Semaphore::new(0),
            tx: broadcast::channel(1).0,
        })
    }

    fn tunes(&self) -> Vec<Tuning> {
        self.tunes.lock().unwrap().clone()
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

#[tokio::test]
async fn rigid_start_over_an_elastic_holder_tunes_and_relaunches_it() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let elastic = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 9_600), true),
    )
    .await;
    let before = streams(&node, &elastic);
    assert!(!before.is_empty());
    assert!(record(&node, &elastic).elastic);

    let rigid_band = band(100_020_000, 9_600);
    let rigid = started(&node, start(JobKind::Passthrough, rigid_band, false)).await;
    let s = source(&node).await;
    assert_eq!(s.current, Some(rigid_band));
    assert_eq!(s.holders, vec![rigid.clone()]);
    assert_eq!(s.elastic, vec![elastic.clone()]);

    let rec = record(&node, &elastic);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(Some(rec.tuning), s.current);
    let after = streams(&node, &elastic);
    assert!(!after.is_empty());
    assert!(
        after.iter().all(|s| !before.contains(s)),
        "{before:?} {after:?}"
    );
    assert!(!record(&node, &rigid).elastic);
}

/// A narrow rigid start inside the wide capture an elastic job follows is leased
/// on that capture: no tune, so the page's view does not collapse, and the
/// elastic job keeps its streams.
#[tokio::test]
async fn rigid_start_inside_an_elastic_capture_does_not_tune() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let wide = band(100_000_000, 2_000_000);
    let elastic = started(&node, start(JobKind::Spectrum, wide, true)).await;
    let before = streams(&node, &elastic);
    let tunes = radio.tunes();
    assert_eq!(tunes, vec![wide]);

    let rigid_band = band(100_500_000, 48_000);
    let rigid = started(&node, start(JobKind::Passthrough, rigid_band, false)).await;
    assert_eq!(radio.tunes(), tunes);
    let s = source(&node).await;
    assert_eq!(s.current, Some(wide));
    assert_eq!(s.holders, vec![rigid.clone()]);
    assert_eq!(s.elastic, vec![elastic.clone()]);
    let rec = record(&node, &elastic);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, wide);
    assert_eq!(streams(&node, &elastic), before);
    assert_eq!(record(&node, &rigid).state, JobState::Running);
}

#[tokio::test]
async fn source_ensure_retune_relaunches_the_elastic_job() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let elastic = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 9_600), true),
    )
    .await;
    let before = streams(&node, &elastic);

    let want = band(433_000_000, 48_000);
    let got = node
        .apply(Op::SourceEnsure {
            source: SourceId("radio0".into()),
            center_hz: want.center_hz,
            bandwidth_hz: want.bandwidth_hz,
            replace: vec![],
        })
        .await
        .unwrap();
    assert_eq!(got, OpResult::Tuning(want));

    let rec = record(&node, &elastic);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, want);
    let after = streams(&node, &elastic);
    assert!(!after.is_empty());
    assert!(
        after.iter().all(|s| !before.contains(s)),
        "{before:?} {after:?}"
    );
    let s = source(&node).await;
    assert!(s.holders.is_empty(), "{s:?}");
    assert_eq!(s.elastic, vec![elastic]);
}

#[tokio::test]
async fn elastic_start_on_a_held_port_joins_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let capture = band(100_000_000, 48_000);
    let rigid = started(&node, start(JobKind::Passthrough, capture, false)).await;
    let rigid_streams = streams(&node, &rigid);
    let tunes = radio.tunes().len();

    // Outside the capture: an elastic request never conflicts and never retunes.
    let elastic = started(
        &node,
        start(JobKind::Spectrum, band(433_000_000, 9_600), true),
    )
    .await;
    assert_eq!(radio.tunes().len(), tunes);
    assert_eq!(radio.current(), Some(capture));
    let rec = record(&node, &elastic);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, capture);
    assert_eq!(streams(&node, &rigid), rigid_streams);
    let s = source(&node).await;
    assert_eq!(s.holders, vec![rigid]);
    assert_eq!(s.elastic, vec![elastic]);
}

#[tokio::test]
async fn elastic_on_a_non_spectrum_kind_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    match node
        .apply(start(JobKind::Passthrough, band(100_000_000, 9_600), true))
        .await
    {
        Err(OpError::Other { message }) => assert_eq!(message, "passthrough cannot be elastic"),
        other => panic!("expected other, got {other:?}"),
    }
    assert!(node.job_list().is_empty());
    assert!(radio.tunes().is_empty());
}

#[tokio::test]
async fn scan_stops_elastic_jobs_on_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let elastic = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 9_600), true),
    )
    .await;
    let scan = started(
        &node,
        start(JobKind::Scan, band(100_000_000, 40_000_000), false),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while record(&node, &elastic).state != JobState::Stopped {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{:?}",
            record(&node, &elastic).state
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(streams(&node, &elastic).is_empty());
    assert_eq!(record(&node, &scan).state, JobState::Running);
    let s = source(&node).await;
    assert_eq!(s.holders, vec![scan.clone()]);
    assert!(s.elastic.is_empty());
    node.job_stop(&scan).await.unwrap();
}

#[tokio::test]
async fn failed_retune_leaves_the_elastic_job_running() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let old = band(100_000_000, 9_600);
    let elastic = started(&node, start(JobKind::Spectrum, old, true)).await;
    let before = streams(&node, &elastic);

    radio.fail.store(true, Ordering::SeqCst);
    let e = node
        .apply(start(JobKind::Passthrough, band(433_000_000, 9_600), false))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("tune refused"), "{e}");

    let rec = record(&node, &elastic);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, old);
    assert_eq!(radio.current(), Some(old));
    assert_eq!(streams(&node, &elastic), before);
    let s = source(&node).await;
    assert!(s.holders.is_empty(), "{s:?}");
    assert_eq!(s.elastic, vec![elastic]);
}

#[tokio::test]
async fn elastic_records_resume_after_rigid_ones() {
    let dir = tempfile::tempdir().unwrap();
    let rigid_band = band(100_020_000, 9_600);
    let (elastic, rigid) = {
        let radio = Radio::new();
        let node = open(dir.path(), &radio).await;
        // The elastic job is older and, after the relaunch, as wide as the rigid one,
        // so only the elastic key puts it last.
        let elastic = started(
            &node,
            start(JobKind::Spectrum, band(100_000_000, 9_600), true),
        )
        .await;
        let rigid = started(&node, start(JobKind::Passthrough, rigid_band, false)).await;
        assert_eq!(record(&node, &elastic).tuning, rigid_band);
        (elastic, rigid)
    };
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    assert_eq!(record(&node, &rigid).state, JobState::Running);
    let rec = record(&node, &elastic);
    assert_eq!(rec.state, JobState::Running);
    assert!(rec.elastic);
    assert_eq!(rec.tuning, rigid_band);
    // The rigid record tuned the radio; the elastic one joined that capture.
    assert_eq!(radio.tunes(), vec![rigid_band]);
    let s = source(&node).await;
    assert_eq!(s.holders, vec![rigid]);
    assert_eq!(s.elastic, vec![elastic]);
}

/// A rigid start whose op future is dropped while it relaunches the elastic job (an
/// HTTP timeout, a client gone) still finishes the relaunch: the job runs on the new
/// capture with a supervisor, so a stop ends it and releases its lease.
#[tokio::test]
async fn dropped_op_mid_relaunch_leaves_no_zombie() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = Node::open(
        cfg(dir.path()),
        registry_lingering(Duration::from_millis(300)),
        vec![radio.clone()],
    )
    .await
    .unwrap();
    let elastic = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 9_600), true),
    )
    .await;
    let before = streams(&node, &elastic);
    let rigid_band = band(433_000_000, 9_600);
    // The relaunch waits ~300 ms for the old stages; the op is dropped long before.
    let r = tokio::time::timeout(
        Duration::from_millis(50),
        node.apply(start(JobKind::Passthrough, rigid_band, false)),
    )
    .await;
    assert!(r.is_err(), "the start finished inside the timeout: {r:?}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while record(&node, &elastic).tuning != rigid_band || streams(&node, &elastic).is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "never relaunched");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(record(&node, &elastic).state, JobState::Running);
    assert!(streams(&node, &elastic).iter().all(|s| !before.contains(s)));

    node.job_stop(&elastic).await.unwrap();
    assert_eq!(record(&node, &elastic).state, JobState::Stopped);
    assert!(streams(&node, &elastic).is_empty());
    assert!(node.lease_elastic(&SourceId("radio0".into())).is_empty());
}

/// A `job_stop` that lands while a relaunch waits on the source's gate wins: the job
/// ends `Stopped` in the listing and on disk, and its lease is released.
#[tokio::test]
async fn stop_during_a_relaunch_is_not_undone() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = Arc::new(open(dir.path(), &radio).await);
    let elastic = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 9_600), true),
    )
    .await;
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
    let path = dir
        .path()
        .join("state")
        .join("jobs")
        .join(format!("{}.json", elastic.0));
    let disk: JobRecord = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(disk.state, JobState::Stopped);
    assert!(streams(&node, &elastic).is_empty());
    assert!(node.lease_elastic(&SourceId("radio0".into())).is_empty());
}

/// An elastic job that starts after a scan stopped the source's elastic jobs but
/// before the scan's lease is stopped too: once the scan holds the source no
/// elastic job can join, and the one that joined first does not outlive the scan's start.
#[tokio::test]
async fn elastic_start_while_a_scan_stops_the_others_is_stopped_too() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    // The first elastic job takes 100 ms to end, so the scan's first stop is still
    // waiting on it when the second elastic job starts.
    let node = Arc::new(
        Node::open(
            cfg(dir.path()),
            registry_lingering(Duration::from_millis(100)),
            vec![radio.clone()],
        )
        .await
        .unwrap(),
    );
    let first = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 9_600), true),
    )
    .await;
    let n = node.clone();
    let scan = tokio::spawn(async move {
        n.apply(start(JobKind::Scan, band(100_000_000, 40_000_000), false))
            .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !scan.is_finished(),
        "the scan's stop of the first job lingers"
    );
    let second = started(
        &node,
        start(JobKind::Spectrum, band(100_000_000, 9_600), true),
    )
    .await;
    let scan = match scan.await.unwrap() {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected the scan to start, got {other:?}"),
    };
    for id in [&first, &second] {
        assert_eq!(record(&node, id).state, JobState::Stopped, "{id:?}");
        assert!(streams(&node, id).is_empty(), "{id:?}");
    }
    let s = source(&node).await;
    assert_eq!(s.holders, vec![scan.clone()]);
    assert!(s.elastic.is_empty(), "{s:?}");
    node.job_stop(&scan).await.unwrap();
}
