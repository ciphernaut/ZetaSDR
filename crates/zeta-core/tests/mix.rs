//! `mix_start`: a leaseless node-wide job summing the audio of the jobs it lists. Its
//! placeholder source and band reach every reader of a record: these tests pin what each
//! makes of them, over the `Radio` of `plan_start.rs`, feeding audio straight onto a
//! demodulator's output stream (the radio delivers no blocks).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::graph::{Graph, GraphError, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, SourceInfo};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Readiness, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{
    Capabilities, Claim, Hz, JobId, Provenance, SampleFormat, SourceId, Span, StreamId, StreamKind,
    Timestamp,
};

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

/// The registry with the attached chain's stand-ins, the real `stream_in` and the dsp
/// stages (`mix` among them).
fn attach_registry() -> StageRegistry {
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
    r.register(Stand::new(
        "transcribe",
        &[StreamKind::Event],
        Readiness::always(),
    ));
    r
}

const CH: u64 = 476_625_000;
const WIDTH: u64 = 12_500;

async fn open_on(dir: &Path, radios: &[Arc<Radio>]) -> Node {
    let ports: Vec<Arc<dyn Port>> = radios.iter().map(|r| r.clone() as Arc<dyn Port>).collect();
    Node::open(cfg(dir), attach_registry(), ports)
        .await
        .unwrap()
}

fn src(id: &str) -> SourceId {
    SourceId(id.into())
}

async fn job_of(node: &Node, op: Op) -> Result<JobId, OpError> {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => Ok(id),
        Ok(r) => panic!("expected a job, got {r:?}"),
        Err(e) => Err(e),
    }
}

async fn mix_start(node: &Node, sources: Option<&[&str]>) -> Result<JobId, OpError> {
    let sources = sources.map(|s| s.iter().map(|id| src(id)).collect());
    job_of(node, Op::MixStart { sources }).await
}

async fn mix(node: &Node, sources: Option<&[&str]>) -> JobId {
    mix_start(node, sources).await.unwrap()
}

fn start_op(kind: JobKind, source: &str, center_hz: u64) -> Op {
    Op::JobStart {
        kind,
        source: src(source),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(WIDTH),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        elastic: false,
        replace: vec![],
    }
}

async fn demod(node: &Node, source: &str, center_hz: u64) -> JobId {
    job_of(node, start_op(JobKind::FmDemod, source, center_hz))
        .await
        .unwrap()
}

async fn attach(node: &Node, input: &JobId) -> JobId {
    let op = Op::JobAttach {
        kind: JobKind::Transcribe,
        job: input.clone(),
        language: None,
    };
    job_of(node, op).await.unwrap()
}

async fn retune(node: &Node, job: &JobId, center_hz: u64) -> Result<JobId, OpError> {
    let op = Op::JobRetune {
        job: job.clone(),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(WIDTH),
        bin_hz: None,
        replace: vec![],
    };
    job_of(node, op).await
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .unwrap_or_else(|| panic!("no record of {}", id.0))
}

fn running(node: &Node, id: &JobId) -> bool {
    record(node, id).state == JobState::Running
}

/// `graph_put` of the mix's own graph with `muted` set.
async fn put_muted(node: &Node, mix: &JobId, muted: Value) -> Result<OpResult, OpError> {
    let mut graph = record(node, mix).graph;
    graph.nodes[0].params["muted"] = muted;
    let op = Op::GraphPut {
        job: mix.clone(),
        graph,
    };
    node.apply(op).await
}

async fn source_info(node: &Node, id: &str) -> SourceInfo {
    match node.apply(Op::SourceList {}).await {
        Ok(OpResult::Sources(v)) => v.into_iter().find(|s| s.id.0 == id).unwrap(),
        r => panic!("source_list: {r:?}"),
    }
}

/// `job_context.mix` of `mix`.
fn mix_ctx(node: &Node, mix: &JobId) -> Value {
    node.job_context(mix).unwrap()["mix"].clone()
}

/// Poll `f` every 20 ms for up to 2 s, panicking with `what` when it never holds.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn until_inputs(node: &Node, mix: &JobId, n: u64) {
    until(&format!("mix.inputs == {n}"), || {
        mix_ctx(node, mix)["inputs"] == n
    })
    .await;
}

/// Publish 20 ms of audio on `job`'s audio output, as its demodulator would.
fn feed(node: &Node, job: &JobId) {
    let stream = node.job_audio_stream(job).unwrap();
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let t = Timestamp(t.as_nanos() as u64);
    let prov = Provenance {
        source: record(node, job).source,
        job: Some(job.clone()),
        channel: None,
        span: Span {
            start_ns: t,
            end_ns: t,
        },
    };
    let payload = Payload::Audio {
        sample_rate_hz: Hz(48_000),
        channels: 1,
        samples: Arc::from(vec![0.1f32; 960]),
    };
    node.bus().publish(Frame::new(stream, prov, payload));
}

/// Whether audio fed to `demod` comes out of `mix` within 2 s. Feeding goes on throughout,
/// as a just-started job joins only at the mix's next poll; what an earlier feed left in
/// the queues (200 ms at most) plays out first.
async fn hears(node: &Node, mix: &JobId, demod: &JobId) -> bool {
    tokio::time::sleep(Duration::from_millis(300)).await;
    let out = node.job_audio_stream(mix).unwrap();
    let mut rx = node.bus().subscribe(&out).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        feed(node, demod);
        tokio::time::sleep(Duration::from_millis(20)).await;
        if rx.try_recv().is_ok() {
            return true;
        }
    }
    false
}

fn other_text(r: Result<JobId, OpError>) -> String {
    match r {
        Err(OpError::Other { message }) => message,
        r => panic!("expected an other error, got {r:?}"),
    }
}

#[tokio::test]
async fn mix_start_takes_no_lease_and_a_rigid_tune_is_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new("a");
    let node = open_on(dir.path(), std::slice::from_ref(&radio)).await;
    let m = mix(&node, None).await;

    let rec = record(&node, &m);
    assert_eq!(rec.kind, "mix");
    assert_eq!(rec.source.0, "node");
    assert_eq!(rec.tuning.center_hz, Hz(0));
    assert_eq!(rec.tuning.bandwidth_hz, Hz(0));
    assert_eq!(rec.input, None);
    assert!(running(&node, &m));

    let info = source_info(&node, "a").await;
    assert!(info.holders.is_empty(), "{:?}", info.holders);
    assert!(info.elastic.is_empty(), "{:?}", info.elastic);
    assert_eq!(radio.tunes(), 0);

    let free = Op::SourceFree {
        center_hz: Hz(CH),
        bandwidth_hz: Hz(WIDTH),
    };
    match node.apply(free).await {
        Ok(OpResult::SourceFree { free, busy }) => {
            assert_eq!(free, vec![src("a")]);
            assert!(busy.is_empty());
        }
        r => panic!("source_free: {r:?}"),
    }

    demod(&node, "a", CH).await;
    assert!(radio.tunes() > 0);
    assert_eq!(radio.current(), Some(band(CH, WIDTH)));
}

#[tokio::test]
async fn the_same_set_answers_the_same_id_and_another_set_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let radios = [Radio::new("a"), Radio::new("b")];
    let node = open_on(dir.path(), &radios).await;

    let all = mix(&node, None).await;
    assert_eq!(mix(&node, None).await, all);
    let both = mix(&node, Some(&["a", "b"])).await;
    assert_ne!(both, all, "a list naming every source is another mix");
    assert_eq!(mix(&node, Some(&["b", "a", "b"])).await, both);
    let one = mix(&node, Some(&["a"])).await;
    assert_ne!(one, both);
    assert_ne!(one, all);

    assert!(matches!(
        mix_start(&node, Some(&["nope"])).await,
        Err(OpError::Unknown { .. })
    ));
    assert_eq!(
        other_text(mix_start(&node, Some(&[])).await),
        "sources: empty: pass null for every source"
    );

    put_muted(&node, &one, json!([{"source": "a", "center_hz": CH}]))
        .await
        .unwrap();
    assert_eq!(mix(&node, Some(&["a"])).await, one, "a mute changes no set");

    node.apply(Op::JobStop { job: one.clone() }).await.unwrap();
    let again = mix(&node, Some(&["a"])).await;
    assert_ne!(again, one, "a stopped mix is history");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_mix_starts_answer_one_job() {
    const TASKS: usize = 8;
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(open_on(dir.path(), &[Radio::new("a")]).await);
    for round in 0..30 {
        let barrier = Arc::new(tokio::sync::Barrier::new(TASKS));
        let starts: Vec<_> = (0..TASKS)
            .map(|_| {
                let (node, barrier) = (node.clone(), barrier.clone());
                tokio::spawn(async move {
                    barrier.wait().await;
                    mix(&node, None).await
                })
            })
            .collect();
        let mut ids = Vec::new();
        for start in starts {
            ids.push(start.await.unwrap());
        }
        ids.dedup();
        assert_eq!(ids.len(), 1, "round {round}: {ids:?}");
        let job = ids.remove(0);
        let live = |r: &JobRecord| r.kind == "mix" && r.state == JobState::Running;
        let mixes = node.job_list().iter().filter(|r| live(r)).count();
        assert_eq!(mixes, 1, "round {round}");
        node.apply(Op::JobStop { job }).await.unwrap();
    }
}

#[tokio::test]
async fn jobs_on_both_radios_are_inputs_with_null_and_only_the_named_with_a_list() {
    let dir = tempfile::tempdir().unwrap();
    let radios = [Radio::new("a"), Radio::new("b")];
    let node = open_on(dir.path(), &radios).await;
    let (on_a, on_b) = (demod(&node, "a", CH).await, demod(&node, "b", CH).await);

    let all = mix(&node, None).await;
    until_inputs(&node, &all, 2).await;
    assert!(hears(&node, &all, &on_a).await);
    assert!(hears(&node, &all, &on_b).await);

    let named = mix(&node, Some(&["a"])).await;
    until_inputs(&node, &named, 1).await;
    assert!(hears(&node, &named, &on_a).await);
    assert!(!hears(&node, &named, &on_b).await);
}

#[tokio::test]
async fn a_mute_outlives_the_job_and_a_job_retuned_off_it_is_heard() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_on(dir.path(), &[Radio::new("a")]).await;
    let ch = demod(&node, "a", CH).await;
    let m = mix(&node, None).await;
    assert!(hears(&node, &m, &ch).await);

    put_muted(&node, &m, json!([{"source": "a", "center_hz": CH}]))
        .await
        .unwrap();
    assert_eq!(mix(&node, None).await, m);
    assert!(!hears(&node, &m, &ch).await);

    node.apply(Op::JobStop { job: ch.clone() }).await.unwrap();
    let ch = demod(&node, "a", CH).await;
    assert!(!hears(&node, &m, &ch).await);

    retune(&node, &ch, CH + 1000).await.unwrap();
    assert!(!hears(&node, &m, &ch).await);

    retune(&node, &ch, CH + 25_000).await.unwrap();
    assert!(hears(&node, &m, &ch).await);
    assert_eq!(mix_ctx(&node, &m)["muted"], json!([]));
}

#[tokio::test]
async fn a_bad_muted_is_refused_and_the_mix_keeps_running() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_on(dir.path(), &[Radio::new("a")]).await;
    let m = mix(&node, None).await;
    let streams = node.job_outputs(&m);
    let r = put_muted(&node, &m, json!([{"source": "a"}])).await;
    assert!(
        matches!(
            &r,
            Err(OpError::Graph {
                detail: GraphError::BadParams { .. }
            })
        ),
        "{r:?}"
    );
    assert!(running(&node, &m));
    assert_eq!(node.job_outputs(&m), streams);
}

#[tokio::test]
async fn attached_and_mix_jobs_are_not_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_on(dir.path(), &[Radio::new("a")]).await;
    let ch = demod(&node, "a", CH).await;
    attach(&node, &ch).await;
    job_of(&node, start_op(JobKind::Spectrum, "a", CH))
        .await
        .unwrap();
    let other = mix(&node, Some(&["a"])).await;
    let m = mix(&node, None).await;
    assert_ne!(other, m);
    until_inputs(&node, &m, 1).await;
    // Another poll or two: nothing else joins.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(mix_ctx(&node, &m)["inputs"], 1);
}

#[tokio::test]
async fn job_start_plan_start_and_job_retune_refuse_a_mix() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new("a");
    let node = open_on(dir.path(), std::slice::from_ref(&radio)).await;

    assert_eq!(
        other_text(job_of(&node, start_op(JobKind::Mix, "a", CH)).await),
        "mix takes no band: use mix_start"
    );
    let plan = Op::PlanStart {
        plan: "uhf_cb_au".into(),
        channels: Some(vec![1]),
        kind: JobKind::Mix,
        source: src("a"),
        deviation_hz: None,
        sideband: None,
        attach: None,
        language: None,
        replace: vec![],
    };
    match node.apply(plan).await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "mix cannot be started per channel")
        }
        r => panic!("plan_start: {r:?}"),
    }

    let m = mix(&node, None).await;
    assert_eq!(
        other_text(retune(&node, &m, CH).await),
        "mix is not retunable"
    );
    put_muted(&node, &m, json!([{"source": "a", "center_hz": CH}]))
        .await
        .unwrap();
    assert_eq!(
        other_text(retune(&node, &m, CH).await),
        "mix is not retunable",
        "a mute's graph_put marks the record an operator's graph; the kind still refuses"
    );
    assert_eq!(radio.tunes(), 0);
}

#[tokio::test]
async fn a_transcribe_attaches_follows_a_mute_and_ends_with_the_mix() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_on(dir.path(), &[Radio::new("a")]).await;
    let m = mix(&node, None).await;
    let t = attach(&node, &m).await;
    let rec = record(&node, &t);
    assert_eq!(rec.source.0, "node");
    assert_eq!(rec.input.as_ref(), Some(&m));

    let before = node.job_audio_stream(&m).unwrap();
    put_muted(&node, &m, json!([{"source": "a", "center_hz": CH}]))
        .await
        .unwrap();
    let after = node.job_audio_stream(&m).unwrap();
    assert_ne!(after, before);
    until("the attached job reads the mix's new stream", || {
        running(&node, &t)
            && record(&node, &t)
                .graph
                .nodes
                .iter()
                .any(|n| n.kind == "stream_in" && n.params["stream"] == after.0.as_str())
    })
    .await;

    node.apply(Op::JobStop { job: m.clone() }).await.unwrap();
    until("the attached job ends with the mix", || {
        record(&node, &t).state == JobState::Stopped
    })
    .await;
}

#[tokio::test]
async fn jobs_stop_stops_it_only_without_a_source_or_plan_filter() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_on(dir.path(), &[Radio::new("a")]).await;
    let ch = demod(&node, "a", CH).await;
    let m = mix(&node, None).await;

    let stop = |source: Option<&str>, kind: Option<JobKind>, plan: Option<&str>| Op::JobsStop {
        source: source.map(src),
        kind,
        plan: plan.map(String::from),
    };
    match node.apply(stop(Some("a"), None, None)).await {
        Ok(OpResult::JobsStopped { stopped, failed }) => {
            assert_eq!(stopped, vec![ch]);
            assert!(failed.is_empty());
        }
        r => panic!("jobs_stop source: {r:?}"),
    }
    assert!(running(&node, &m));
    node.apply(stop(None, None, Some("uhf_cb_au")))
        .await
        .unwrap();
    assert!(running(&node, &m), "a plan filter never matches a mix");
    assert!(matches!(
        node.apply(stop(Some("node"), None, None)).await,
        Err(OpError::Unknown { .. })
    ));
    assert!(running(&node, &m));

    node.apply(stop(None, Some(JobKind::Mix), None))
        .await
        .unwrap();
    assert_eq!(record(&node, &m).state, JobState::Stopped);

    let m = mix(&node, None).await;
    match node.apply(stop(None, None, None)).await {
        Ok(OpResult::JobsStopped { stopped, .. }) => assert_eq!(stopped, vec![m]),
        r => panic!("jobs_stop: {r:?}"),
    }
}

#[tokio::test]
async fn a_restart_resumes_the_mix_after_its_jobs_and_before_its_transcribe() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new("a");
    let node = open_on(dir.path(), std::slice::from_ref(&radio)).await;
    // The order of creation that would sort worst.
    let m = mix(&node, None).await;
    let ch = demod(&node, "a", CH).await;
    let t = attach(&node, &m).await;
    drop(node);

    let node = open_on(dir.path(), std::slice::from_ref(&radio)).await;
    for id in [&m, &ch, &t] {
        assert_eq!(
            record(&node, id).state,
            JobState::Running,
            "{}",
            record(&node, id).kind
        );
    }
    assert_eq!(radio.current(), Some(band(CH, WIDTH)));
    assert_eq!(source_info(&node, "a").await.holders, vec![ch.clone()]);
    until_inputs(&node, &m, 1).await;
    assert!(hears(&node, &m, &ch).await);
}

#[tokio::test]
async fn a_restart_keeps_the_mute_in_the_mix() {
    let dir = tempfile::tempdir().unwrap();
    let radio = Radio::new("a");
    let node = open_on(dir.path(), std::slice::from_ref(&radio)).await;
    let m = mix(&node, None).await;
    let ch = demod(&node, "a", CH).await;
    put_muted(&node, &m, json!([{"source": "a", "center_hz": CH}]))
        .await
        .unwrap();
    drop(node);

    let node = open_on(dir.path(), std::slice::from_ref(&radio)).await;
    assert_eq!(record(&node, &m).state, JobState::Running);
    until_inputs(&node, &m, 1).await;
    assert!(!hears(&node, &m, &ch).await);
    assert_eq!(
        mix_ctx(&node, &m)["muted"],
        json!([{"job": ch.0, "source": "a", "center_hz": CH}])
    );
}

/// A `graph_start` of a graph whose root is a `mix` node: a mix by another name.
async fn graph_mix(node: &Node) -> JobId {
    let graph = serde_json::from_value(json!({
        "id": "g",
        "nodes": [{"id": "mix", "kind": "mix", "params": {"sources": null}}],
        "edges": [],
    }))
    .unwrap();
    job_of(node, Op::GraphStart { graph }).await.unwrap()
}

#[tokio::test]
async fn a_mix_started_by_graph_start_is_not_mixed() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_on(dir.path(), &[Radio::new("a")]).await;
    let ch = demod(&node, "a", CH).await;
    let m = mix(&node, None).await;
    let (g1, g2) = (graph_mix(&node).await, graph_mix(&node).await);
    assert_eq!(record(&node, &g1).kind, "graph");
    // Each reads the demodulator only: neither the mix nor the other graph mix.
    for id in [&m, &g1, &g2] {
        until_inputs(&node, id, 1).await;
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    for id in [&m, &g1, &g2] {
        assert_eq!(mix_ctx(&node, id)["inputs"], 1, "{id:?}");
    }
    assert!(hears(&node, &m, &ch).await);
}

/// A demodulator stand-in with audio-kind outputs carrying `roles`.
struct RoleOut {
    spec: StageSpec,
    roles: Vec<Option<&'static str>>,
}

#[async_trait]
impl Stage for RoleOut {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    fn output_roles(&self, _params: &Value) -> Vec<Option<&'static str>> {
        self.roles.clone()
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

fn role_out(kind: &'static str, roles: Vec<Option<&'static str>>) -> Arc<RoleOut> {
    Arc::new(RoleOut {
        spec: StageSpec {
            kind,
            inputs: vec![StreamKind::Iq],
            outputs: vec![StreamKind::Audio; roles.len()],
            params_schema: schemars::json_schema!({"type": "object"}),
        },
        roles,
    })
}

async fn role_job(node: &Node, kind: &str, center_hz: u64) -> JobId {
    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("n".into()),
            kind: kind.into(),
            params: json!({}),
        }],
        edges: vec![],
    };
    node.job_start(kind, src("a"), band(center_hz, WIDTH), graph, None, false)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_mix_never_sums_a_composite() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = attach_registry();
    reg.register(role_out("comp_then_audio", vec![Some("composite"), None]));
    reg.register(role_out("comp_only", vec![Some("composite")]));
    let ports: Vec<Arc<dyn Port>> = vec![Radio::new("a")];
    let node = Node::open(cfg(dir.path()), reg, ports).await.unwrap();

    // Only a composite: the job is no input of the mix.
    let only = role_job(&node, "comp_only", CH).await;
    let m = mix(&node, None).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(mix_ctx(&node, &m)["inputs"], 0);
    node.job_stop(&only).await.unwrap();

    // A composite then audio: the mix's input is the second, role-less stream.
    let both = role_job(&node, "comp_then_audio", CH + 100_000).await;
    until_inputs(&node, &m, 1).await;
    let outs = node.job_outputs(&both);
    assert_eq!(outs.len(), 2);
    assert_eq!(node.job_audio_stream(&both).unwrap(), outs[1].1);
    assert!(hears(&node, &m, &both).await);
}
