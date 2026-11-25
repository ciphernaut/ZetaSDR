//! Attached jobs as core sees them: `job_attach`, the `stream_in` stage, the no-lease
//! launch, the end of an attached job with its input, resume order, and the launch
//! registering every output of a multi-output stage, and `job_attach` of `packets` with
//! its refusals. The real `denoise`, `vad` and `transcribe` stages live in other crates;
//! inline stand-ins take their kinds here.

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

mod common;
use common::{MapStage, audio, same};

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
    common::cfg(dir, true)
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
        stream: None,
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

/// A stopped input an attached record names is kept when a re-run of it (the same kind
/// on the same source and band) stops: the re-run takes a slot of its own.
#[tokio::test]
async fn a_rerun_never_replaces_an_input_an_attached_record_names() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;
    node.job_stop(&input).await.unwrap();
    assert_eq!(wait_not_running(&node, &id, 1000).await, JobState::Stopped);
    let rerun = start_audio(&node).await;
    node.job_stop(&rerun).await.unwrap();
    let mut ids: Vec<_> = node.job_list().into_iter().map(|r| r.id).collect();
    ids.sort();
    let mut want = vec![input.clone(), id.clone(), rerun];
    want.sort();
    assert_eq!(ids, want);
    assert_eq!(record(&node, &id).input, Some(input));
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

/// Two `graph_put`s of the input with different graphs at once take turns: the second,
/// arriving while the first's restart has the record briefly `stopped`, still restarts
/// the job, so the graph that runs at the end is the one stored last, and the attached
/// job never leaves `Running`.
#[tokio::test]
async fn overlapping_graph_puts_of_the_input_run_the_last_graph_stored() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_audio(&node).await;
    let id = attach_ok(&node, &input).await;
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
    let done = std::sync::atomic::AtomicUsize::new(0);
    let landed = || done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut seen = Vec::new();
    let watch = async {
        while done.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            let state = record(&node, &id).state;
            if state != JobState::Running {
                seen.push(state);
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    };
    // The second graph's `two_out` publishes `two` events: they show which graph runs.
    let (a, b, ()) = tokio::join!(
        async {
            let r = node
                .job_graph_put(&input, node_graph("audio_src", serde_json::json!({})))
                .await;
            landed();
            r
        },
        async {
            let r = node
                .job_graph_put(&input, node_graph("two_out", serde_json::json!({})))
                .await;
            landed();
            r
        },
        watch,
    );
    assert!(a.is_ok() && b.is_ok(), "both land: {a:?} {b:?}");
    assert!(seen.is_empty(), "attached job seen {seen:?}");
    assert_eq!(record(&node, &input).graph.nodes[0].kind, "two_out");
    assert_eq!(record(&node, &input).state, JobState::Running);
    assert!(
        event(&node, &input, "two", 2000).await.is_some(),
        "the stored graph runs"
    );
    assert_running_for(&node, &id, 500).await;
    assert!(event(&node, &id, "transcript", 2000).await.is_some());
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
        audio_policy: None,
        dwell_scale: None,
        elastic: false,
        replace: vec![],
        handler: None,
        needs: vec![],
        beside: None,
        choose: None,
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
    // Its record's copy of the input's tuning follows the retune, on disk too.
    let retuned = record(&node, &input).tuning;
    assert_eq!(record(&node, &att).tuning, retuned);
    let path = dir.path().join(format!("state/jobs/{}.json", att.0));
    let on_disk: JobRecord = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(on_disk.tuning, retuned);
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

/// A `job_retune` and a `graph_put` of one input at once take turns (the input's restart
/// lock): its attached job is never seen `stopped` or `failed` between them, both land,
/// and it ends `Running` on the input's last tuning.
#[tokio::test]
async fn a_retune_and_a_graph_put_of_one_input_at_once_keep_its_attached_job_running() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry_with_dsp(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = start_fm(&node).await;
    let att = attach_ok(&node, &input).await;
    assert!(event(&node, &att, "transcript", 3000).await.is_some());
    let graph = record(&node, &input).graph;
    let retune = Op::JobRetune {
        job: input.clone(),
        center_hz: Hz(100_002_000),
        bandwidth_hz: Hz(10_000),
        bin_hz: None,
        replace: vec![],
    };
    let done = std::sync::atomic::AtomicUsize::new(0);
    let landed = || done.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut seen = Vec::new();
    let watch = async {
        while done.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            let state = record(&node, &att).state;
            if state != JobState::Running {
                seen.push(state);
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    };
    let (a, b, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(
            async {
                let r = node.apply(retune).await;
                landed();
                r
            },
            async {
                let r = node.job_graph_put(&input, graph).await;
                landed();
                r
            },
            watch,
        )
    })
    .await
    .expect("no deadlock");
    assert!(seen.is_empty(), "attached job seen {seen:?}");
    assert_eq!(a, Ok(OpResult::Job(input.clone())));
    assert!(b.is_ok(), "graph_put: {b:?}");
    let retuned = Tuning {
        center_hz: Hz(100_002_000),
        bandwidth_hz: Hz(10_000),
    };
    assert_eq!(record(&node, &input).tuning, retuned);
    assert_eq!(record(&node, &input).state, JobState::Running);
    assert_running_for(&node, &att, 500).await;
    assert_eq!(record(&node, &att).tuning, retuned);
    assert!(event(&node, &att, "transcript", 3000).await.is_some());
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
            stream: None,
        })
        .await;
    assert_eq!(not_attachable, Err(other("wfm_demod is not attachable")));
    for bad in ["", "e", "EN", "english", "en_US", "en-", "en-x"] {
        let r = node
            .apply(Op::JobAttach {
                kind: JobKind::Transcribe,
                job: running.clone(),
                language: Some(bad.into()),
                stream: None,
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
                stream: None,
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
            audio_policy: None,
            dwell_scale: None,
            elastic: false,
            replace: vec![],
            handler: None,
            needs: vec![],
            beside: None,
            choose: None,
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

/// [`registry`] with a stand-in decoder publishing one AX.25 packet per IQ block, and
/// the real `packet_capture`.
fn registry_with_packets() -> StageRegistry {
    let mut r = registry();
    r.register(MapStage::new(
        "packet_src",
        StreamKind::Iq,
        &[StreamKind::Packet],
        |_| {
            vec![(
                0,
                Payload::Packet {
                    link_type: zeta_core::stream::LinkType::Ax25Kiss,
                    data: Arc::from(vec![0u8, 0x82, 0xa0]),
                },
            )]
        },
    ));
    r.register(Arc::new(
        zeta_core::packet_capture::PacketCaptureStage::new(),
    ));
    r
}

/// A `packets` chain by hand, `stream_in` (packet) into `packet_capture`, every param
/// the launch fills stale.
fn packets_graph() -> Graph {
    let (a, b) = (NodeId("in".into()), NodeId("cap".into()));
    Graph {
        id: GraphId::new(),
        nodes: vec![
            GraphNode {
                id: a.clone(),
                kind: "stream_in".into(),
                params: serde_json::json!({"job": "stale", "kind": "packet", "stream": "stale"}),
            },
            GraphNode {
                id: b.clone(),
                kind: "packet_capture".into(),
                params: serde_json::json!({
                    "job": "stale", "center_hz": 1, "mode": "stale", "access": "restricted"
                }),
            },
        ],
        edges: vec![Edge {
            from: a,
            to: b,
            kind: StreamKind::Packet,
        }],
    }
}

#[tokio::test]
async fn a_packet_capture_names_its_input() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry_with_packets(), vec![port])
        .await
        .unwrap();
    src.start();
    let input = node
        .job_start(
            "pkt",
            file0(),
            tuning(),
            node_graph("packet_src", serde_json::json!({})),
            None,
            false,
        )
        .await
        .unwrap();
    assert!(node.job_packet_stream(&input).is_ok());
    let att = node
        .job_attach("packets", &input, None, packets_graph())
        .await
        .unwrap();
    let store = node.packets();
    common::until("a capture of the input", Duration::from_secs(10), || {
        store.list().first().is_some_and(|r| r.packets > 0)
    })
    .await;
    let r = store.list().remove(0);
    assert_eq!(r.job, input);
    assert_eq!(r.source, file0());
    assert_eq!(r.center_hz, Hz(100_000_000));
    // A kind of no mode row: no mode, no access.
    assert_eq!((r.mode, r.access), (None, None));
    assert_eq!(
        capture_params(&node, &att),
        serde_json::json!({
            "job": input.0, "center_hz": 100_000_000u64, "mode": null, "access": null
        })
    );
}

/// The params of `job`'s `packet_capture` node as its record holds them.
fn capture_params(node: &Node, job: &JobId) -> serde_json::Value {
    record(node, job)
        .graph
        .nodes
        .into_iter()
        .find(|n| n.kind == "packet_capture")
        .expect("a packet_capture node")
        .params
}

/// An op from its wire form.
fn op(v: serde_json::Value) -> Op {
    serde_json::from_value(v).expect("an op")
}

/// `job_attach` of `packets` to `job`, with `language` and `stream` as given.
async fn attach_packets_as(
    node: &Node,
    job: &JobId,
    language: Option<&str>,
    stream: Option<&str>,
) -> Result<OpResult, OpError> {
    node.apply(op(serde_json::json!({
        "op": "job_attach", "kind": "packets", "job": job.0, "language": language, "stream": stream
    })))
    .await
}

async fn attach_packets(node: &Node, job: &JobId) -> Result<OpResult, OpError> {
    attach_packets_as(node, job, None, None).await
}

async fn attach_packets_ok(node: &Node, job: &JobId) -> JobId {
    match attach_packets(node, job).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("attach packets: {r:?}"),
    }
}

/// A `job_start` of `kind` on the tone at `center_hz`, 12.5 kHz wide.
fn start_op(kind: &str, center_hz: u64) -> Op {
    Op::JobStart {
        kind: JobKind::from_name(kind).unwrap(),
        source: file0(),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy: None,
        dwell_scale: None,
        elastic: false,
        replace: vec![],
        handler: None,
        needs: vec![],
        beside: None,
        choose: None,
    }
}

async fn start_kind(node: &Node, kind: &str) -> JobId {
    match node.apply(start_op(kind, 100_000_000)).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("{kind} start: {r:?}"),
    }
}

/// The id of the restricted copy of the `aprs_decode` row [`packets_node`] loads.
const RESTRICTED_ROW: &str = "aprs_restricted";

/// A node over the tone with the real DSP stages and `packet_capture`, and a modes
/// directory holding [`RESTRICTED_ROW`]: the `aprs_decode` row as `access: restricted`.
async fn packets_node(dir: &Path) -> (Arc<FileSource>, Node) {
    let (src, port) = port(dir);
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let (_, aprs) = zeta_core::modes::ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("aprs_decode.json"))
        .unwrap();
    let mut row: serde_json::Value = serde_json::from_str(aprs).unwrap();
    row["id"] = RESTRICTED_ROW.into();
    row["access"] = "restricted".into();
    std::fs::write(
        modes.join(format!("{RESTRICTED_ROW}.json")),
        row.to_string(),
    )
    .unwrap();
    let mut registry = registry_with_dsp();
    registry.register(Arc::new(
        zeta_core::packet_capture::PacketCaptureStage::new(),
    ));
    let cfg = NodeConfig {
        modes_dir: Some(modes),
        handlers_dir: None,
        ..cfg(dir)
    };
    let node = Node::open(cfg, registry, vec![port]).await.unwrap();
    assert!(
        node.modes().get(RESTRICTED_ROW).is_some(),
        "the restricted row loads"
    );
    src.start();
    (src, node)
}

#[tokio::test]
async fn a_packets_attachment_takes_its_inputs_row_and_follows_its_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = packets_node(dir.path()).await;
    let input = start_kind(&node, "aprs_decode").await;
    let att = attach_packets_ok(&node, &input).await;
    let rec = record(&node, &att);
    assert_eq!(rec.kind, "packets");
    assert_eq!(rec.input, Some(input.clone()));
    let kinds: Vec<&str> = rec.graph.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["stream_in", "packet_capture"]);
    assert_eq!(rec.graph.nodes[0].params["kind"], "packet");
    assert_eq!(
        rec.graph.edges.iter().map(|e| e.kind).collect::<Vec<_>>(),
        [StreamKind::Packet]
    );
    assert_eq!(
        capture_params(&node, &att),
        serde_json::json!({
            "job": input.0, "center_hz": 100_000_000u64, "mode": "aprs_decode", "access": null,
            "handler": "builtin"
        })
    );
    let retune = Op::JobRetune {
        job: input.clone(),
        center_hz: Hz(100_002_000),
        bandwidth_hz: Hz(12_500),
        bin_hz: None,
        replace: vec![],
    };
    assert_eq!(node.apply(retune).await, Ok(OpResult::Job(input.clone())));
    common::until(
        "the capture's centre follows the retune",
        Duration::from_secs(10),
        || capture_params(&node, &att)["center_hz"] == 100_002_000u64,
    )
    .await;
    assert_running_for(&node, &att, 500).await;
    assert_eq!(capture_params(&node, &att)["mode"], "aprs_decode");
}

#[tokio::test]
async fn job_attach_of_packets_refuses_what_it_cannot_capture_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = packets_node(dir.path()).await;
    let aprs = start_kind(&node, "aprs_decode").await;
    let fm = start_kind(&node, "fm_demod").await;
    let restricted = start_kind(&node, RESTRICTED_ROW).await;
    let jobs = node.job_list().len();
    let refused = |r: Result<OpResult, OpError>, message: String| {
        assert_eq!(r, Err(other(&message)));
    };

    refused(
        attach_packets_as(&node, &aprs, None, Some("slot1")).await,
        "packets reads the job's packet stream: pass stream null".into(),
    );
    refused(
        attach_packets(&node, &fm).await,
        format!(
            "job {} has no packet stream: its kind's outputs list no packet (modes_list)",
            fm.0
        ),
    );
    refused(
        attach_packets(&node, &restricted).await,
        format!("packets: {RESTRICTED_ROW} is restricted: its capture waits on access control"),
    );
    assert_eq!(node.job_list().len(), jobs, "nothing recorded");

    // `language` is not checked for `packets` (ruling R2).
    let captured = match attach_packets_as(&node, &aprs, Some("not a language"), None).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("attach packets: {r:?}"),
    };
    let jobs = node.job_list().len();
    refused(
        attach_packets(&node, &aprs).await,
        format!("job {} is already captured by {}", aprs.0, captured.0),
    );
    // A `packets` job's own edge is a packet output: capturing it would name the wrong
    // job (ruling R7).
    refused(
        attach_packets(&node, &captured).await,
        format!(
            "packets: job {} is an attached job: attach to its input {}",
            captured.0, aprs.0
        ),
    );
    assert_eq!(node.job_list().len(), jobs, "nothing recorded");
    assert_eq!(attached_to(&node, &aprs), vec![captured]);
}

#[tokio::test]
async fn attaching_packets_to_a_job_with_none_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let node = Node::open(cfg(dir.path()), registry_with_packets(), vec![port])
        .await
        .unwrap();
    src.start();
    let audio = start_audio(&node).await;
    let e = node.job_packet_stream(&audio).unwrap_err();
    let message = format!(
        "job {} has no packet stream: its kind's outputs list no packet (modes_list)",
        audio.0
    );
    assert_eq!(e.to_string(), message);
    assert_eq!(attach_packets(&node, &audio).await, Err(other(&message)));
    assert_eq!(attached_to(&node, &audio), Vec::<JobId>::new());
}

/// Attaches of `packets` to one job at once make one capture: every other one is refused
/// as already captured by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_packets_attaches_to_one_job_make_one_capture() {
    const N: usize = 8;
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = packets_node(dir.path()).await;
    let node = Arc::new(node);
    let aprs = start_kind(&node, "aprs_decode").await;
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let tasks: Vec<_> = (0..N)
        .map(|_| {
            let (node, aprs, barrier) = (node.clone(), aprs.clone(), barrier.clone());
            tokio::spawn(async move {
                barrier.wait().await;
                attach_packets(&node, &aprs).await
            })
        })
        .collect();
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.unwrap());
    }
    let won: Vec<JobId> = results
        .iter()
        .filter_map(|r| match r {
            Ok(OpResult::Job(id)) => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(won.len(), 1, "one capture: {results:?}");
    let refusal = Err(other(&format!(
        "job {} is already captured by {}",
        aprs.0, won[0].0
    )));
    assert!(
        results.iter().filter(|r| r.is_err()).all(|r| *r == refusal),
        "{results:?}"
    );
    let packets: Vec<JobId> = node
        .job_list()
        .into_iter()
        .filter(|r| r.kind == "packets" && r.input.as_ref() == Some(&aprs))
        .map(|r| r.id)
        .collect();
    assert_eq!(packets, won);
}

/// A row edited to `access: restricted` after its job was captured stops the capture at
/// the next relaunch (here the input's retune): the `packets` job ends `failed` with the
/// attach's refusal, and the decoder keeps running.
#[tokio::test]
async fn a_row_made_restricted_stops_its_capture_on_relaunch() {
    const ROW: &str = "aprs_operator";
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = packets_node(dir.path()).await;
    let file = dir.path().join("modes").join(format!("{ROW}.json"));
    let (_, aprs) = zeta_core::modes::ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("aprs_decode.json"))
        .unwrap();
    let mut row: serde_json::Value = serde_json::from_str(aprs).unwrap();
    row["id"] = ROW.into();
    std::fs::write(&file, row.to_string()).unwrap();
    node.reload_modes().unwrap();
    let input = start_kind(&node, ROW).await;
    let att = attach_packets_ok(&node, &input).await;
    assert_eq!(capture_params(&node, &att)["mode"], ROW);
    row["access"] = "restricted".into();
    std::fs::write(&file, row.to_string()).unwrap();
    node.reload_modes().unwrap();
    assert_eq!(record(&node, &att).state, JobState::Running);
    let retune = Op::JobRetune {
        job: input.clone(),
        center_hz: Hz(100_002_000),
        bandwidth_hz: Hz(12_500),
        bin_hz: None,
        replace: vec![],
    };
    assert_eq!(node.apply(retune).await, Ok(OpResult::Job(input.clone())));
    let failed = JobState::Failed {
        reason: format!("packets: {ROW} is restricted: its capture waits on access control"),
    };
    common::until(
        "the capture fails on its relaunch",
        Duration::from_secs(10),
        || record(&node, &att).state == failed,
    )
    .await;
    assert_running_for(&node, &input, 500).await;
    assert_eq!(record(&node, &input).tuning.center_hz, Hz(100_002_000));
}

/// A row that says what it carries: its job's context holds `content` from launch (and
/// again after a relaunch), its capture's parameters carry it, and its capture is not
/// refused as a restricted row's is. A row with none leaves both without the member.
#[tokio::test]
async fn a_content_row_puts_content_in_its_jobs_context_and_its_captures_params() {
    const ROW: &str = "aprs_network";
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = packets_node(dir.path()).await;
    let file = dir.path().join("modes").join(format!("{ROW}.json"));
    let (_, aprs) = zeta_core::modes::ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("aprs_decode.json"))
        .unwrap();
    let mut row: serde_json::Value = serde_json::from_str(aprs).unwrap();
    row["id"] = ROW.into();
    row["content"] = "network_traffic".into();
    std::fs::write(&file, row.to_string()).unwrap();
    node.reload_modes().unwrap();
    let plain = start_kind(&node, "aprs_decode").await;
    let input = start_kind(&node, ROW).await;
    // Written when the stages start.
    common::until("the content note", Duration::from_secs(10), || {
        node.job_context(&input)
            .is_some_and(|c| c["content"] == "network_traffic")
    })
    .await;
    assert!(node.job_context(&plain).unwrap().get("content").is_none());
    let att = attach_packets_ok(&node, &input).await;
    assert_eq!(capture_params(&node, &att)["content"], "network_traffic");
    let plain_att = attach_packets_ok(&node, &plain).await;
    assert!(capture_params(&node, &plain_att).get("content").is_none());
    // A relaunch (the retune) keeps both.
    let retune = Op::JobRetune {
        job: input.clone(),
        center_hz: Hz(100_002_000),
        bandwidth_hz: Hz(12_500),
        bin_hz: None,
        replace: vec![],
    };
    assert_eq!(node.apply(retune).await, Ok(OpResult::Job(input.clone())));
    common::until("the capture follows", Duration::from_secs(10), || {
        capture_params(&node, &att)["center_hz"] == 100_002_000u64
    })
    .await;
    assert_eq!(capture_params(&node, &att)["content"], "network_traffic");
    assert_eq!(
        node.job_context(&input).unwrap()["content"],
        "network_traffic"
    );
    assert_running_for(&node, &att, 300).await;
}

/// A row made unreadable after its job was captured stops the capture on relaunch too.
#[tokio::test]
async fn a_row_made_unreadable_stops_its_capture_on_relaunch() {
    const ROW: &str = "aprs_operator";
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = packets_node(dir.path()).await;
    let file = dir.path().join("modes").join(format!("{ROW}.json"));
    let (_, aprs) = zeta_core::modes::ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("aprs_decode.json"))
        .unwrap();
    let mut row: serde_json::Value = serde_json::from_str(aprs).unwrap();
    row["id"] = ROW.into();
    std::fs::write(&file, row.to_string()).unwrap();
    node.reload_modes().unwrap();
    let input = start_kind(&node, ROW).await;
    let att = attach_packets_ok(&node, &input).await;
    std::fs::remove_file(&file).unwrap();
    node.reload_modes().unwrap();
    assert!(node.modes().get(ROW).is_none());
    // A retune checks the row and is refused; a graph put of the input's own graph is a
    // relaunch that does not.
    let retune = Op::JobRetune {
        job: input.clone(),
        center_hz: Hz(100_002_000),
        bandwidth_hz: Hz(12_500),
        bin_hz: None,
        replace: vec![],
    };
    assert!(node.apply(retune).await.is_err());
    assert_eq!(record(&node, &att).state, JobState::Running);
    let graph = record(&node, &input).graph;
    node.job_graph_put(&input, graph).await.unwrap();
    let failed = JobState::Failed {
        reason: format!("packets: {ROW} has no readable mode row: its access cannot be checked"),
    };
    common::until(
        "the capture fails on its relaunch",
        Duration::from_secs(10),
        || record(&node, &att).state == failed,
    )
    .await;
}

/// A row job whose row can no longer be read (broken, then gone, after a modes reload)
/// is not captured: its access cannot be checked, so the attach is refused.
#[tokio::test]
async fn a_job_whose_row_cannot_be_read_is_not_captured() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = packets_node(dir.path()).await;
    let job = start_kind(&node, RESTRICTED_ROW).await;
    let refusal = Err(other(&format!(
        "packets: {RESTRICTED_ROW} has no readable mode row: its access cannot be checked"
    )));
    let file = dir
        .path()
        .join("modes")
        .join(format!("{RESTRICTED_ROW}.json"));
    let jobs = node.job_list().len();
    std::fs::write(&file, "{").unwrap();
    node.reload_modes().unwrap();
    assert_eq!(record(&node, &job).state, JobState::Running);
    assert_eq!(attach_packets(&node, &job).await, refusal);
    std::fs::remove_file(&file).unwrap();
    node.reload_modes().unwrap();
    assert!(node.modes().get(RESTRICTED_ROW).is_none());
    assert_eq!(attach_packets(&node, &job).await, refusal);
    assert_eq!(node.job_list().len(), jobs, "nothing recorded");
}

// Plan 42: a capture judges and records the handler its job runs.

use common::handlers::{self as h, ALT, OWN};

/// A node on [`h::Radio`] whose `adsb_decode` has the handler `alt` giving packets (the
/// row's own gives none), its jobs' packets published by the `external_decode`
/// stand-in.
async fn handler_node(dir: &Path) -> Node {
    let ports: Vec<Arc<dyn Port>> = vec![h::Radio::new()];
    Node::open(h::cfg(dir, true), h::stages(Some(ALT)), ports)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_capture_of_a_job_whose_handler_gives_no_packets_names_who_would() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path()).await;
    let own = h::start(&node, None, &[]).await;
    assert_eq!(record(&node, &own).handler.as_deref(), Some(OWN));
    let jobs = node.job_list().len();
    assert_eq!(
        attach_packets(&node, &own).await,
        Err(other(&format!(
            "job {} has no packet stream: handler {OWN} outputs none ({ALT} does)",
            own.0
        )))
    );
    assert_eq!(node.job_list().len(), jobs, "nothing recorded");
}

#[tokio::test]
async fn a_capture_records_its_jobs_handler() {
    let dir = tempfile::tempdir().unwrap();
    let node = handler_node(dir.path()).await;
    let job = h::start(&node, Some(ALT), &[]).await;
    let att = attach_packets_ok(&node, &job).await;
    assert_eq!(capture_params(&node, &att)["handler"], ALT);
    common::until("a packet captured", Duration::from_secs(10), || {
        node.packets().list().first().is_some_and(|r| r.packets > 0)
    })
    .await;
    let list = match node.apply(Op::PacketsList {}).await {
        Ok(OpResult::PacketCaptures(list)) => list,
        r => panic!("packets_list: {r:?}"),
    };
    assert_eq!(list[0].handler.as_deref(), Some(ALT));
    let wire = serde_json::to_value(&list[0]).unwrap();
    assert_eq!(wire["handler"], ALT);
    let bytes = std::fs::read(&list[0].path).unwrap();
    let whole = zeta_core::pcapng::whole_blocks(&bytes);
    let (_, packets) = zeta_core::pcapng::read(&bytes[..whole]).unwrap();
    let comment = packets[0].comment.as_deref().unwrap_or_default();
    assert!(
        comment.starts_with(&format!("job={} handler={ALT} source=", job.0)),
        "{comment}"
    );
}

/// A decoder whose record names no handler (one from before handlers) runs its row's
/// own, and its capture says so, as its events do.
#[tokio::test]
async fn a_capture_of_an_old_record_names_the_rows_own_handler() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = packets_node(dir.path()).await;
    let band = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(12_500),
    };
    let graph = JobKind::AprsDecode
        .expand(&file0(), band, Hz(48_000), None, None, None, &node.modes())
        .unwrap();
    let input = node
        .job_start("aprs_decode", file0(), band, graph, Some(Hz(48_000)), false)
        .await
        .unwrap();
    assert_eq!(record(&node, &input).handler, None);
    let att = attach_packets_ok(&node, &input).await;
    assert_eq!(capture_params(&node, &att)["handler"], "builtin");
}

// ---- Riders (plan 49.3): a decoder row attached to its demodulator's channel IQ ----

/// The station [`rider_node`] plays: PI 2AB1.
const STATION_PI: &str = "2AB1";

/// A second row riding with `wfm_demod` ([`rider_node`]'s modes directory).
const TWIN_ROW: &str = "rds_twin";

/// A row riding with `wfm_demod` whose decode stage is not registered: not ready.
const GHOST_ROW: &str = "rds_ghost";

/// A copy of the embedded `rds_decode` row as `id` in `modes`, its decode node of stage
/// `stage`.
fn rds_row_as(modes: &Path, id: &str, stage: &str) {
    let (_, rds) = zeta_core::modes::ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("rds_decode.json"))
        .unwrap();
    let mut row: serde_json::Value = serde_json::from_str(rds).unwrap();
    row["id"] = id.into();
    row["decoder"]["nodes"][1]["kind"] = stage.into();
    std::fs::write(modes.join(format!("{id}.json")), row.to_string()).unwrap();
}

/// A node on a file of an FM station with RDS at 100 MHz, sampled at `rate`, tunable from
/// 90 to 110 MHz, its channels on a polyphase bank when `bank`; its modes directory holds
/// [`TWIN_ROW`] and [`GHOST_ROW`]. The file plays while the node runs.
async fn rider_node(dir: &Path, rate: u32, bank: bool) -> (Arc<FileSource>, Node) {
    let src = rider_source(dir, rate);
    let node = rider_open(dir, &src).await;
    if bank {
        node.set_bank_factory(Arc::new(zeta_dsp::bank::PfbBankFactory));
    }
    src.start();
    (src, node)
}

/// [`rider_node`]'s file source (its file and modes directory written once).
fn rider_source(dir: &Path, rate: u32) -> Arc<FileSource> {
    use zeta_core::capture::{CaptureMeta, write_cf32};
    use zeta_core::types::Timestamp;
    let modes = dir.join("modes");
    if !modes.exists() {
        std::fs::create_dir_all(&modes).unwrap();
        rds_row_as(&modes, TWIN_ROW, "rds_decode");
        rds_row_as(&modes, GHOST_ROW, "no_such_stage");
    }
    let iq = zeta_dsp::synth::rds_station_iq(0x2AB1, "ZETA FM ", "HELLO FROM ZETASDR", rate);
    let meta = CaptureMeta {
        sample_rate_hz: Hz(rate as u64),
        center_hz: Hz(100_000_000),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    let meta = write_cf32(dir, "fm", &meta, &iq).unwrap();
    let opts = FileOpts {
        realtime: true,
        block_samples: rate as usize / 100,
        repeat: true,
        tune_range: Some((Hz(90_000_000), Hz(110_000_000))),
    };
    Arc::new(FileSource::open(file0(), &meta, opts).unwrap())
}

/// A node on `src` with the DSP stages and [`rider_source`]'s modes directory.
async fn rider_open(dir: &Path, src: &Arc<FileSource>) -> Node {
    let port: Arc<dyn Port> = src.clone();
    let cfg = NodeConfig {
        modes_dir: Some(dir.join("modes")),
        handlers_dir: None,
        ..cfg(dir)
    };
    let node = Node::open(cfg, registry_with_dsp(), vec![port])
        .await
        .unwrap();
    assert!(node.modes().get(TWIN_ROW).is_some(), "the twin row loads");
    node
}

/// A `job_start` of `kind` at 100 MHz, `bandwidth_hz` wide.
async fn start_at(node: &Node, kind: JobKind, bandwidth_hz: u64) -> JobId {
    let mut op = start_op(kind.name(), 100_000_000);
    if let Op::JobStart {
        bandwidth_hz: bw, ..
    } = &mut op
    {
        *bw = Hz(bandwidth_hz);
    }
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("{} start: {r:?}", kind.name()),
    }
}

/// A `wfm_demod` at 100 MHz, 200 kHz wide, once its channel runs on `path`.
async fn start_wfm(node: &Node, path: &str) -> JobId {
    let demod = start_at(node, JobKind::WfmDemod, 200_000).await;
    wait_path(node, &demod, path).await;
    demod
}

/// Until `job`'s channel reports `path` (`bank` or `direct`).
async fn wait_path(node: &Node, job: &JobId, path: &str) {
    common::until(
        &format!("channel path {path}"),
        Duration::from_secs(5),
        || record(node, job).context["channel"]["path"] == path,
    )
    .await;
}

/// `job_attach` of `kind` to `job` on the wire, with `stream`.
async fn attach_kind_as(
    node: &Node,
    kind: &str,
    job: &JobId,
    stream: Option<&str>,
) -> Result<OpResult, OpError> {
    node.apply(op(serde_json::json!({
        "op": "job_attach", "kind": kind, "job": job.0, "language": null, "stream": stream
    })))
    .await
}

async fn attach_rider(node: &Node, kind: &str, job: &JobId) -> JobId {
    match attach_kind_as(node, kind, job, None).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("attach {kind}: {r:?}"),
    }
}

/// The centre and output rate of `job`'s root `channel` node, as its record holds them.
fn channel_params(node: &Node, job: &JobId) -> (u64, u64) {
    let g = record(node, job).graph;
    let n = g
        .nodes
        .iter()
        .find(|n| n.kind == "channel")
        .expect("a channel");
    (
        n.params["center_hz"].as_u64().unwrap(),
        n.params["out_rate_hz"].as_u64().unwrap(),
    )
}

/// The stream `job`'s `stream_in` node reads, as its record holds it.
fn reads(node: &Node, job: &JobId) -> StreamId {
    let g = record(node, job).graph;
    let n = g
        .nodes
        .iter()
        .find(|n| n.kind == "stream_in")
        .expect("a stream_in");
    StreamId(n.params["stream"].as_str().unwrap().into())
}

/// The next IQ frame on one of `job`'s IQ streams within `ms`: (stream, centre, rate).
async fn iq_frame(node: &Node, job: &JobId, ms: u64) -> Option<(StreamId, u64, u64)> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < deadline {
        let mut rxs: Vec<_> = streams_of(node, job)
            .iter()
            .filter(|s| s.kind == StreamKind::Iq)
            .filter_map(|s| node.bus().subscribe(&s.id).map(|rx| (s.id.clone(), rx)))
            .collect();
        let until = tokio::time::Instant::now() + Duration::from_millis(300);
        while tokio::time::Instant::now() < until {
            for (sid, rx) in &mut rxs {
                if let Ok(f) = rx.try_recv()
                    && let Payload::Iq(b) = f.payload
                {
                    return Some((sid.clone(), b.center_hz.0, b.sample_rate_hz.0));
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    None
}

/// `job`'s audio stream, the samples it carried and the frames lagged over `ms`.
async fn audio_window(node: &Node, job: &JobId, ms: u64) -> (StreamId, usize, u64) {
    let s = streams_of(node, job)
        .into_iter()
        .find(|s| s.kind == StreamKind::Audio)
        .expect("audio stream");
    let mut rx = node.bus().subscribe(&s.id).unwrap();
    let (mut n, mut lags) = (0usize, 0u64);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => break,
            Ok(Ok(f)) => {
                if let Payload::Audio { samples, .. } = f.payload {
                    n += samples.len();
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(k))) => lags += k,
            Ok(Err(broadcast::error::RecvError::Closed)) => break,
        }
    }
    (s.id, n, lags)
}

/// The first `rds` event of `rider` names the station.
async fn assert_rds(node: &Node, rider: &JobId) {
    let ev = event(node, rider, "rds", 15_000)
        .await
        .expect("an rds event");
    assert_eq!(ev["pi"], STATION_PI, "{ev}");
}

/// `rider` reads `demod`'s channel stream now, at its channel's centre and rate.
async fn assert_reads_channel(node: &Node, rider: &JobId, demod: &JobId) -> StreamId {
    let channel = node.job_channel_stream(demod).expect("a channel stream");
    assert_eq!(reads(node, rider), channel);
    let (sid, centre, rate) = iq_frame(node, rider, 3000).await.expect("rider iq");
    assert_eq!((centre, rate), channel_params(node, demod));
    sid
}

/// On a bank: `rds_decode` attached to a running `wfm_demod` is recorded as the row's
/// job on the demodulator, reads its channel at the channel's centre and rate, and
/// decodes the station; a second riding row on the same demodulator decodes it too.
#[tokio::test(flavor = "multi_thread")]
async fn rds_attached_to_a_wfm_demod_gives_rds_events() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 2_400_000, true).await;
    let demod = start_wfm(&node, "bank").await;
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    let rec = record(&node, &rider);
    assert_eq!(rec.kind, "rds_decode");
    assert_eq!(rec.input, Some(demod.clone()));
    assert_eq!(rec.handler.as_deref(), Some("builtin"));
    assert_eq!(rec.tuning, record(&node, &demod).tuning);
    // The row's graph, its root channel now the stream_in.
    assert!(rec.graph.nodes.iter().all(|n| n.kind != "channel"));
    assert!(rec.graph.nodes.iter().any(|n| n.kind == "rds_decode"));
    assert_reads_channel(&node, &rider, &demod).await;
    assert_rds(&node, &rider).await;
    let twin = attach_rider(&node, TWIN_ROW, &demod).await;
    assert_rds(&node, &twin).await;
    assert_eq!(record(&node, &rider).state, JobState::Running);
}

/// Direct: the demodulator's audio runs on as it was while a rider reads its channel.
#[tokio::test(flavor = "multi_thread")]
async fn the_demodulators_audio_is_unchanged_with_a_rider_attached() {
    audio_unchanged(960_000, false).await;
}

/// On a bank: the demodulator's audio runs on as it was while a rider reads its channel.
#[tokio::test(flavor = "multi_thread")]
async fn the_demodulators_audio_is_unchanged_with_a_rider_attached_on_a_bank() {
    audio_unchanged(2_400_000, true).await;
}

/// The demodulator's audio, on the bank or direct, keeps its stream, loses no frame and
/// carries as many samples with a rider attached as before it.
async fn audio_unchanged(rate: u32, bank: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), rate, bank).await;
    let path = if bank { "bank" } else { "direct" };
    let demod = start_wfm(&node, path).await;
    let (audio0, before, lag0) = audio_window(&node, &demod, 2000).await;
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    assert_rds(&node, &rider).await;
    let (audio1, during, lag1) = audio_window(&node, &demod, 2000).await;
    assert_eq!(audio0, audio1, "the attach did not restart the demodulator");
    assert_eq!((lag0, lag1), (0, 0));
    assert!(
        during as f64 > before as f64 * 0.9 && (during as f64) < before as f64 * 1.1,
        "{before} vs {during}"
    );
}

/// Direct: a retune of the demodulator's centre and width relaunches the rider under its
/// id on the new channel, at the new centre and the new channel rate, and it decodes on.
#[tokio::test(flavor = "multi_thread")]
async fn an_attached_rider_follows_its_inputs_retune() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let demod = start_wfm(&node, "direct").await;
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    let s1 = assert_reads_channel(&node, &rider, &demod).await;
    let (_, rate1) = channel_params(&node, &demod);
    let op = Op::JobRetune {
        job: demod.clone(),
        center_hz: Hz(100_010_000),
        bandwidth_hz: Hz(180_000),
        bin_hz: None,
        replace: vec![],
    };
    assert_eq!(node.apply(op).await, Ok(OpResult::Job(demod.clone())));
    common::until(
        "the demodulator's channel at its new centre",
        Duration::from_secs(5),
        || channel_params(&node, &demod).0 == 100_010_000,
    )
    .await;
    assert_running_for(&node, &rider, 1000).await;
    let s2 = assert_reads_channel(&node, &rider, &demod).await;
    assert_ne!(s1, s2, "a new run's stream");
    let (centre2, rate2) = channel_params(&node, &demod);
    assert_eq!(centre2, 100_010_000);
    assert_ne!(
        rate1, rate2,
        "the narrower band has a narrower channel rate"
    );
    assert_eq!(record(&node, &rider).tuning, record(&node, &demod).tuning);
    assert_rds(&node, &rider).await;
}

/// A retune of the demodulator under the rider's `min_hz` ends the rider `failed` with
/// the attach's refusal; the demodulator runs on.
#[tokio::test(flavor = "multi_thread")]
async fn a_rider_fails_when_its_input_is_retuned_under_its_min_hz() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let demod = start_wfm(&node, "direct").await;
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    let op = Op::JobRetune {
        job: demod.clone(),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(120_000),
        bin_hz: None,
        replace: vec![],
    };
    assert_eq!(node.apply(op).await, Ok(OpResult::Job(demod.clone())));
    let state = wait_not_running(&node, &rider, 5000).await;
    let want = format!(
        "rds_decode cannot ride on job {}: it needs a band of at least 150000 Hz, and the job's is 120000 Hz",
        demod.0
    );
    assert_eq!(state, JobState::Failed { reason: want });
    assert_eq!(record(&node, &demod).state, JobState::Running);
    assert!(streams_of(&node, &rider).is_empty());
}

/// On a bank: a capture change that relaunches the demodulator (the source widened past
/// its capture) brings the rider back under its id on the new run's channel.
#[tokio::test(flavor = "multi_thread")]
async fn an_attached_rider_comes_back_with_its_inputs_relaunch() {
    let rate = 2_400_000u64;
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), rate as u32, true).await;
    let demod = start_wfm(&node, "bank").await;
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    let s1 = assert_reads_channel(&node, &rider, &demod).await;
    let channel1 = node.job_channel_stream(&demod).unwrap();
    let want = Tuning {
        center_hz: Hz(100_000_000 + rate / 4),
        bandwidth_hz: Hz(rate),
    };
    node.source_widen(&file0(), want, &[]).await.expect("widen");
    common::until(
        "the demodulator on a new run",
        Duration::from_secs(5),
        || node.job_channel_stream(&demod).is_ok_and(|s| s != channel1),
    )
    .await;
    wait_path(&node, &demod, "bank").await;
    assert_running_for(&node, &rider, 1000).await;
    let s2 = assert_reads_channel(&node, &rider, &demod).await;
    assert_ne!(s1, s2, "relaunched with its input");
}

/// A stop of the demodulator ends each rider on it `stopped`, with no stream of it left.
#[tokio::test(flavor = "multi_thread")]
async fn an_attached_rider_stops_with_its_input() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let demod = start_wfm(&node, "direct").await;
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    let twin = attach_rider(&node, TWIN_ROW, &demod).await;
    assert!(iq_frame(&node, &rider, 3000).await.is_some());
    node.job_stop(&demod).await.unwrap();
    assert_eq!(
        wait_not_running(&node, &rider, 3000).await,
        JobState::Stopped
    );
    assert_eq!(
        wait_not_running(&node, &twin, 3000).await,
        JobState::Stopped
    );
    assert!(streams_of(&node, &rider).is_empty());
    assert!(streams_of(&node, &twin).is_empty());
}

/// A stop of the rider leaves its demodulator running, its audio on the same stream.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_rider_leaves_its_demodulator_running() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let demod = start_wfm(&node, "direct").await;
    let audio = node.job_audio_stream(&demod).unwrap();
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    assert!(iq_frame(&node, &rider, 3000).await.is_some());
    node.job_stop(&rider).await.unwrap();
    assert_eq!(
        wait_not_running(&node, &rider, 3000).await,
        JobState::Stopped
    );
    assert_running_for(&node, &demod, 1000).await;
    assert_eq!(node.job_audio_stream(&demod).unwrap(), audio);
    assert!(streams_of(&node, &rider).is_empty());
}

/// A `graph_put` of the demodulator that keeps its root channel relaunches the rider
/// under its id on the new run's channel; one that drops it fails the rider, naming why.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_put_on_a_ridden_demodulator_rebuilds_or_fails_its_rider() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let demod = start_wfm(&node, "direct").await;
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    let s1 = assert_reads_channel(&node, &rider, &demod).await;
    let channel1 = node.job_channel_stream(&demod).unwrap();
    let graph = record(&node, &demod).graph;
    node.job_graph_put(&demod, graph).await.unwrap();
    common::until(
        "the demodulator on a new run",
        Duration::from_secs(5),
        || node.job_channel_stream(&demod).is_ok_and(|s| s != channel1),
    )
    .await;
    assert_running_for(&node, &rider, 1000).await;
    let s2 = assert_reads_channel(&node, &rider, &demod).await;
    assert_ne!(s1, s2, "rebuilt on the new run");
    assert_rds(&node, &rider).await;
    let put = Op::GraphPut {
        job: demod.clone(),
        graph: node_graph("audio_src", serde_json::json!({})),
    };
    assert!(node.apply(put).await.is_ok());
    let want = format!(
        "rds_decode cannot ride on job {}: its graph has no root channel node",
        demod.0
    );
    assert_eq!(
        wait_not_running(&node, &rider, 5000).await,
        JobState::Failed { reason: want }
    );
    assert!(streams_of(&node, &rider).is_empty());
    assert_eq!(record(&node, &demod).state, JobState::Running);
}

/// A rider whose row was edited so it no longer rides fails at its next relaunch,
/// naming the row, as one whose row was made broken does; its demodulator runs on.
#[tokio::test(flavor = "multi_thread")]
async fn a_rider_whose_row_no_longer_rides_fails_on_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let demod = start_wfm(&node, "direct").await;
    let twin = attach_rider(&node, TWIN_ROW, &demod).await;
    assert!(iq_frame(&node, &twin, 3000).await.is_some());
    let file = dir.path().join("modes").join(format!("{TWIN_ROW}.json"));
    let mut row: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    row.as_object_mut().unwrap().remove("rides");
    std::fs::write(&file, row.to_string()).unwrap();
    node.reload_modes().unwrap();
    assert!(node.modes().get(TWIN_ROW).unwrap().row.rides.is_none());
    let graph = record(&node, &demod).graph;
    node.job_graph_put(&demod, graph).await.unwrap();
    let want = format!(
        "{TWIN_ROW} cannot ride on job {}: mode {TWIN_ROW} no longer rides with a demodulator",
        demod.0
    );
    assert_eq!(
        wait_not_running(&node, &twin, 5000).await,
        JobState::Failed { reason: want }
    );
    assert_eq!(record(&node, &demod).state, JobState::Running);
}

/// A rider resumes after a node restart, after its input, on the input's new channel.
#[tokio::test(flavor = "multi_thread")]
async fn an_attached_rider_resumes_on_its_inputs_new_channel() {
    let dir = tempfile::tempdir().unwrap();
    let src = rider_source(dir.path(), 960_000);
    // The file source plays only while subscribed: keep it playing between nodes.
    std::mem::forget(src.subscribe());
    src.start();
    let (demod, rider) = {
        let node = rider_open(dir.path(), &src).await;
        let demod = start_wfm(&node, "direct").await;
        let rider = attach_rider(&node, "rds_decode", &demod).await;
        assert_rds(&node, &rider).await;
        (demod, rider)
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let node = rider_open(dir.path(), &src).await;
    common::until("both resumed", Duration::from_secs(5), || {
        record(&node, &demod).state == JobState::Running
            && record(&node, &rider).state == JobState::Running
    })
    .await;
    assert_reads_channel(&node, &rider, &demod).await;
    assert_rds(&node, &rider).await;
}

/// `rds_decode` is refused on a demodulator it does not ride with, naming both; nothing
/// is attached.
#[tokio::test(flavor = "multi_thread")]
async fn a_rider_is_refused_on_a_job_it_does_not_ride_with() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let fm = start_at(&node, JobKind::FmDemod, 12_500).await;
    let r = attach_kind_as(&node, "rds_decode", &fm, None).await;
    let want = format!(
        "rds_decode cannot ride on job {}: it rides with wfm_demod, and the job runs fm_demod",
        fm.0
    );
    assert_eq!(r, Err(other(&want)));
    assert!(attached_to(&node, &fm).is_empty());
}

/// `rds_decode` is refused on a `wfm_demod` narrower than its `min_hz`; nothing is
/// attached.
#[tokio::test(flavor = "multi_thread")]
async fn a_rider_is_refused_on_a_band_under_its_min_hz() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let demod = start_at(&node, JobKind::WfmDemod, 120_000).await;
    let r = attach_kind_as(&node, "rds_decode", &demod, None).await;
    let want = format!(
        "rds_decode cannot ride on job {}: it needs a band of at least 150000 Hz, and the job's is 120000 Hz",
        demod.0
    );
    assert_eq!(r, Err(other(&want)));
    assert!(attached_to(&node, &demod).is_empty());
}

/// A rider that is not ready, one already riding on the job, a `stream` and a job of the
/// kind whose graph has no root channel are each refused; nothing more is attached.
#[tokio::test(flavor = "multi_thread")]
async fn a_rider_is_refused_unready_twice_with_a_stream_or_without_a_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (_src, node) = rider_node(dir.path(), 960_000, false).await;
    let demod = start_wfm(&node, "direct").await;
    let r = attach_kind_as(&node, GHOST_ROW, &demod, None).await;
    let Err(OpError::Other { message }) = r else {
        panic!("{r:?}")
    };
    let head = format!(
        "{GHOST_ROW} cannot ride on job {}: mode {GHOST_ROW} is broken: ",
        demod.0
    );
    assert!(message.starts_with(&head), "{message}");
    let r = attach_kind_as(&node, "rds_decode", &demod, Some("slot1")).await;
    assert_eq!(
        r,
        Err(other(
            "rds_decode reads the job's channel IQ: pass stream null"
        ))
    );
    let rider = attach_rider(&node, "rds_decode", &demod).await;
    let r = attach_kind_as(&node, "rds_decode", &demod, None).await;
    let want = format!(
        "rds_decode cannot ride on job {}: rds_decode job {} already rides on it",
        demod.0, rider.0
    );
    assert_eq!(r, Err(other(&want)));
    assert_eq!(attached_to(&node, &demod), vec![rider]);
    // A wfm_demod whose graph the operator put without a channel.
    let bare = start_at(&node, JobKind::WfmDemod, 190_000).await;
    let put = Op::GraphPut {
        job: bare.clone(),
        graph: node_graph("audio_src", serde_json::json!({})),
    };
    assert!(node.apply(put).await.is_ok());
    common::until("the put graph runs", Duration::from_secs(5), || {
        node.job_audio_stream(&bare).is_ok()
            && record(&node, &bare)
                .graph
                .nodes
                .iter()
                .all(|n| n.kind != "channel")
    })
    .await;
    let r = attach_kind_as(&node, "rds_decode", &bare, None).await;
    let want = format!(
        "rds_decode cannot ride on job {}: its graph has no root channel node",
        bare.0
    );
    assert_eq!(r, Err(other(&want)));
    assert!(attached_to(&node, &bare).is_empty());
}
