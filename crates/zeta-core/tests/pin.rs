//! Pinned captures: `source_pin` holds a source at a band without a job, a rigid
//! holder that is not a job, persisted in `sources/<id>.json` and restored before
//! anything else on resume; `source_unpin` releases it.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, SourceInfo};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{
    Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, StreamId, StreamKind, Timestamp,
};

/// A radio that tunes to exactly the band asked for, counting tunes and idles; with
/// `fail` set every tune is refused.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tunes: AtomicUsize,
    idles: AtomicUsize,
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
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
            },
            cur: Mutex::new(None),
            tunes: AtomicUsize::new(0),
            idles: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            tx: broadcast::channel(1).0,
        })
    }

    fn failing() -> Arc<Self> {
        let r = Self::new();
        r.fail.store(true, Ordering::SeqCst);
        r
    }

    fn tunes(&self) -> usize {
        self.tunes.load(Ordering::SeqCst)
    }

    fn idles(&self) -> usize {
        self.idles.load(Ordering::SeqCst)
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
        self.tunes.fetch_add(1, Ordering::SeqCst);
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
        self.idles.fetch_add(1, Ordering::SeqCst);
        *self.cur.lock().unwrap() = None;
    }
}

/// Stands in for the dsp `fft` stage a `spectrum` or `scan` job runs (core cannot
/// depend on it): one event per IQ frame, ending when cancelled or its input closes.
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

async fn open(dir: &Path, radio: &Arc<Radio>) -> Node {
    Node::open(cfg(dir), registry(), vec![radio.clone()])
        .await
        .unwrap()
}

fn band(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

fn radio0() -> SourceId {
    SourceId("radio0".into())
}

fn pin(t: Tuning, replace: Vec<JobId>) -> Op {
    Op::SourcePin {
        source: radio0(),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        replace,
    }
}

fn unpin() -> Op {
    Op::SourceUnpin { source: radio0() }
}

fn start(kind: JobKind, t: Tuning, elastic: bool, replace: Vec<JobId>) -> Op {
    Op::JobStart {
        kind,
        source: radio0(),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
        elastic,
        replace,
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

fn source_file(dir: &Path) -> std::path::PathBuf {
    dir.join("state").join("sources").join("radio0.json")
}

fn on_disk(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(source_file(dir)).unwrap()).unwrap()
}

fn tuning_json(t: Tuning) -> serde_json::Value {
    serde_json::json!({"center_hz": t.center_hz.0, "bandwidth_hz": t.bandwidth_hz.0})
}

/// The conflict's holders and options, panicking on anything else.
fn conflict(r: Result<OpResult, OpError>) -> (Vec<JobId>, Vec<String>) {
    match r {
        Err(OpError::Conflict {
            holders, options, ..
        }) => (holders, options),
        other => panic!("expected a conflict, got {other:?}"),
    }
}

const UNPIN: &str = "unpin source radio0 (source_unpin)";

#[tokio::test]
async fn pin_tunes_an_unheld_source_and_is_listed() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let want = band(100_000_000, 48_000);
    let got = node.apply(pin(want, vec![])).await.unwrap();
    assert_eq!(got, OpResult::Tuning(want));
    assert_eq!(radio.current(), Some(want));
    let s = source(&node).await;
    assert_eq!(s.pin, Some(want));
    assert!(s.holders.is_empty(), "{s:?}");
    assert!(s.elastic.is_empty(), "{s:?}");
    let rec = on_disk(dir.path());
    assert_eq!(rec["format"], 1, "{rec}");
    assert_eq!(rec["pin"], tuning_json(want), "{rec}");
    // The listing's JSON carries `pin` too.
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["pin"], tuning_json(want), "{json}");
}

#[tokio::test]
async fn pin_is_a_rigid_holder_without_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let pinned = band(100_000_000, 1_000_000);
    node.apply(pin(pinned, vec![])).await.unwrap();

    let outside = band(433_000_000, 48_000);
    let (holders, options) = conflict(
        node.apply(start(JobKind::Passthrough, outside, false, vec![]))
            .await,
    );
    assert!(holders.is_empty(), "{holders:?}");
    assert!(options.iter().any(|o| o == UNPIN), "{options:?}");
    assert!(
        !options.iter().any(|o| o.starts_with("retry with replace")),
        "{options:?}"
    );
    // No job holds it: there is no holder to stop.
    assert!(
        !options.iter().any(|o| o == "stop one of the holders"),
        "{options:?}"
    );

    let (holders, options) = conflict(
        node.apply(Op::SourceEnsure {
            source: radio0(),
            center_hz: outside.center_hz,
            bandwidth_hz: outside.bandwidth_hz,
            replace: vec![],
        })
        .await,
    );
    assert!(holders.is_empty(), "{holders:?}");
    assert!(options.iter().any(|o| o == UNPIN), "{options:?}");

    let tunes = radio.tunes();
    let inside = band(100_200_000, 48_000);
    let job = started(&node, start(JobKind::Passthrough, inside, false, vec![])).await;
    assert_eq!(radio.tunes(), tunes);
    assert_eq!(radio.current(), Some(pinned));
    assert_eq!(source(&node).await.holders, vec![job.clone()]);

    // With a job holding it too: the unpin option comes before the replace line.
    let (holders, options) = conflict(
        node.apply(start(JobKind::Passthrough, outside, false, vec![]))
            .await,
    );
    assert_eq!(holders, vec![job.clone()]);
    assert_eq!(options[0], "stop one of the holders", "{options:?}");
    let unpin_at = options.iter().position(|o| o == UNPIN).expect("unpin");
    let replace_at = options
        .iter()
        .position(|o| o.starts_with("retry with replace"))
        .expect("replace");
    assert!(unpin_at < replace_at, "{options:?}");

    let pin_id = JobId("pin:radio0".into());
    match node
        .apply(start(JobKind::Passthrough, outside, false, vec![pin_id]))
        .await
    {
        Err(OpError::Unknown { field, .. }) => assert_eq!(field, "job"),
        other => panic!("expected unknown_job, got {other:?}"),
    }
    assert_eq!(node.pinned(&radio0()), Some(pinned));
}

#[tokio::test]
async fn elastic_jobs_join_the_pin_and_follow_a_repin() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let a = band(100_000_000, 1_000_000);
    node.apply(pin(a, vec![])).await.unwrap();
    let elastic = started(
        &node,
        start(JobKind::Spectrum, band(433_000_000, 48_000), true, vec![]),
    )
    .await;
    assert_eq!(record(&node, &elastic).tuning, a);
    let before = streams(&node, &elastic);
    assert!(!before.is_empty());

    let b = band(146_000_000, 2_000_000);
    assert_eq!(
        node.apply(pin(b, vec![])).await.unwrap(),
        OpResult::Tuning(b)
    );
    assert_eq!(radio.current(), Some(b));
    let rec = record(&node, &elastic);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, b);
    let after = streams(&node, &elastic);
    assert!(!after.is_empty());
    assert!(
        after.iter().all(|s| !before.contains(s)),
        "{before:?} {after:?}"
    );
    let s = source(&node).await;
    assert_eq!(s.pin, Some(b));
    assert_eq!(s.elastic, vec![elastic]);
}

/// R13: a pin is about the capture itself, so with only elastic jobs on the
/// source it retunes to its band even when that band fits the current capture
/// (a decoder inside the capture would not); the elastic jobs follow it.
#[tokio::test]
async fn pin_inside_an_elastic_only_capture_retunes_to_the_pin() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let wide = band(476_920_350, 4_096_000);
    let elastic = started(&node, start(JobKind::Spectrum, wide, true, vec![])).await;
    assert_eq!(radio.current(), Some(wide));
    let before = streams(&node, &elastic);
    assert!(!before.is_empty());
    let tunes = radio.tunes();

    let want = band(476_918_750, 1_000_000);
    assert_eq!(
        node.apply(pin(want, vec![])).await.unwrap(),
        OpResult::Tuning(want)
    );
    assert_eq!(radio.tunes(), tunes + 1);
    assert_eq!(radio.current(), Some(want));
    let s = source(&node).await;
    assert_eq!(s.pin, Some(want));
    assert_eq!(s.current, Some(want));
    assert_eq!(s.elastic, vec![elastic.clone()]);
    let rec = record(&node, &elastic);
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(rec.tuning, want);
    let after = streams(&node, &elastic);
    assert!(!after.is_empty());
    assert!(
        after.iter().all(|s| !before.contains(s)),
        "{before:?} {after:?}"
    );
}

#[tokio::test]
async fn repin_over_job_holders_outside_their_capture_is_a_conflict_and_keeps_the_old_pin() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    let a = band(100_000_000, 1_000_000);
    node.apply(pin(a, vec![])).await.unwrap();
    let job = started(
        &node,
        start(
            JobKind::Passthrough,
            band(100_100_000, 48_000),
            false,
            vec![],
        ),
    )
    .await;

    let b = band(433_000_000, 1_000_000);
    let (holders, _) = conflict(node.apply(pin(b, vec![])).await);
    assert_eq!(holders, vec![job.clone()]);
    assert_eq!(node.pinned(&radio0()), Some(a));
    assert_eq!(on_disk(dir.path())["pin"], tuning_json(a));

    assert_eq!(
        node.apply(pin(b, vec![job.clone()])).await.unwrap(),
        OpResult::Tuning(b)
    );
    assert_eq!(node.pinned(&radio0()), Some(b));
    assert_eq!(record(&node, &job).state, JobState::Stopped);
    assert_eq!(on_disk(dir.path())["pin"], tuning_json(b));
}

#[tokio::test]
async fn unpin_releases_and_idles() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    node.apply(pin(band(100_000_000, 48_000), vec![]))
        .await
        .unwrap();
    let idles = radio.idles();
    assert_eq!(node.apply(unpin()).await.unwrap(), OpResult::Ok);
    assert_eq!(source(&node).await.pin, None);
    assert_eq!(on_disk(dir.path())["pin"], serde_json::Value::Null);
    assert!(radio.idles() > idles, "the port was idled");
    match node.apply(unpin()).await {
        Err(OpError::Other { message }) => assert_eq!(message, "source radio0 is not pinned"),
        other => panic!("expected other, got {other:?}"),
    }
}

/// The unpin waits for the source's gate: the lease release and the record write land
/// together, so a `source_pin` racing it cannot leave disk and memory disagreeing.
#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn unpin_waits_for_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new();
    let node = Arc::new(open(dir.path(), &radio).await);
    let pinned = band(100_000_000, 48_000);
    node.apply(pin(pinned, vec![])).await.unwrap();
    let gate = node.debug_gate(&radio0()).await;
    let n = node.clone();
    let unpin = tokio::spawn(async move { n.apply(unpin()).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        node.pinned(&radio0()),
        Some(pinned),
        "released under the gate"
    );
    assert_eq!(on_disk(dir.path())["pin"], tuning_json(pinned));
    drop(gate);
    assert_eq!(unpin.await.unwrap().unwrap(), OpResult::Ok);
    assert_eq!(node.pinned(&radio0()), None);
    assert_eq!(on_disk(dir.path())["pin"], serde_json::Value::Null);
}

/// A `Running` record on `radio0`, as a node persists one.
fn running(kind: &str, tuning: Tuning, created_ns: u64) -> JobRecord {
    let graph = match kind {
        "scan" => Graph {
            id: GraphId::new(),
            nodes: vec![],
            edges: vec![],
        },
        _ => Graph {
            id: GraphId::new(),
            nodes: vec![GraphNode {
                id: NodeId("p".into()),
                kind: "passthrough".into(),
                params: serde_json::json!({"kind": "iq"}),
            }],
            edges: vec![],
        },
    };
    JobRecord {
        format: JOB_RECORD_FORMAT,
        id: JobId::new(),
        kind: kind.into(),
        graph,
        source: radio0(),
        tuning,
        state: JobState::Running,
        context: serde_json::json!({}),
        created_ns: Timestamp(created_ns),
        input: None,
        capture_rate_hz: None,
        elastic: false,
        operator_graph: false,
        owner: None,
    }
}

fn write_state(dir: &Path, pinned: Tuning, recs: &[JobRecord]) {
    let jobs = dir.join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    for r in recs {
        std::fs::write(
            jobs.join(format!("{}.json", r.id.0)),
            serde_json::to_vec(r).unwrap(),
        )
        .unwrap();
    }
    let sources = dir.join("state").join("sources");
    std::fs::create_dir_all(&sources).unwrap();
    let rec = serde_json::json!({"format": 1, "pin": tuning_json(pinned), "gain": null});
    std::fs::write(source_file(dir), serde_json::to_vec(&rec).unwrap()).unwrap();
}

fn failed_reason(node: &Node, id: &JobId) -> String {
    match record(node, id).state {
        JobState::Failed { reason } => reason,
        s => panic!("{id:?} resumed as {s:?}"),
    }
}

#[tokio::test]
async fn pin_resumes_before_the_records_and_a_scan_fails_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let pinned = band(100_000_000, 1_000_000);
    let inside = running("passthrough", band(100_200_000, 48_000), 1);
    let outside = running("passthrough", band(433_000_000, 2_000_000), 2);
    let scan = running("scan", band(400_000_000, 40_000_000), 3);
    write_state(
        dir.path(),
        pinned,
        &[inside.clone(), outside.clone(), scan.clone()],
    );
    let radio = Radio::new();
    let node = open(dir.path(), &radio).await;
    assert!(node.load_errors().is_empty(), "{:?}", node.load_errors());
    assert_eq!(source(&node).await.pin, Some(pinned));
    assert_eq!(radio.current(), Some(pinned));
    assert_eq!(record(&node, &inside.id).state, JobState::Running);
    let r = failed_reason(&node, &outside.id);
    assert!(r.contains("unpin source"), "{r}");
    let r = failed_reason(&node, &scan.id);
    assert!(r.contains("unpin source"), "{r}");
    assert_eq!(radio.tunes(), 1, "only the pin tuned");
}

#[tokio::test]
async fn a_pin_whose_tune_fails_is_a_load_error_and_stays_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    write_state(dir.path(), band(100_000_000, 48_000), &[]);
    let before = std::fs::read(source_file(dir.path())).unwrap();
    let radio = Radio::failing();
    let node = open(dir.path(), &radio).await;
    let errors = node.load_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("source radio0: pin "), "{errors:?}");
    assert_eq!(std::fs::read(source_file(dir.path())).unwrap(), before);
    assert_eq!(node.pinned(&radio0()), None);
}
