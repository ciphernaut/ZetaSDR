//! Atomic replace: `job_start` and `source_ensure` name the holders that may be
//! stopped to make room, and the node stops them under the source's gate right
//! before the request, so nothing can take the radio in between.

use std::path::Path;
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

/// A radio that tunes to exactly the band asked for.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
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
            },
            cur: Mutex::new(None),
            tx: broadcast::channel(1).0,
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
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

/// Stands in for the dsp `fft` stage a `spectrum` or `scan` job runs.
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

const RADIO: &str = "radio0";

fn start_on(source: &str, kind: JobKind, t: Tuning, elastic: bool, replace: &[&JobId]) -> Op {
    Op::JobStart {
        kind,
        source: SourceId(source.into()),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
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
    // A scan time-shares the source: only stopping it frees the radio.
    let scan = started(
        &node,
        start(JobKind::Scan, band(100_000_000, 40_000_000), &[]),
    )
    .await;
    match node.apply(start(JobKind::Passthrough, far, &[])).await {
        Err(OpError::Conflict {
            holders, options, ..
        }) => {
            assert_eq!(holders, vec![scan.clone()]);
            assert_eq!(options, vec!["stop the scan holding the source"]);
        }
        other => panic!("expected conflict, got {other:?}"),
    }
    node.job_stop(&scan).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replace_stops_a_scan_holder() {
    for round in 0..10 {
        let dir = tempfile::tempdir().unwrap();
        // No dwell: on another worker the scan is always retuning, so it waits on
        // the gate the replace holds while the replace stops it.
        let cfg = NodeConfig {
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
    for round in 0..20 {
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(open(dir.path(), &[Radio::new(RADIO)]).await);
        let a = started(
            &node,
            start(JobKind::Passthrough, band(100_000_000, 48_000), &[]),
        )
        .await;
        let (n1, n2) = (node.clone(), node.clone());
        let b_op = start(JobKind::Passthrough, band(433_000_000, 48_000), &[&a]);
        let d_op = start(JobKind::Passthrough, band(868_000_000, 48_000), &[]);
        let (b, d) = within(async {
            tokio::join!(
                tokio::spawn(async move { n1.apply(b_op).await }),
                tokio::spawn(async move { n2.apply(d_op).await }),
            )
        })
        .await;
        let (b, d) = (b.unwrap(), d.unwrap());
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
        assert!(!holders.is_empty(), "round {round}");
        for (j, t) in &holders {
            assert!(
                fits(t, &cur),
                "round {round}: {j:?} at {t:?} outside {cur:?}"
            );
            assert_eq!(record(&node, j).state, JobState::Running, "round {round}");
        }
    }
}
