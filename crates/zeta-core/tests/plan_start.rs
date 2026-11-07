//! `plan_start`: one kind on every channel of a plan (or the channels named), under
//! one lease decision for the band bounding them, answering what started, what
//! already ran and what was refused.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;

use tokio_util::sync::CancellationToken;
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, SourceInfo};
use zeta_core::ops::{PlanExisting, PlanStarted};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Readiness, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId};
use zeta_core::types::{StreamId, StreamKind};

/// A radio that tunes to exactly the band asked for, counting its tunes; with
/// `slow` set a tune takes a second, so a caller's future can be dropped inside it.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
    tunes: AtomicUsize,
    slow: AtomicBool,
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
            slow: AtomicBool::new(false),
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
        if self.slow.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
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
        source_positions: Default::default(),
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
            assert_eq!(
                known,
                vec!["uhf_cb_au".to_string(), "acars_vhf".to_string()]
            );
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

/// A stand-in for an audio-in stage of the given kind and readiness.
struct Stand {
    spec: StageSpec,
    readiness: Readiness,
}

impl Stand {
    fn new(kind: &'static str, outputs: &[StreamKind], readiness: Readiness) -> Arc<Self> {
        Arc::new(Self {
            spec: StageSpec {
                kind,
                inputs: vec![StreamKind::Audio],
                outputs: outputs.to_vec(),
                params_schema: schemars::json_schema!({"type": "object"}),
            },
            readiness,
        })
    }
}

#[async_trait]
impl Stage for Stand {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    fn readiness(&self) -> Readiness {
        self.readiness.clone()
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
        let Some(rx) = inputs.first_mut() else {
            return Err(StageError::Failed("expected 1 input".into()));
        };
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        // Audio-out stages pass the audio on; event outputs stay quiet.
                        if let (Some(out), Payload::Audio { .. }) = (outputs.first(), &frame.payload)
                            && self.spec.outputs.first() == Some(&StreamKind::Audio)
                        {
                            bus.publish(Frame::new(
                                out.clone(),
                                frame.provenance.clone(),
                                frame.payload.clone(),
                            ));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

/// The registry with the attached chain's stand-ins; `transcribe` answers `readiness`.
fn attach_registry(readiness: Readiness) -> StageRegistry {
    let mut r = registry();
    r.register(Arc::new(StreamInStage::new()));
    r.register(Stand::new(
        "denoise",
        &[StreamKind::Audio],
        Readiness::always(),
    ));
    r.register(Stand::new(
        "vad",
        &[StreamKind::Audio, StreamKind::Event],
        Readiness::always(),
    ));
    r.register(Stand::new("transcribe", &[StreamKind::Event], readiness));
    r
}

async fn open_with(dir: &Path, radio: &Arc<Radio>, r: StageRegistry) -> Node {
    let ports: Vec<Arc<dyn Port>> = vec![radio.clone()];
    Node::open(cfg(dir), r, ports).await.unwrap()
}

fn fm_attach(channels: Vec<u32>) -> Op {
    plan_op(
        PLAN,
        Some(channels),
        JobKind::FmDemod,
        Some(JobKind::Transcribe),
        None,
        &[],
    )
}

fn capacity(c: u32) -> Readiness {
    Readiness {
        ready: true,
        reason: None,
        capacity: Some(c),
    }
}

type PlanParts = (Vec<PlanStarted>, Vec<PlanExisting>, Vec<String>);

fn plan_result(r: Result<OpResult, OpError>) -> PlanParts {
    match r {
        Ok(OpResult::PlanStart {
            started,
            existing,
            warnings,
            ..
        }) => (started, existing, warnings),
        other => panic!("expected plan_start, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unready_attach_refuses_the_whole_op_before_any_start() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open_with(
        dir.path(),
        &radio,
        attach_registry(Readiness::unready("no backend here")),
    )
    .await;
    let m = other_message(node.apply(fm_attach(vec![1, 2])).await);
    assert_eq!(m, "transcribe: no backend here; start without attach");
    assert!(node.job_list().is_empty());
    assert_eq!(radio.tunes(), 0);
}

#[tokio::test]
async fn attach_follows_every_started_channel_and_warns_over_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open_with(dir.path(), &radio, attach_registry(capacity(4))).await;
    let (started, existing, warnings) = plan_result(node.apply(fm_attach((1..=8).collect())).await);
    assert_eq!(started.len(), 8);
    assert!(existing.is_empty());
    assert_eq!(running(&node).len(), 16);
    for s in &started {
        let att = s.attached.clone().expect("attached");
        assert_eq!(s.attach_error, None);
        let rec = node.job_list().into_iter().find(|r| r.id == att).unwrap();
        assert_eq!(rec.input.as_ref(), Some(&s.job));
        assert_eq!(rec.kind, "transcribe");
    }
    assert_eq!(
        warnings,
        [
            "8 transcribe jobs share the speech backend's 4 slots: with the squelch on most channels are silent, so the slots are rarely contested; a busy daemon drops utterances with reason busy"
        ]
    );
    // Two channels alone stay under the capacity.
    let dir2 = tempfile::tempdir().unwrap();
    let radio2 = Radio::new(RADIO);
    let node2 = open_with(dir2.path(), &radio2, attach_registry(capacity(4))).await;
    let (started, _, warnings) = plan_result(node2.apply(fm_attach(vec![1, 2])).await);
    assert_eq!(started.len(), 2);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[tokio::test]
async fn the_data_channels_start_without_an_attached_transcribe() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open_with(dir.path(), &radio, attach_registry(Readiness::always())).await;
    let (started, _, warnings) = plan_result(node.apply(fm_attach(vec![20, 22, 23])).await);
    let by = |n| started.iter().find(|s| s.channel == n).unwrap();
    assert!(by(20).attached.is_some());
    for n in [22, 23] {
        assert_eq!(
            (by(n).attached.clone(), by(n).attach_error.clone()),
            (None, None)
        );
    }
    assert_eq!(running(&node).len(), 4, "three demods, one transcribe");
    assert_eq!(
        warnings,
        ["ch22, ch23: data, no voice: no transcribe attached"]
    );
}

#[tokio::test]
async fn a_second_run_with_attach_attaches_the_existing_demods() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open_with(dir.path(), &radio, attach_registry(Readiness::always())).await;
    let first = planned(&node, fm_plan(Some(vec![1, 2, 3]), &[])).await;
    assert_eq!(first.started.len(), 3);
    let (started, existing, warnings) = plan_result(node.apply(fm_attach(vec![1, 2, 3])).await);
    assert!(started.is_empty());
    assert!(warnings.is_empty());
    assert_eq!(existing.len(), 3);
    let ids: Vec<JobId> = existing
        .iter()
        .map(|e| e.attached.clone().expect("attached"))
        .collect();
    assert_eq!(running(&node).len(), 6);
    let (started, existing, _) = plan_result(node.apply(fm_attach(vec![1, 2, 3])).await);
    assert!(started.is_empty());
    let again: Vec<JobId> = existing
        .iter()
        .map(|e| e.attached.clone().unwrap())
        .collect();
    assert_eq!(again, ids);
    assert_eq!(running(&node).len(), 6);
}

#[tokio::test]
async fn an_existing_channel_that_cannot_attach_is_named_in_a_warning() {
    // `measure` publishes no audio, so a transcriber has nothing to read: the stand-in
    // `transcribe` is ready but `attach_one` fails on the existing job's audio lookup.
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open_with(dir.path(), &radio, attach_registry(Readiness::always())).await;
    let measure = |attach| plan_op(PLAN, Some(vec![1, 2]), JobKind::Measure, attach, None, &[]);
    let first = planned(&node, measure(None)).await;
    assert_eq!(first.started.len(), 2);
    let (started, existing, warnings) =
        plan_result(node.apply(measure(Some(JobKind::Transcribe))).await);
    assert!(started.is_empty());
    assert!(existing.iter().all(|e| e.attached.is_none()));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].starts_with("ch1: attach failed: "),
        "{warnings:?}"
    );
    assert!(
        warnings[1].starts_with("ch2: attach failed: "),
        "{warnings:?}"
    );
}

#[tokio::test]
async fn a_plan_start_dropped_mid_way_leaves_no_holder() {
    // A request timed out or disconnected drops the op's future: whatever it had
    // leased for the plan is released with it, and the source's gate goes too.
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    radio.slow.store(true, Ordering::SeqCst);
    let dropped =
        tokio::time::timeout(Duration::from_millis(50), node.apply(fm_plan(None, &[]))).await;
    assert!(
        dropped.is_err(),
        "the plan_start was not dropped: {dropped:?}"
    );
    assert_eq!(radio.tunes(), 1, "dropped inside the bounding band's tune");
    radio.slow.store(false, Ordering::SeqCst);
    let s = source(&node).await;
    assert!(s.holders.is_empty(), "holders: {:?}", s.holders);
    assert!(running(&node).is_empty());
    // A start elsewhere on the source neither conflicts nor waits on the gate.
    let two_metres = band(144_800_000, 12_500);
    let job = tokio::time::timeout(
        Duration::from_secs(5),
        started(&node, start(JobKind::Passthrough, two_metres)),
    )
    .await
    .expect("the gate was let go");
    assert_eq!(source(&node).await.holders, vec![job.clone()]);
    assert_eq!(radio.current(), Some(two_metres));
    assert_eq!(running(&node), vec![job]);
}

#[tokio::test]
async fn jobs_stop_stops_what_matches_and_keeps_the_elastic_spectrum() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let stop = |source: Option<&str>, kind: Option<JobKind>, plan: Option<&str>| Op::JobsStop {
        source: source.map(|s| zeta_core::types::SourceId(s.into())),
        kind,
        plan: plan.map(Into::into),
    };
    planned(&node, fm_plan(None, &[])).await;
    let elastic = started(
        &node,
        Op::JobStart {
            kind: JobKind::Spectrum,
            source: zeta_core::types::SourceId(RADIO.into()),
            center_hz: Hz(476_918_750),
            bandwidth_hz: Hz(1_000_000),
            deviation_hz: None,
            sideband: None,
            elastic: true,
            replace: vec![],
        },
    )
    .await;
    assert_eq!(running(&node).len(), 81);

    // Bad filters change nothing; a filter that matches nothing is an empty answer.
    let r = node.apply(stop(Some("nope"), None, None)).await;
    assert!(matches!(r, Err(OpError::Unknown { ref field, .. }) if field == "source"));
    let r = node.apply(stop(None, None, Some("nope"))).await;
    assert!(matches!(r, Err(OpError::Unknown { ref field, .. }) if field == "plan"));
    match node
        .apply(stop(None, Some(JobKind::Measure), None))
        .await
        .unwrap()
    {
        OpResult::JobsStopped { stopped, failed } => {
            assert!(stopped.is_empty() && failed.is_empty())
        }
        r => panic!("jobs_stop: {r:?}"),
    }
    assert_eq!(running(&node).len(), 81);

    match node.apply(stop(None, None, Some(PLAN))).await.unwrap() {
        OpResult::JobsStopped { stopped, failed } => {
            assert_eq!(stopped.len(), 80);
            assert!(failed.is_empty());
        }
        r => panic!("jobs_stop: {r:?}"),
    }
    assert_eq!(running(&node), vec![elastic]);
    assert!(source(&node).await.holders.is_empty());
}

#[tokio::test]
async fn jobs_stop_with_no_filter_stops_a_job_outside_any_plan() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let a = started(
        &node,
        start(JobKind::Passthrough, band(100_000_000, 48_000)),
    )
    .await;
    let r = node
        .apply(Op::JobsStop {
            source: None,
            kind: None,
            plan: Some(PLAN.into()),
        })
        .await
        .unwrap();
    assert!(matches!(r, OpResult::JobsStopped { ref stopped, .. } if stopped.is_empty()));
    assert_eq!(running(&node), vec![a.clone()]);
    let r = node
        .apply(Op::JobsStop {
            source: None,
            kind: None,
            plan: None,
        })
        .await
        .unwrap();
    assert!(matches!(r, OpResult::JobsStopped { ref stopped, .. } if *stopped == vec![a.clone()]));
    assert!(running(&node).is_empty());
}

#[tokio::test]
async fn a_whole_plan_result_keeps_its_counts_when_the_ops_record_is_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open(dir.path(), &radio).await;
    let r = node.apply(fm_plan(None, &[])).await.unwrap();
    let v = r.summary_json();
    assert_eq!(v["truncated"], true);
    assert_eq!(v["result"], "plan_start");
    assert_eq!(
        v["counts"],
        serde_json::json!({"started": 80, "existing": 0, "refused": 0})
    );
}

#[tokio::test]
async fn jobs_stop_ends_demodulators_and_their_attached_jobs_without_failures() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new(RADIO);
    let node = open_with(dir.path(), &radio, attach_registry(capacity(4))).await;
    node.apply(fm_attach((1..=8).collect())).await.unwrap();
    assert_eq!(running(&node).len(), 16);
    let r = node
        .apply(Op::JobsStop {
            source: Some(SourceId(RADIO.into())),
            kind: None,
            plan: None,
        })
        .await
        .unwrap();
    match r {
        OpResult::JobsStopped { stopped, failed } => {
            assert_eq!(stopped.len(), 16);
            assert!(failed.is_empty(), "{failed:?}");
        }
        r => panic!("jobs_stop: {r:?}"),
    }
    assert!(running(&node).is_empty());
    assert!(source(&node).await.holders.is_empty());
}
