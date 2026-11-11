//! Attached jobs as core sees them: `job_attach`, the `stream_in` stage, the no-lease
//! launch, the end of an attached job with its input, resume order, and the launch
//! registering every output of a multi-output stage. The real `denoise`, `vad` and
//! `transcribe` stages live in other crates; inline stand-ins take their kinds here.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::graph::{Edge, Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Readiness, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus, StreamInfo};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{Hz, JobId, SourceId, StreamId, StreamKind};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// What a stand-in stage emits for one input frame: (output index, payload) pairs.
type MapFn = fn(&Frame) -> Vec<(usize, Payload)>;

/// A stage that maps every frame of its one input through `f`, keeping its provenance.
struct MapStage {
    spec: StageSpec,
    f: MapFn,
}

impl MapStage {
    fn new(kind: &'static str, input: StreamKind, outputs: &[StreamKind], f: MapFn) -> Arc<Self> {
        Arc::new(Self {
            spec: StageSpec {
                kind,
                inputs: vec![input],
                outputs: outputs.to_vec(),
                params_schema: schemars::json_schema!({"type": "object"}),
            },
            f,
        })
    }
}

#[async_trait]
impl Stage for MapStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
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
        let Some(rx) = inputs.first_mut() else {
            return Err(StageError::Failed("expected 1 input".into()));
        };
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        for (i, payload) in (self.f)(&frame) {
                            if let Some(out) = outputs.get(i) {
                                bus.publish(Frame::new(out.clone(), frame.provenance.clone(), payload));
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

/// A stand-in demodulator whose audio outputs carry roles: `roles` lines up with `outputs`.
struct RoleStage {
    spec: StageSpec,
    roles: Vec<Option<&'static str>>,
}

#[async_trait]
impl Stage for RoleStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    fn output_roles(&self, _params: &serde_json::Value) -> Vec<Option<&'static str>> {
        self.roles.clone()
    }
    async fn run(
        &self,
        _params: serde_json::Value,
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

fn role_stage(kind: &'static str, roles: Vec<Option<&'static str>>) -> Arc<RoleStage> {
    Arc::new(RoleStage {
        spec: StageSpec {
            kind,
            inputs: vec![StreamKind::Iq],
            outputs: vec![StreamKind::Audio; roles.len()],
            params_schema: schemars::json_schema!({"type": "object"}),
        },
        roles,
    })
}

fn audio() -> Payload {
    Payload::Audio {
        sample_rate_hz: Hz(48_000),
        channels: 1,
        samples: Arc::from(vec![0.1f32; 480]),
    }
}

fn same(f: &Frame) -> Vec<(usize, Payload)> {
    vec![(0, f.payload.clone())]
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(StreamInStage::new()));
    // A demodulator stand-in: one audio frame per IQ block.
    r.register(MapStage::new(
        "audio_src",
        StreamKind::Iq,
        &[StreamKind::Audio],
        |_| vec![(0, audio())],
    ));
    // The attached chain's stand-ins, under the kinds `expand_attached` names.
    r.register(MapStage::new(
        "denoise",
        StreamKind::Audio,
        &[StreamKind::Audio],
        same,
    ));
    r.register(MapStage::new(
        "vad",
        StreamKind::Audio,
        &[StreamKind::Audio, StreamKind::Event],
        |f| {
            vec![
                (0, f.payload.clone()),
                (
                    1,
                    Payload::Event(serde_json::json!({"protocol": "vad", "speech": true})),
                ),
            ]
        },
    ));
    r.register(MapStage::new(
        "transcribe",
        StreamKind::Audio,
        &[StreamKind::Event],
        |_| {
            vec![(
                0,
                Payload::Event(serde_json::json!({"protocol": "transcript", "text": "stand-in"})),
            )]
        },
    ));
    // Two outputs, audio then event, for the launch's output registration.
    r.register(MapStage::new(
        "two_out",
        StreamKind::Iq,
        &[StreamKind::Audio, StreamKind::Event],
        |_| {
            vec![
                (0, audio()),
                (1, Payload::Event(serde_json::json!({"protocol": "two"}))),
            ]
        },
    ));
    r
}

fn port(dir: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    }
}

fn tuning() -> Tuning {
    Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(48_000),
    }
}

fn node_graph(kind: &str, params: serde_json::Value) -> Graph {
    Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("n".into()),
            kind: kind.into(),
            params,
        }],
        edges: vec![],
    }
}

fn file0() -> SourceId {
    SourceId("file0".into())
}

/// A running radio job producing one audio stream.
async fn start_audio(node: &Node) -> JobId {
    node.job_start(
        "audio",
        file0(),
        tuning(),
        node_graph("audio_src", serde_json::json!({})),
        None,
        false,
    )
    .await
    .unwrap()
}

async fn attach(node: &Node, job: &JobId) -> Result<OpResult, OpError> {
    node.apply(Op::JobAttach {
        kind: JobKind::Transcribe,
        job: job.clone(),
        language: None,
    })
    .await
}

async fn attach_ok(node: &Node, job: &JobId) -> JobId {
    match attach(node, job).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("attach: {other:?}"),
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

fn streams_of(node: &Node, id: &JobId) -> Vec<StreamInfo> {
    node.bus()
        .list()
        .into_iter()
        .filter(|s| s.provenance.job.as_ref() == Some(id))
        .collect()
}

/// The first event with `protocol` on any of `job`'s event streams within `ms`.
async fn event(node: &Node, job: &JobId, protocol: &str, ms: u64) -> Option<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    let mut rxs: Vec<_> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        if rxs.is_empty() {
            rxs = streams_of(node, job)
                .iter()
                .filter(|s| s.kind == StreamKind::Event)
                .filter_map(|s| node.bus().subscribe(&s.id))
                .collect();
        }
        for rx in &mut rxs {
            while let Ok(f) = rx.try_recv() {
                if let Payload::Event(v) = f.payload
                    && v["protocol"] == protocol
                {
                    return Some(v);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

async fn wait_not_running(node: &Node, id: &JobId, ms: u64) -> JobState {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        let state = record(node, id).state;
        if state != JobState::Running || tokio::time::Instant::now() >= deadline {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn other(message: &str) -> OpError {
    OpError::Other {
        message: message.into(),
    }
}

#[tokio::test]
async fn attach_runs_without_a_lease_and_records_its_input() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;

    let rec = record(&node, &id);
    assert_eq!(rec.kind, "transcribe");
    assert_eq!(rec.input, Some(input.clone()));
    assert_eq!(rec.state, JobState::Running);
    let inrec = record(&node, &input);
    assert_eq!(
        (rec.source.clone(), rec.tuning),
        (inrec.source, inrec.tuning)
    );
    // The graph reads the input's audio stream, which `stream_in` names.
    let first = &rec.graph.nodes[0];
    assert_eq!(first.kind, "stream_in");
    assert_eq!(first.params["job"], input.0);
    assert_eq!(first.params["kind"], "audio");
    let audio = streams_of(&node, &input)
        .into_iter()
        .find(|s| s.kind == StreamKind::Audio)
        .unwrap();
    assert_eq!(first.params["stream"], audio.id.0);

    // Only the input job holds the source.
    assert_eq!(
        node.lease_holders(&file0()),
        vec![(input.clone(), tuning())]
    );
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(s) => assert_eq!(s[0].holders, vec![input.clone()]),
        r => panic!("{r:?}"),
    }
    // No IQ stream; three audio edges, `vad`'s event output and `transcribe`'s.
    let mut kinds: Vec<_> = streams_of(&node, &id).iter().map(|s| s.kind).collect();
    kinds.sort_by_key(|k| format!("{k:?}"));
    assert_eq!(
        kinds,
        [
            StreamKind::Audio,
            StreamKind::Audio,
            StreamKind::Audio,
            StreamKind::Event,
            StreamKind::Event
        ]
    );
    let t = event(&node, &id, "transcript", 2000)
        .await
        .expect("transcript");
    assert_eq!(t["text"], "stand-in");
    assert!(event(&node, &id, "vad", 2000).await.is_some(), "vad event");
    // The record on disk carries `input` too.
    let disk: JobRecord = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("state/jobs").join(format!("{}.json", id.0)))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(disk.input, Some(input));
}

#[tokio::test]
async fn attached_job_stops_when_input_stops() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
    node.job_stop(&input).await.unwrap();
    assert_eq!(wait_not_running(&node, &id, 1000).await, JobState::Stopped);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while !streams_of(&node, &id).is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(streams_of(&node, &id).is_empty(), "streams unregistered");
}

/// The attached job follows a `graph_put` restart of its input: relaunched under its
/// own id on the input's new stream, never `Stopped`.
#[tokio::test]
async fn attach_follows_graph_put_of_the_input() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;
    node.job_graph_put(&input, node_graph("audio_src", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(record(&node, &input).state, JobState::Running);
    assert_running_for(&node, &id, 2000).await;
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
    assert_eq!(attached_to(&node, &input), vec![id]);
}

/// Two `graph_put`s of the input at once (one may be refused as `already running a
/// launch`): the attached job ends up `Running` under its id on the input's stream,
/// never stopped by the other restart's relaunch.
#[tokio::test]
async fn overlapping_graph_puts_of_the_input_keep_the_attached_job() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;
    let (a, b) = tokio::join!(
        node.job_graph_put(&input, node_graph("audio_src", serde_json::json!({}))),
        node.job_graph_put(&input, node_graph("audio_src", serde_json::json!({}))),
    );
    assert!(a.is_ok() || b.is_ok(), "one restart lands: {a:?} {b:?}");
    assert_eq!(record(&node, &input).state, JobState::Running);
    assert_running_for(&node, &id, 1000).await;
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
    assert_eq!(attached_to(&node, &input), vec![id]);
}

/// The ids of the records attached to `input`.
fn attached_to(node: &Node, input: &JobId) -> Vec<JobId> {
    node.job_list()
        .into_iter()
        .filter(|r| r.input.as_ref() == Some(input))
        .map(|r| r.id)
        .collect()
}

/// `id` stays `Running` throughout `ms` (it is relaunched, never ended).
async fn assert_running_for(node: &Node, id: &JobId, ms: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < deadline {
        let state = record(node, id).state;
        assert_eq!(state, JobState::Running, "attached job {}", id.0);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// [`registry`] plus the real DSP stages (the stand-ins registered first win their kinds).
fn registry_with_dsp() -> StageRegistry {
    let mut r = registry();
    zeta_dsp::register_all(&mut r);
    r
}

/// An `fm_demod` on the tone, whose squelch opens, feeding the attached chain.
async fn start_fm(node: &Node) -> JobId {
    let op = Op::JobStart {
        kind: JobKind::FmDemod,
        source: file0(),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("fm_demod start: {other:?}"),
    }
}

async fn retune_input(node: &Node, input: &JobId) {
    let op = Op::JobRetune {
        job: input.clone(),
        center_hz: Hz(100_002_000),
        bandwidth_hz: Hz(10_000),
        bin_hz: None,
        replace: vec![],
    };
    let r = tokio::time::timeout(Duration::from_secs(10), node.apply(op))
        .await
        .expect("no deadlock");
    assert_eq!(r, Ok(OpResult::Job(input.clone())));
}

#[tokio::test]
async fn an_attached_transcribe_survives_its_inputs_retune_under_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry_with_dsp(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_fm(&node).await;
    let att = attach_ok(&node, &input).await;
    assert!(event(&node, &att, "transcript", 3000).await.is_some());
    retune_input(&node, &input).await;
    assert_eq!(
        record(&node, &input).tuning,
        Tuning {
            center_hz: Hz(100_002_000),
            bandwidth_hz: Hz(10_000),
        }
    );
    assert_running_for(&node, &att, 2000).await;
    assert_eq!(record(&node, &att).input, Some(input.clone()));
    assert!(event(&node, &att, "transcript", 3000).await.is_some());
    assert_eq!(attached_to(&node, &input), vec![att]);
}

#[tokio::test]
async fn an_attached_job_the_operator_stopped_stays_stopped_through_a_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry_with_dsp(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_fm(&node).await;
    let att = attach_ok(&node, &input).await;
    node.job_stop(&att).await.unwrap();
    assert_eq!(record(&node, &att).state, JobState::Stopped);
    retune_input(&node, &input).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(record(&node, &att).state, JobState::Stopped);
    assert_eq!(attached_to(&node, &input), vec![att]);
}

#[tokio::test]
async fn attach_refuses_non_running_or_audio_less_input() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let stopped = start_audio(&node).await;
    node.job_stop(&stopped).await.unwrap();
    assert_eq!(
        attach(&node, &stopped).await,
        Err(other(&format!("job {} is not running", stopped.0)))
    );
    let iq = node
        .job_start(
            "monitor",
            file0(),
            tuning(),
            node_graph("passthrough", serde_json::json!({"kind": "iq"})),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        attach(&node, &iq).await,
        Err(other(&format!("job {} has no audio stream", iq.0)))
    );
    let nobody = JobId::new();
    assert!(matches!(
        attach(&node, &nobody).await,
        Err(OpError::Unknown { field, .. }) if field == "job"
    ));
    let running = start_audio(&node).await;
    // An attached job's audio runs only between its stages: it outputs none.
    let attached = attach_ok(&node, &running).await;
    assert_eq!(
        attach(&node, &attached).await,
        Err(other(&format!("job {} has no audio stream", attached.0)))
    );
    let not_attachable = node
        .apply(Op::JobAttach {
            kind: JobKind::WfmDemod,
            job: running.clone(),
            language: None,
        })
        .await;
    assert_eq!(not_attachable, Err(other("wfm_demod is not attachable")));
    for bad in ["", "e", "EN", "english", "en_US", "en-", "en-x"] {
        let r = node
            .apply(Op::JobAttach {
                kind: JobKind::Transcribe,
                job: running.clone(),
                language: Some(bad.into()),
            })
            .await;
        assert!(
            matches!(&r, Err(OpError::Other { message }) if message.contains("BCP-47")),
            "{bad:?}: {r:?}"
        );
    }
    for good in ["de", "pt-BR", "zh-Hant-TW", "yue"] {
        let r = node
            .apply(Op::JobAttach {
                kind: JobKind::Transcribe,
                job: running.clone(),
                language: Some(good.into()),
            })
            .await;
        let Ok(OpResult::Job(id)) = r else {
            panic!("{good}: {r:?}")
        };
        let tr = record(&node, &id)
            .graph
            .nodes
            .into_iter()
            .find(|n| n.kind == "transcribe")
            .unwrap();
        assert_eq!(tr.params["language"], good);
    }
    // Nothing was recorded for the refused attaches: two audio jobs, the iq job, five attached.
    assert_eq!(node.job_list().len(), 2 + 1 + 5);
}

#[tokio::test]
async fn job_start_with_transcribe_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    let r = node
        .apply(Op::JobStart {
            kind: JobKind::Transcribe,
            source: file0(),
            center_hz: Hz(100_000_000),
            bandwidth_hz: Hz(48_000),
            deviation_hz: None,
            sideband: None,
            dwell_ms: None,
            average: None,
            bits: None,
            method: None,
            bin_hz: None,
            elastic: false,
            replace: vec![],
        })
        .await;
    assert_eq!(
        r,
        Err(other("transcribe attaches to a job: use job_attach"))
    );
    assert!(node.job_list().is_empty());
}

fn on_disk(dir: &Path, id: &JobId) -> std::path::PathBuf {
    dir.join("state/jobs").join(format!("{}.json", id.0))
}

#[tokio::test]
async fn attached_job_resumes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    // The file source plays only while subscribed: keep it playing between nodes.
    let _hold = port.subscribe();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;
    let before = record(&node, &id).graph.nodes[0].params["stream"].clone();
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Both resume, the input first; the attached job reads the input's new stream.
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    assert_eq!(record(&node, &input).state, JobState::Running);
    assert_eq!(record(&node, &id).state, JobState::Running);
    let after = record(&node, &id).graph.nodes[0].params["stream"].clone();
    assert_ne!(before, after, "stream re-resolved");
    let audio = streams_of(&node, &input)
        .into_iter()
        .find(|s| s.kind == StreamKind::Audio)
        .unwrap();
    assert_eq!(after, audio.id.0);
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
    assert_eq!(node.lease_holders(&file0()).len(), 1);
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The input no longer running: the attached job fails, naming it.
    let path = on_disk(dir.path(), &input);
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let running = v["state"].clone();
    v["state"] = serde_json::json!({"status": "stopped"});
    std::fs::write(&path, v.to_string()).unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    assert_eq!(
        record(&node, &id).state,
        JobState::Failed {
            reason: format!("input job {} not running", input.0)
        }
    );
    assert!(streams_of(&node, &id).is_empty());
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // ... and resumes once the input runs again.
    v["state"] = running;
    std::fs::write(&path, v.to_string()).unwrap();
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    assert_eq!(record(&node, &id).state, JobState::Running);
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
}

/// Ruling R2: every output of a stage gets a stream, the edge's where one of that kind
/// leaves the node, else a job output stream.
#[tokio::test]
async fn launch_registers_every_output_of_a_multi_output_stage() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    // No edges: an IQ root stream plus one stream per output.
    let alone = node
        .job_start(
            "two",
            file0(),
            tuning(),
            node_graph("two_out", serde_json::json!({})),
            None,
            false,
        )
        .await
        .unwrap();
    let mut kinds: Vec<_> = streams_of(&node, &alone)
        .iter()
        .map(|s| format!("{:?}", s.kind))
        .collect();
    kinds.sort();
    assert_eq!(kinds, ["Audio", "Event", "Iq"]);
    assert!(event(&node, &alone, "two", 2000).await.is_some());
    node.job_stop(&alone).await.unwrap();

    // An audio edge to `denoise`: the edge takes the audio output, the event output gets
    // its own stream, `denoise` its audio output.
    let (a, b) = (NodeId("a".into()), NodeId("b".into()));
    let g = Graph {
        id: GraphId::new(),
        nodes: vec![
            GraphNode {
                id: a.clone(),
                kind: "two_out".into(),
                params: serde_json::json!({}),
            },
            GraphNode {
                id: b.clone(),
                kind: "denoise".into(),
                params: serde_json::json!({}),
            },
        ],
        edges: vec![Edge {
            from: a,
            to: b,
            kind: StreamKind::Audio,
        }],
    };
    let chained = node
        .job_start("two", file0(), tuning(), g, None, false)
        .await
        .unwrap();
    let mut kinds: Vec<_> = streams_of(&node, &chained)
        .iter()
        .map(|s| format!("{:?}", s.kind))
        .collect();
    kinds.sort();
    assert_eq!(kinds, ["Audio", "Audio", "Event", "Iq"]);
    assert!(event(&node, &chained, "two", 2000).await.is_some());
}

/// [`port`] behind a [`zeta_core::test_fixtures::FaultablePort`] the test can fault.
#[cfg(feature = "test-fixtures")]
fn faultable(
    dir: &Path,
) -> (
    Arc<FileSource>,
    Arc<zeta_core::test_fixtures::FaultablePort>,
) {
    let (src, _) = port(dir);
    let port = zeta_core::test_fixtures::FaultablePort::new(src.clone());
    (src, port)
}

/// An input that died of its source's fault fails the attached job as an input that is
/// not running, the reason a resume reads as "relaunch me once the input runs again", so
/// the node's recovery of the source brings the transcription back with the demodulator.
#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn an_attached_job_whose_input_died_of_a_source_fault_fails_input_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = faultable(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
    port.set_fault(Some("device unplugged".into()));
    assert_eq!(
        wait_not_running(&node, &input, 2000).await,
        JobState::Failed {
            reason: "source file0: device unplugged".into()
        }
    );
    assert_eq!(
        wait_not_running(&node, &id, 2000).await,
        JobState::Failed {
            reason: format!("input job {} not running", input.0)
        }
    );
}

/// An input stopped on purpose (the port healthy) leaves the attached job `Stopped`.
#[cfg(feature = "test-fixtures")]
#[tokio::test]
async fn an_attached_job_whose_input_was_stopped_stays_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = faultable(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
    node.job_stop(&input).await.unwrap();
    assert_eq!(
        wait_not_running(&node, &input, 2000).await,
        JobState::Stopped
    );
    assert_eq!(wait_not_running(&node, &id, 2000).await, JobState::Stopped);
}

/// A transcribe attached to a demodulator that denoises its own audio (`denoise: true`
/// put on its `nbfm_demod` node) runs without a `denoise` stage of its own; the put keeps
/// the input the node's to re-expand.
#[tokio::test]
async fn an_attached_transcribe_skips_denoise_when_its_input_denoises() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry_with_dsp(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_fm(&node).await;
    let mut g = record(&node, &input).graph;
    for n in g.nodes.iter_mut().filter(|n| n.kind == "nbfm_demod") {
        n.params["denoise"] = true.into();
    }
    node.job_graph_put(&input, g).await.unwrap();
    assert_running_for(&node, &input, 300).await;
    assert!(
        !record(&node, &input).operator_graph,
        "a denoise toggle keeps the node's graph"
    );
    let t = attach_ok(&node, &input).await;
    let kinds: Vec<String> = record(&node, &t)
        .graph
        .nodes
        .iter()
        .map(|n| n.kind.clone())
        .collect();
    assert_eq!(kinds, ["stream_in", "vad", "transcribe"]);
    // Toggled off again, a fresh attach denoises for itself.
    let mut g = record(&node, &input).graph;
    for n in g.nodes.iter_mut().filter(|n| n.kind == "nbfm_demod") {
        n.params["denoise"] = false.into();
    }
    node.job_graph_put(&input, g).await.unwrap();
    assert_running_for(&node, &input, 300).await;
    let t2 = attach_ok(&node, &input).await;
    let kinds: Vec<String> = record(&node, &t2)
        .graph
        .nodes
        .iter()
        .map(|n| n.kind.clone())
        .collect();
    assert_eq!(kinds, ["stream_in", "denoise", "vad", "transcribe"]);
}

/// A stand-in that wraps another stage and answers [`Stage::readiness`] with a refusal.
struct Unready(Arc<MapStage>, &'static str);

#[async_trait]
impl Stage for Unready {
    fn spec(&self) -> &StageSpec {
        self.0.spec()
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    fn readiness(&self) -> Readiness {
        Readiness::unready(self.1)
    }
    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        self.0.run(params, inputs, outputs, bus, cancel, ctx).await
    }
}

#[tokio::test]
async fn an_unready_attached_stage_is_refused_at_job_attach() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(StreamInStage::new()));
    r.register(MapStage::new(
        "audio_src",
        StreamKind::Iq,
        &[StreamKind::Audio],
        |_| vec![(0, audio())],
    ));
    r.register(Arc::new(Unready(
        MapStage::new(
            "transcribe",
            StreamKind::Audio,
            &[StreamKind::Event],
            |_| vec![],
        ),
        "no backend here",
    )));
    let node = Node::open(cfg(dir.path()), r, vec![port]).await.unwrap();
    src.start();
    let input = start_audio(&node).await;
    let before = node.job_list().len();
    match attach(&node, &input).await {
        Err(OpError::Other { message }) => assert_eq!(message, "transcribe: no backend here"),
        other => panic!("attach: {other:?}"),
    }
    assert_eq!(node.job_list().len(), before, "no job record");
    assert_eq!(
        node.stage_readiness("transcribe"),
        Some(Readiness::unready("no backend here"))
    );
    assert_eq!(node.stage_readiness("nope"), None);
}

#[test]
fn registry_describe_lists_readiness() {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    let d = r.describe();
    assert_eq!(
        d[0]["readiness"],
        serde_json::json!({"ready": true, "reason": null, "capacity": null})
    );
}

#[tokio::test]
async fn a_composite_output_is_never_the_jobs_audio() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let mut reg = registry();
    reg.register(role_stage("comp_then_audio", vec![Some("composite"), None]));
    reg.register(role_stage("comp_only", vec![Some("composite")]));
    let node = Node::open(cfg(dir.path()), reg, vec![port]).await.unwrap();
    src.start();

    let both = node
        .job_start(
            "both",
            file0(),
            tuning(),
            node_graph("comp_then_audio", serde_json::json!({})),
            None,
            false,
        )
        .await
        .unwrap();
    let audios: Vec<StreamInfo> = node
        .job_outputs(&both)
        .into_iter()
        .map(|(_, id)| node.bus().list().into_iter().find(|s| s.id == id).unwrap())
        .collect();
    assert_eq!(audios.len(), 2);
    assert_eq!(audios[0].role.as_deref(), Some("composite"));
    assert_eq!(audios[1].role, None);
    assert_eq!(node.job_audio_stream(&both).unwrap(), audios[1].id);
    let attached = attach_ok(&node, &both).await;
    let first = &record(&node, &attached).graph.nodes[0];
    assert_eq!(first.params["stream"], audios[1].id.0);

    let only = node
        .job_start(
            "only",
            file0(),
            tuning(),
            node_graph("comp_only", serde_json::json!({})),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        attach(&node, &only).await,
        Err(other(&format!("job {} has no audio stream", only.0)))
    );
    assert!(matches!(
        node.job_audio_stream(&only),
        Err(zeta_core::node::NodeError::NoAudioStream(_))
    ));
}
