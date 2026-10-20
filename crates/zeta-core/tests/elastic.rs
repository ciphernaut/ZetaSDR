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
/// `fail` set every tune is refused.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tunes: Mutex<Vec<Tuning>>,
    fail: AtomicBool,
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
/// one event per IQ frame, ending when its input closes or it is cancelled.
struct FakeFft(StageSpec);

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
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
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
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(FakeFft(StageSpec {
        kind: "fft",
        inputs: vec![StreamKind::Iq],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({}),
    })));
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
