//! `plan_start`: one kind on every channel of a plan (or the channels named), under
//! one lease decision for the band bounding them, answering what started, what
//! already ran and what was refused.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;

use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, SourceInfo};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId};

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
        Self::with_max(id, 20_000_000)
    }

    fn with_max(id: &str, max_bandwidth_hz: u64) -> Arc<Self> {
        Arc::new(Self {
            id: SourceId(id.into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(max_bandwidth_hz)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: false,
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

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
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

async fn open(dir: &Path, radio: &Arc<Radio>) -> Node {
    let ports: Vec<Arc<dyn Port>> = vec![radio.clone()];
    Node::open(cfg(dir), registry(), ports).await.unwrap()
}

fn band(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

const RADIO: &str = "radio0";
const PLAN: &str = "uhf_cb_au";

fn plan_op(
    plan: &str,
    channels: Option<Vec<u32>>,
    kind: JobKind,
    attach: Option<JobKind>,
    language: Option<&str>,
    replace: &[&JobId],
) -> Op {
    Op::PlanStart {
        plan: plan.into(),
        channels,
        kind,
        source: SourceId(RADIO.into()),
        deviation_hz: None,
        sideband: None,
        attach,
        language: language.map(String::from),
        replace: replace.iter().map(|j| (*j).clone()).collect(),
    }
}

/// `fm_demod` on the channels named (`None` for all), nothing attached.
fn fm_plan(channels: Option<Vec<u32>>, replace: &[&JobId]) -> Op {
    plan_op(PLAN, channels, JobKind::FmDemod, None, None, replace)
}

/// A `plan_start` answer, reduced to what the tests read.
struct Answer {
    capture: Tuning,
    /// `(channel, center_hz, job)` per started channel.
    started: Vec<(u32, u64, JobId)>,
    /// `(channel, job)` per existing channel.
    existing: Vec<(u32, JobId)>,
    refused: usize,
    warnings: Vec<String>,
}

async fn planned(node: &Node, op: Op) -> Answer {
    match node.apply(op).await {
        Ok(OpResult::PlanStart {
            plan,
            source,
            capture,
            started,
            existing,
            refused,
            warnings,
        }) => {
            assert_eq!(plan, PLAN);
            assert_eq!(source.0, RADIO);
            Answer {
                capture,
                started: started
                    .into_iter()
                    .map(|s| {
                        assert_eq!(s.attached, None);
                        assert_eq!(s.attach_error, None);
                        (s.channel, s.center_hz.0, s.job)
                    })
                    .collect(),
                existing: existing.into_iter().map(|e| (e.channel, e.job)).collect(),
                refused: refused.len(),
                warnings,
            }
        }
        other => panic!("expected plan_start, got {other:?}"),
    }
}

async fn started(node: &Node, op: Op) -> JobId {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    }
}

fn start(kind: JobKind, t: Tuning) -> Op {
    Op::JobStart {
        kind,
        source: SourceId(RADIO.into()),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    }
}

async fn source(node: &Node) -> SourceInfo {
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v.into_iter().find(|s| s.id.0 == RADIO).expect("listed"),
        r => panic!("source_list: {r:?}"),
    }
}

fn running(node: &Node) -> Vec<JobId> {
    node.job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running)
        .map(|r| r.id)
        .collect()
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

fn other_message(r: Result<OpResult, OpError>) -> String {
    match r {
        Err(OpError::Other { message }) => message,
        other => panic!("expected other, got {other:?}"),
    }
}

#[tokio::test]
async fn plan_start_starts_every_channel_under_one_tune() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let a = planned(&node, fm_plan(None, &[])).await;
    assert_eq!(a.started.len(), 80);
    let numbers: Vec<u32> = a.started.iter().map(|s| s.0).collect();
    assert_eq!(numbers, (1..=80).collect::<Vec<u32>>());
    assert_eq!(a.started[0].1, 476_425_000);
    assert_eq!(a.started[79].1, 477_412_500);
    assert!(a.existing.is_empty());
    assert_eq!(a.refused, 0);
    assert!(a.warnings.is_empty());
    assert_eq!(source(&node).await.holders.len(), 80);
    assert_eq!(radio.tunes(), 1);
    // ch1's lower edge 476_418_750 to ch80's upper edge 477_418_750.
    assert_eq!(a.capture, band(476_918_750, 1_000_000));
    assert_eq!(radio.current(), Some(band(476_918_750, 1_000_000)));
    for r in node.job_list() {
        assert_eq!(r.kind, "fm_demod");
        assert_eq!(r.tuning.bandwidth_hz, Hz(12_500));
        assert_eq!(r.input, None);
    }
}

#[tokio::test]
async fn a_holder_outside_the_band_is_a_conflict_and_nothing_starts() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let two_metres = band(144_800_000, 12_500);
    let h = started(&node, start(JobKind::Passthrough, two_metres)).await;
    let (holders, _) = conflict(node.apply(fm_plan(None, &[])).await);
    assert_eq!(holders, vec![h.clone()]);
    assert_eq!(running(&node), vec![h.clone()]);
    assert_eq!(radio.current(), Some(two_metres));
    let a = planned(&node, fm_plan(None, &[&h])).await;
    assert_eq!(a.started.len(), 80);
    let rec = node.job_list().into_iter().find(|r| r.id == h).unwrap();
    assert_eq!(rec.state, JobState::Stopped);
    assert_eq!(running(&node).len(), 80);
}

#[tokio::test]
async fn a_scan_on_the_source_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let scan = started(&node, start(JobKind::Scan, band(403_000_000, 6_000_000))).await;
    let (holders, _) = conflict(node.apply(fm_plan(None, &[])).await);
    assert_eq!(holders, vec![scan.clone()]);
    assert_eq!(running(&node), vec![scan]);
}

#[tokio::test]
async fn inside_a_pin_nothing_retunes_and_a_pin_elsewhere_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let pin = |t: Tuning| Op::SourcePin {
        source: SourceId(RADIO.into()),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        replace: vec![],
    };
    node.apply(pin(band(476_918_750, 1_024_000))).await.unwrap();
    assert_eq!(radio.tunes(), 1);
    let a = planned(&node, fm_plan(None, &[])).await;
    assert_eq!(a.started.len(), 80);
    assert_eq!(radio.tunes(), 1, "the pin's capture holds every channel");
    assert_eq!(a.capture, band(476_918_750, 1_024_000));
    for id in running(&node) {
        node.apply(Op::JobStop { job: id }).await.unwrap();
    }
    node.apply(Op::SourceUnpin {
        source: SourceId(RADIO.into()),
    })
    .await
    .unwrap();
    node.apply(pin(band(144_800_000, 1_000_000))).await.unwrap();
    let (_, options) = conflict(node.apply(fm_plan(None, &[])).await);
    assert!(
        options
            .iter()
            .any(|o| o == "unpin source radio0 (source_unpin)"),
        "{options:?}"
    );
    assert!(running(&node).is_empty());
}

#[tokio::test]
async fn channels_are_validated_before_anything_happens() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let a = planned(&node, fm_plan(Some(vec![1, 2, 41]), &[])).await;
    let numbers: Vec<u32> = a.started.iter().map(|s| s.0).collect();
    assert_eq!(numbers, vec![1, 2, 41]);
    let before = node.job_list().len();
    let tunes = radio.tunes();

    assert_eq!(
        other_message(node.apply(fm_plan(Some(vec![1, 1]), &[])).await),
        "channels: 1 is listed twice"
    );
    match node.apply(fm_plan(Some(vec![81]), &[])).await {
        Err(OpError::Unknown {
            field,
            value,
            known,
        }) => {
            assert_eq!(field, "channels");
            assert_eq!(value, "81");
            assert_eq!(known.len(), 80);
        }
        other => panic!("expected unknown channel, got {other:?}"),
    }
    match node
        .apply(plan_op("nope", None, JobKind::FmDemod, None, None, &[]))
        .await
    {
        Err(OpError::Unknown { field, known, .. }) => {
            assert_eq!(field, "plan");
            assert_eq!(known, vec!["uhf_cb_au".to_string()]);
        }
        other => panic!("expected unknown plan, got {other:?}"),
    }
    let kind_refusal = |kind: JobKind| plan_op(PLAN, None, kind, None, None, &[]);
    assert_eq!(
        other_message(node.apply(kind_refusal(JobKind::Spectrum)).await),
        "spectrum cannot be started per channel"
    );
    assert_eq!(
        other_message(node.apply(kind_refusal(JobKind::Transcribe)).await),
        "transcribe cannot be started per channel"
    );
    let attach_fm = plan_op(
        PLAN,
        None,
        JobKind::FmDemod,
        Some(JobKind::FmDemod),
        None,
        &[],
    );
    assert_eq!(
        other_message(node.apply(attach_fm).await),
        "fm_demod is not attachable"
    );
    let language_alone = plan_op(PLAN, None, JobKind::FmDemod, None, Some("de"), &[]);
    assert_eq!(
        other_message(node.apply(language_alone).await),
        "language needs attach"
    );
    assert_eq!(node.job_list().len(), before);
    assert_eq!(running(&node).len(), 3);
    assert_eq!(radio.tunes(), tunes);
}

#[tokio::test]
async fn a_second_plan_start_adds_only_what_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let first = planned(&node, fm_plan(None, &[])).await;
    assert_eq!(first.started.len(), 80);
    let second = planned(&node, fm_plan(None, &[])).await;
    assert!(second.started.is_empty());
    let want: Vec<(u32, JobId)> = first.started.into_iter().map(|(n, _, j)| (n, j)).collect();
    assert_eq!(second.existing, want);
    assert_eq!(second.capture, band(476_918_750, 1_000_000));
    assert_eq!(running(&node).len(), 80);
    assert_eq!(radio.tunes(), 1);
}

#[tokio::test]
async fn a_bounding_band_wider_than_the_source_is_out_of_range() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::with_max(RADIO, 500_000);
    let node = open(dir.path(), &radio).await;
    match node.apply(fm_plan(None, &[])).await {
        Err(OpError::OutOfRange {
            field, value, max, ..
        }) => {
            assert_eq!(field, "bandwidth_hz");
            assert_eq!(value, 1_000_000);
            assert_eq!(max, 500_000);
        }
        other => panic!("expected out_of_range, got {other:?}"),
    }
    assert!(node.job_list().is_empty());
    assert_eq!(radio.tunes(), 0);
}

#[tokio::test]
async fn a_restart_resumes_the_plan_jobs_as_ordinary_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let a = planned(&node, fm_plan(Some(vec![1, 2, 3]), &[])).await;
    assert_eq!(a.started.len(), 3);
    drop(node);
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let fm: Vec<_> = node
        .job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running && r.kind == "fm_demod")
        .collect();
    assert_eq!(fm.len(), 3);
    assert_eq!(source(&node).await.holders.len(), 3);
}
