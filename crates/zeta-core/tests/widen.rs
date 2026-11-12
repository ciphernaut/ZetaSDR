//! Widening a held capture (`source_ensure` with `widen`) and the capture policy
//! (`source_set_capture`): a capture jobs hold moves when every holder still fits in
//! it, the holders restarting on it under their ids; under `widest` a rigid job's tune
//! of a free source takes the source's whole rate.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
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
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{
    Capabilities, CapturePolicy, Claim, Hz, JobId, SampleFormat, SourceId, StreamId, StreamKind,
};

/// A radio that tunes to exactly the band asked for, counting its tunes.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
    tunes: AtomicUsize,
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
            tunes: AtomicUsize::new(0),
        })
    }

    fn tunes(&self) -> usize {
        self.tunes.load(Ordering::SeqCst)
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
        self.tunes.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(2)).await;
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

/// Stands in for the `transcribe` stage, which lives in another crate: it waits for
/// its input to close or the job to stop.
struct Transcribe(StageSpec);

#[async_trait]
impl Stage for Transcribe {
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
        _outputs: Vec<StreamId>,
        _bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = inputs[0].recv() => match r {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r.register(Arc::new(StreamInStage::new()));
    r.register(Arc::new(Transcribe(StageSpec {
        kind: "transcribe",
        inputs: vec![StreamKind::Audio],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({"type": "object"}),
    })));
    // Stands in for the program rows' stage (zeta-ext's): `adsb_decode` starts on it.
    r.register(Arc::new(Transcribe(StageSpec {
        kind: "external_decode",
        inputs: vec![StreamKind::Iq, StreamKind::Audio],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({"type": "object"}),
    })));
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

const RADIO: &str = "radio0";

fn t(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

async fn open(dir: &Path) -> (Node, Arc<Radio>) {
    let radio = Radio::new(RADIO);
    let node = Node::open(cfg(dir), registry(), vec![radio.clone() as Arc<dyn Port>])
        .await
        .unwrap();
    (node, radio)
}

async fn start_as(node: &Node, kind: JobKind, band: Tuning, elastic: bool) -> JobId {
    let op = Op::JobStart {
        kind,
        source: SourceId(RADIO.into()),
        center_hz: band.center_hz,
        bandwidth_hz: band.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        elastic,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    }
}

async fn start(node: &Node, kind: JobKind, band: Tuning) -> JobId {
    start_as(node, kind, band, false).await
}

async fn pin(node: &Node, band: Tuning) {
    node.apply(Op::SourcePin {
        source: SourceId(RADIO.into()),
        center_hz: band.center_hz,
        bandwidth_hz: band.bandwidth_hz,
        replace: vec![],
    })
    .await
    .unwrap();
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

async fn source(node: &Node) -> SourceInfo {
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v.into_iter().find(|s| s.id.0 == RADIO).expect("listed"),
        r => panic!("source_list: {r:?}"),
    }
}

async fn stop(node: &Node, id: &JobId) {
    node.apply(Op::JobStop { job: id.clone() }).await.unwrap();
}

async fn ensure(node: &Node, band: Tuning, widen: bool) -> Result<OpResult, OpError> {
    let op = Op::SourceEnsure {
        source: SourceId(RADIO.into()),
        center_hz: band.center_hz,
        bandwidth_hz: band.bandwidth_hz,
        replace: vec![],
        widen,
    };
    tokio::time::timeout(Duration::from_secs(10), node.apply(op))
        .await
        .expect("no deadlock")
}

async fn set_policy(node: &Node, policy: CapturePolicy) {
    let op = Op::SourceSetCapture {
        source: SourceId(RADIO.into()),
        policy,
    };
    assert_eq!(node.apply(op).await, Ok(OpResult::Ok));
}

/// Poll until `id`'s record satisfies `done`.
async fn until(node: &Node, id: &JobId, what: &str, done: impl Fn(&JobRecord) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rec = record(node, id);
        if done(&rec) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}: {rec:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_held_capture_is_not_moved_without_widen_and_the_conflict_offers_it() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let capture = t(100_000_000, 1_000_000);
    let a = start(&node, JobKind::Passthrough, capture).await;
    let tunes = radio.tunes();
    let Err(OpError::Conflict {
        holders, options, ..
    }) = ensure(&node, t(100_000_000, 4_000_000), false).await
    else {
        panic!("expected a conflict");
    };
    assert_eq!(holders, vec![a]);
    assert!(
        options.iter().any(|o| o
            == "widen the capture to 100000000/4000000 Hz (source_ensure with widen: the holders restart on it)"),
        "{options:?}"
    );
    assert_eq!(source(&node).await.current, Some(capture));
    assert_eq!(radio.tunes(), tunes);
}

#[tokio::test]
async fn widen_moves_the_capture_and_restarts_every_holder_under_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let capture = t(100_000_000, 1_000_000);
    let a = start(&node, JobKind::Passthrough, capture).await;
    let b = start(&node, JobKind::FmDemod, t(100_100_000, 12_500)).await;
    let el = start_as(&node, JobKind::Spectrum, capture, true).await;
    let before = record(&node, &b);
    assert_eq!(before.capture_rate_hz, Some(1_000_000));
    let tunes = radio.tunes();
    let wide = t(100_000_000, 4_000_000);
    assert_eq!(ensure(&node, wide, true).await, Ok(OpResult::Tuning(wide)));
    assert_eq!(
        radio.tunes(),
        tunes + 1,
        "one tune, and no holder tuned again"
    );
    let s = source(&node).await;
    assert_eq!(s.current, Some(wide));
    let mut held = s.holders.clone();
    held.sort();
    let mut want = vec![a.clone(), b.clone()];
    want.sort();
    assert_eq!(held, want, "both keep their lease");
    let after = record(&node, &b);
    assert_eq!(after.state, JobState::Running);
    assert_eq!(after.tuning, t(100_100_000, 12_500));
    assert_eq!(
        after.capture_rate_hz,
        Some(4_000_000),
        "expanded for the new rate"
    );
    assert_ne!(
        before.graph, after.graph,
        "the channel is made again for the new capture"
    );
    assert_eq!(record(&node, &a).state, JobState::Running);
    assert_eq!(record(&node, &a).tuning, capture);
    until(&node, &el, "the elastic job did not follow", |r| {
        r.tuning == wide && r.state == JobState::Running
    })
    .await;
}

#[tokio::test]
async fn widen_shifts_the_capture_when_the_holders_stay_inside() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let a = start(&node, JobKind::Passthrough, t(100_000_000, 1_000_000)).await;
    let shifted = t(100_400_000, 2_000_000);
    assert_eq!(
        ensure(&node, shifted, true).await,
        Ok(OpResult::Tuning(shifted))
    );
    assert_eq!(source(&node).await.current, Some(shifted));
    assert_eq!(record(&node, &a).state, JobState::Running);
}

#[tokio::test]
async fn widen_that_would_leave_a_holder_outside_is_a_conflict_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    let capture = t(100_000_000, 1_000_000);
    let a = start(&node, JobKind::Passthrough, capture).await;
    let tunes = radio.tunes();
    let Err(OpError::Conflict {
        holders, current, ..
    }) = ensure(&node, t(102_000_000, 2_000_000), true).await
    else {
        panic!("expected a conflict");
    };
    assert_eq!(holders, vec![a.clone()]);
    assert_eq!(current, capture);
    assert_eq!(source(&node).await.current, Some(capture));
    assert_eq!(radio.tunes(), tunes);
    assert_eq!(record(&node, &a).state, JobState::Running);
}

#[tokio::test]
async fn widen_keeps_a_pin_inside_the_new_capture() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let pinned = t(476_918_750, 1_024_000);
    pin(&node, pinned).await;
    let id = start(&node, JobKind::FmDemod, t(476_900_000, 12_500)).await;
    let wide = t(477_000_000, 4_000_000);
    assert_eq!(ensure(&node, wide, true).await, Ok(OpResult::Tuning(wide)));
    let s = source(&node).await;
    assert_eq!(s.current, Some(wide));
    assert_eq!(s.pin, Some(pinned));
    assert_eq!(s.holders, vec![id.clone()]);
    assert_eq!(record(&node, &id).capture_rate_hz, Some(4_000_000));
    // A band that would leave the pin outside is refused.
    assert!(matches!(
        ensure(&node, t(480_000_000, 4_000_000), true).await,
        Err(OpError::Conflict { .. })
    ));
    assert_eq!(source(&node).await.current, Some(wide));
}

#[tokio::test]
async fn widen_on_a_free_source_is_a_plain_ensure() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _radio) = open(dir.path()).await;
    let band = t(100_000_000, 2_000_000);
    assert_eq!(ensure(&node, band, true).await, Ok(OpResult::Tuning(band)));
    assert_eq!(source(&node).await.current, Some(band));
}

#[tokio::test]
async fn under_widest_a_rigid_job_takes_the_whole_rate_and_leases_its_own_band() {
    let dir = tempfile::tempdir().unwrap();
    let (node, radio) = open(dir.path()).await;
    assert_eq!(source(&node).await.capture_policy, CapturePolicy::Fit);
    set_policy(&node, CapturePolicy::Widest).await;
    assert_eq!(source(&node).await.capture_policy, CapturePolicy::Widest);
    let a = start(&node, JobKind::FmDemod, t(476_900_000, 12_500)).await;
    let whole = t(476_900_000, 20_000_000);
    assert_eq!(source(&node).await.current, Some(whole));
    assert_eq!(record(&node, &a).capture_rate_hz, Some(20_000_000));
    assert_eq!(
        node.lease_holders(&SourceId(RADIO.into())),
        vec![(a.clone(), t(476_900_000, 12_500))],
        "the lease is the job's band, never the capture"
    );
    // A job megahertz away fits beside it untuned.
    let tunes = radio.tunes();
    let b = start(&node, JobKind::FmDemod, t(484_000_000, 12_500)).await;
    assert_eq!(radio.tunes(), tunes);
    assert_eq!(source(&node).await.current, Some(whole));
    // And the capture can shift about both.
    let shifted = t(480_000_000, 20_000_000);
    assert_eq!(
        ensure(&node, shifted, true).await,
        Ok(OpResult::Tuning(shifted))
    );
    assert_eq!(record(&node, &a).state, JobState::Running);
    assert_eq!(record(&node, &b).state, JobState::Running);
    // An explicit ensure and an elastic job tune to their own band whatever the policy.
    stop(&node, &a).await;
    stop(&node, &b).await;
    let own = t(100_000_000, 1_000_000);
    assert_eq!(ensure(&node, own, false).await, Ok(OpResult::Tuning(own)));
}

#[tokio::test]
async fn the_capture_policy_is_persisted_and_applied_at_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let (node, _radio) = open(dir.path()).await;
        set_policy(&node, CapturePolicy::Widest).await;
    }
    let (node, _radio) = open(dir.path()).await;
    assert_eq!(source(&node).await.capture_policy, CapturePolicy::Widest);
    start(&node, JobKind::FmDemod, t(476_900_000, 12_500)).await;
    assert_eq!(
        source(&node).await.current,
        Some(t(476_900_000, 20_000_000))
    );
    set_policy(&node, CapturePolicy::Fit).await;
    assert_eq!(
        source(&node).await.current,
        Some(t(476_900_000, 20_000_000)),
        "a held capture stays"
    );
}
