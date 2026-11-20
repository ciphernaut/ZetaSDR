//! Voices in the node: a job whose stage has several voices publishes its main audio
//! stream (no role) and one stream per voice (`Stage::voice_roles`); its row lists them as
//! `voices`, `job_attach`'s `stream` reads the one it names, and `job_start`'s
//! `audio_policy` reaches the stage through the row's `"$audio_policy"`, kept by the
//! job's record through a restart and a retune. Over the `two_voice_test` stage, whose
//! three audio streams each carry their own tone; `denoise`, `vad` and `transcribe` are
//! stand-ins, the last naming the tone it hears.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{AudioPolicy, JobKind, Op, OpError, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::stream_in::StreamInStage;
use zeta_core::test_fixtures::{TWO_VOICE_RATE_HZ, TWO_VOICE_TONES_HZ, TwoVoiceTestStage};
use zeta_core::types::{Hz, JobId, SourceId, StreamId, StreamKind};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// The operator row over `two_voice_test`.
const ROW: &str = "two_voice_row";

/// What the stand-in `transcribe` says it hears, by tone: the main stream, then each slot.
const HEARD: [&str; 3] = ["main", "slot1", "slot2"];

/// Power of `hz` in `x` at the test stage's rate (Goertzel).
fn power(x: &[f32], hz: f32) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f64::from(hz) / TWO_VOICE_RATE_HZ as f64;
    let c = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = f64::from(v) + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - c * s1 * s2) / (x.len().max(1) as f64).powi(2)
}

/// The powers of the three test tones in `x`.
fn tones(x: &[f32]) -> [f64; 3] {
    TWO_VOICE_TONES_HZ.map(|hz| power(x, hz))
}

/// Which test tone dominates `x`, by its [`HEARD`] name.
fn loudest(x: &[f32]) -> &'static str {
    let p = tones(x);
    let i = (0..3).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap();
    HEARD[i]
}

/// A `transcribe` stand-in: one `transcript` event per audio frame, its `text` the test
/// tone the frame carries and its `stream` its params' (as the real stage's events).
struct HearStage {
    spec: StageSpec,
}

#[async_trait]
impl Stage for HearStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        params: Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let (Some(rx), Some(out)) = (inputs.first_mut(), outputs.first()) else {
            return Err(StageError::Failed("expected 1 input, 1 output".into()));
        };
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        let Payload::Audio { samples, .. } = &frame.payload else { continue };
                        let event = json!({
                            "protocol": "transcript",
                            "text": loudest(samples),
                            "stream": params.get("stream").cloned().unwrap_or(Value::Null),
                        });
                        bus.publish(Frame::new(out.clone(), frame.provenance.clone(), Payload::Event(event)));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

/// A stand-in that passes its audio input on unchanged.
struct Through {
    spec: StageSpec,
}

#[async_trait]
impl Stage for Through {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let (Some(rx), Some(out)) = (inputs.first_mut(), outputs.first()) else {
            return Err(StageError::Failed("expected 1 input".into()));
        };
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        bus.publish(Frame::new(out.clone(), frame.provenance.clone(), frame.payload.clone()));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

fn spec(kind: &'static str, outputs: &[StreamKind]) -> StageSpec {
    StageSpec {
        kind,
        inputs: vec![StreamKind::Audio],
        outputs: outputs.to_vec(),
        params_schema: schemars::json_schema!({"type": "object"}),
    }
}

/// The stand-ins (registered first, so they win their kinds), the test stage, then the
/// real DSP stages (`channel`, the demodulators, `mix`, `m17_decode`).
fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(StreamInStage::new()));
    r.register(Arc::new(Through {
        spec: spec("denoise", &[StreamKind::Audio]),
    }));
    r.register(Arc::new(Through {
        spec: spec("vad", &[StreamKind::Audio, StreamKind::Event]),
    }));
    r.register(Arc::new(HearStage {
        spec: spec("transcribe", &[StreamKind::Event]),
    }));
    r.register(Arc::new(TwoVoiceTestStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

/// An operator row: a channel into `two_voice_test`, its `audio` param the job's policy.
fn row() -> Value {
    json!({
        "id": ROW,
        "name": "Two voices (test)",
        "family": "general",
        "band": {"bandwidth_hz": 12500, "centers_hz": [], "min_hz": null, "range_hz": null},
        "modulation": ["fsk"],
        "label": "2V",
        "protocol": "two_voice_test",
        "decoder": {
            "kind": "graph",
            "nodes": [
                {"id": "channel", "kind": "channel", "params": {
                    "center_hz": "$center_hz",
                    "bandwidth_hz": "$bandwidth_hz",
                    "out_rate_hz": "$out_rate_hz"
                }},
                {"id": "decode", "kind": "two_voice_test", "params": {"audio": "$audio_policy"}}
            ],
            "edges": [{"from": "channel", "to": "decode", "kind": "iq"}]
        },
        "event": {
            "schema": {
                "type": "object",
                "properties": {"protocol": {"type": "string"}, "audio": {"type": "string"}}
            },
            "summary": "{audio}"
        },
        "identify": {
            "proof": "event",
            "min_events": 1,
            "dwell_ms": 1000,
            "min_span_ms": 0,
            "quick": null,
            "priority": 0
        },
        "catalogue": []
    })
}

fn tone() -> SourceId {
    SourceId("tone".into())
}

fn port(dir: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(tone(), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn cfg(dir: &Path) -> NodeConfig {
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(modes.join(format!("{ROW}.json")), row().to_string()).unwrap();
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        owns_state_dir: true,
        bank_factory: None,
        modes_dir: Some(modes),
        handlers_dir: None,
    }
}

/// A node on the tone with the test row, the source playing.
async fn open(dir: &Path) -> Node {
    let (src, port) = port(dir);
    let node = Node::open(cfg(dir), registry(), vec![port]).await.unwrap();
    src.start();
    node
}

fn start_op(kind: JobKind, center_hz: u64, audio_policy: Option<AudioPolicy>) -> Op {
    Op::JobStart {
        kind,
        source: tone(),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(12_500),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        audio_policy,
        dwell_scale: None,
        elastic: false,
        replace: vec![],
    }
}

fn two_voice() -> JobKind {
    JobKind::from_name(ROW).unwrap()
}

async fn start(node: &Node, op: Op) -> Result<JobId, OpError> {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => Ok(id),
        Ok(r) => panic!("expected a job, got {r:?}"),
        Err(e) => Err(e),
    }
}

async fn start_two_voice(node: &Node, center_hz: u64, policy: Option<AudioPolicy>) -> JobId {
    start(node, start_op(two_voice(), center_hz, policy))
        .await
        .unwrap()
}

async fn attach(node: &Node, job: &JobId, stream: Option<&str>) -> Result<JobId, OpError> {
    let op = Op::JobAttach {
        kind: JobKind::Transcribe,
        job: job.clone(),
        language: None,
        stream: stream.map(String::from),
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => Ok(id),
        Ok(r) => panic!("expected a job, got {r:?}"),
        Err(e) => Err(e),
    }
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

/// Poll `f` every 20 ms for up to 5 s, panicking with `what` when it never holds.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The first event with `protocol` on any of `job`'s event outputs within 5 s.
async fn event(node: &Node, job: &JobId, protocol: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut rxs: Vec<_> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        if rxs.is_empty() {
            rxs = node
                .job_outputs(job)
                .into_iter()
                .filter(|(k, _)| *k == StreamKind::Event)
                .filter_map(|(_, s)| node.bus().subscribe(&s))
                .collect();
        }
        for rx in &mut rxs {
            while let Ok(f) = rx.try_recv() {
                if let Payload::Event(v) = f.payload
                    && v["protocol"] == protocol
                {
                    return v;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no {protocol} event from job {} within 5 s", job.0)
}

/// What the attached stand-in `job` hears, from its next `transcript` event.
async fn heard(node: &Node, job: &JobId) -> Value {
    event(node, job, "transcript").await["text"].clone()
}

/// The `audio` param of `job`'s `two_voice_test` node.
fn graph_policy(node: &Node, job: &JobId) -> Value {
    record(node, job)
        .graph
        .nodes
        .iter()
        .find(|n| n.kind == "two_voice_test")
        .expect("a two_voice_test node")
        .params["audio"]
        .clone()
}

/// The audio `stream` carries over about `ms` milliseconds.
async fn listen(node: &Node, stream: &StreamId, ms: u64) -> Vec<f32> {
    let mut rx = node.bus().subscribe(stream).expect("stream registered");
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    let mut out = Vec::new();
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(f)) => {
                if let Payload::Audio { samples, .. } = &f.payload {
                    out.extend_from_slice(samples);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) | Err(_) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) => break,
        }
    }
    out
}

fn other(message: String) -> OpError {
    OpError::Other { message }
}

#[tokio::test]
async fn a_two_voice_row_lists_its_voices() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let Ok(OpResult::Modes { modes }) = node
        .apply(Op::ModesList {
            family: None,
            state: None,
        })
        .await
    else {
        panic!("modes_list")
    };
    let two = modes.iter().find(|m| m.id.0 == ROW).expect("the test row");
    assert_eq!(two.reason, None);
    assert_eq!(two.voices, ["slot1", "slot2"]);
    // Kinds, not one entry per stream.
    assert_eq!(two.outputs, [StreamKind::Event, StreamKind::Audio]);
    // The embedded DMR row: a slot each.
    let dmr = modes.iter().find(|m| m.id.0 == "dmr_decode").unwrap();
    assert_eq!(dmr.voices, ["slot1", "slot2"]);
    assert_eq!(dmr.outputs, [StreamKind::Event, StreamKind::Audio]);
    let m17 = modes.iter().find(|m| m.id.0 == "m17_decode").unwrap();
    assert!(m17.voices.is_empty(), "{:?}", m17.voices);
    // Always written, even empty.
    let v = serde_json::to_value(m17).unwrap();
    assert_eq!(v["voices"], json!([]));
}

#[tokio::test]
async fn a_plain_attach_reads_the_main_stream() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let job = start_two_voice(&node, 100_000_000, None).await;
    let t = attach(&node, &job, None).await.unwrap();
    assert_eq!(record(&node, &t).stream, None);
    assert_eq!(heard(&node, &t).await, "main");
    let first = &record(&node, &t).graph.nodes[0];
    assert_eq!(
        first.params["stream"],
        node.job_audio_stream(&job).unwrap().0
    );
    // The transcript events say so.
    assert_eq!(event(&node, &t, "transcript").await["stream"], Value::Null);
}

#[tokio::test]
async fn an_attach_to_a_voice_reads_that_voice() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let job = start_two_voice(&node, 100_000_000, None).await;
    let two = attach(&node, &job, Some("slot2")).await.unwrap();
    assert_eq!(record(&node, &two).stream.as_deref(), Some("slot2"));
    assert_eq!(heard(&node, &two).await, "slot2");
    assert_eq!(event(&node, &two, "transcript").await["stream"], "slot2");
    // One per slot, together.
    let one = attach(&node, &job, Some("slot1")).await.unwrap();
    assert_eq!(heard(&node, &one).await, "slot1");
    assert_eq!(heard(&node, &two).await, "slot2");
    assert_eq!(record(&node, &one).state, JobState::Running);
    assert_eq!(record(&node, &two).state, JobState::Running);
    // Each reads its own stream, neither the main one.
    let main = node.job_audio_stream(&job).unwrap();
    let streams: Vec<Value> = [&one, &two]
        .map(|t| record(&node, t).graph.nodes[0].params["stream"].clone())
        .to_vec();
    assert_ne!(streams[0], streams[1]);
    assert!(streams.iter().all(|s| *s != main.0), "{streams:?}");
}

#[tokio::test]
async fn an_unknown_voice_is_refused_naming_the_voices() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let job = start_two_voice(&node, 100_000_000, None).await;
    let before = node.job_list().len();
    assert_eq!(
        attach(&node, &job, Some("slot3")).await,
        Err(other(format!(
            "stream: {} has no voice slot3 (it has: slot1, slot2)",
            job.0
        )))
    );
    let fm = start(&node, start_op(JobKind::FmDemod, 100_010_000, None))
        .await
        .unwrap();
    assert_eq!(
        attach(&node, &fm, Some("slot1")).await,
        Err(other(format!("stream: {} has one voice", fm.0)))
    );
    // Nothing recorded for either.
    assert_eq!(node.job_list().len(), before + 1);
}

#[tokio::test]
async fn the_policy_reaches_the_stage() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let follow = start_two_voice(&node, 100_000_000, Some(AudioPolicy::Follow)).await;
    assert_eq!(
        event(&node, &follow, "two_voice_test").await["audio"],
        "follow"
    );
    assert_eq!(graph_policy(&node, &follow), "follow");
    assert_eq!(
        record(&node, &follow).audio_policy,
        Some(AudioPolicy::Follow)
    );
    let default = start_two_voice(&node, 100_015_000, None).await;
    assert_eq!(
        event(&node, &default, "two_voice_test").await["audio"],
        "first_active"
    );
    assert_eq!(record(&node, &default).audio_policy, None);
}

#[tokio::test]
async fn a_policy_on_a_one_voice_row_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let m17 = JobKind::from_name("m17_decode").unwrap();
    assert_eq!(
        start(
            &node,
            start_op(m17.clone(), 100_000_000, Some(AudioPolicy::All))
        )
        .await,
        Err(other("audio_policy: m17_decode has one voice".into()))
    );
    assert_eq!(
        start(
            &node,
            start_op(JobKind::FmDemod, 100_000_000, Some(AudioPolicy::Follow))
        )
        .await,
        Err(other("audio_policy: fm_demod has one voice".into()))
    );
    assert!(node.job_list().is_empty(), "nothing started");
    let id = start(&node, start_op(m17, 100_000_000, None))
        .await
        .unwrap();
    assert_eq!(record(&node, &id).state, JobState::Running);
}

#[tokio::test]
async fn a_restart_resumes_the_policy_and_the_attached_stream() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    // The file source plays only while subscribed: keep it playing between nodes.
    let _hold = port.subscribe();
    src.start();
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
        .await
        .unwrap();
    let job = start_two_voice(&node, 100_000_000, Some(AudioPolicy::Follow)).await;
    let t = attach(&node, &job, Some("slot2")).await.unwrap();
    assert_eq!(heard(&node, &t).await, "slot2");
    drop(node);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    until("both resumed", || {
        record(&node, &job).state == JobState::Running
            && record(&node, &t).state == JobState::Running
    })
    .await;
    assert_eq!(record(&node, &job).audio_policy, Some(AudioPolicy::Follow));
    assert_eq!(
        event(&node, &job, "two_voice_test").await["audio"],
        "follow"
    );
    assert_eq!(record(&node, &t).stream.as_deref(), Some("slot2"));
    assert_eq!(heard(&node, &t).await, "slot2");
}

#[tokio::test]
async fn a_retune_keeps_the_policy() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let job = start_two_voice(&node, 100_000_000, Some(AudioPolicy::All)).await;
    let t = attach(&node, &job, Some("slot1")).await.unwrap();
    let r = node
        .apply(Op::JobRetune {
            job: job.clone(),
            center_hz: Hz(100_005_000),
            bandwidth_hz: Hz(12_500),
            bin_hz: None,
            replace: vec![],
        })
        .await;
    assert_eq!(r, Ok(OpResult::Job(job.clone())));
    assert_eq!(record(&node, &job).tuning.center_hz, Hz(100_005_000));
    assert_eq!(record(&node, &job).audio_policy, Some(AudioPolicy::All));
    assert_eq!(graph_policy(&node, &job), "all");
    assert_eq!(event(&node, &job, "two_voice_test").await["audio"], "all");
    // The attached job follows on the same voice.
    until("the attached job relaunched", || {
        record(&node, &t).state == JobState::Running
    })
    .await;
    assert_eq!(heard(&node, &t).await, "slot1");
}

/// An attached job whose relaunch after its input's retune fails (the row, reloaded
/// meanwhile, no longer has its voice) ends `failed` at the input's new tuning, the one
/// it was asked to run at, on disk too.
#[tokio::test]
async fn an_attached_job_whose_relaunch_fails_after_a_retune_records_the_new_tuning() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let job = start_two_voice(&node, 100_000_000, None).await;
    let t = attach(&node, &job, Some("slot1")).await.unwrap();
    assert_eq!(heard(&node, &t).await, "slot1");
    // The row's decoder is now a one-voice demodulator.
    let mut one_voice = row();
    one_voice["decoder"]["nodes"][1] =
        json!({"id": "decode", "kind": "am_demod", "params": {"audio_rate_hz": 16000}});
    std::fs::write(
        dir.path().join(format!("modes/{ROW}.json")),
        one_voice.to_string(),
    )
    .unwrap();
    node.reload_modes().unwrap();
    let r = node
        .apply(Op::JobRetune {
            job: job.clone(),
            center_hz: Hz(100_005_000),
            bandwidth_hz: Hz(12_500),
            bin_hz: None,
            replace: vec![],
        })
        .await;
    assert_eq!(r, Ok(OpResult::Job(job.clone())));
    let retuned = record(&node, &job).tuning;
    assert_eq!(retuned.center_hz, Hz(100_005_000));
    assert_eq!(record(&node, &job).state, JobState::Running);
    until("the attached relaunch failed", || {
        matches!(record(&node, &t).state, JobState::Failed { .. })
    })
    .await;
    let JobState::Failed { reason } = record(&node, &t).state else {
        unreachable!()
    };
    assert_eq!(reason, format!("stream: {} has one voice", job.0));
    assert_eq!(record(&node, &t).tuning, retuned);
    let path = dir.path().join(format!("state/jobs/{}.json", t.0));
    let on_disk: JobRecord = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(on_disk.tuning, retuned);
    assert!(matches!(on_disk.state, JobState::Failed { .. }));
}

/// The refusal's text for a start on `center_hz` that repeats `job`, running `policy`.
fn duplicate_message(center_hz: u64, job: &JobId, policy: &str) -> String {
    format!(
        "an identical {ROW} already runs on tone at {center_hz}/12500 Hz as job {} \
         (audio_policy {policy}; a start with replace naming it changes the policy)",
        job.0
    )
}

#[tokio::test]
async fn a_start_with_another_policy_is_refused_naming_the_running_policy() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let job = start_two_voice(&node, 100_000_000, Some(AudioPolicy::Follow)).await;
    let before = node.job_list().len();
    assert_eq!(
        start(
            &node,
            start_op(two_voice(), 100_000_000, Some(AudioPolicy::All))
        )
        .await,
        Err(OpError::Duplicate {
            message: duplicate_message(100_000_000, &job, "follow"),
            job: job.clone(),
        })
    );
    // A job started with no policy runs the row's default, and says so.
    let default = start_two_voice(&node, 100_015_000, None).await;
    assert_eq!(
        start(
            &node,
            start_op(two_voice(), 100_015_000, Some(AudioPolicy::Follow))
        )
        .await,
        Err(OpError::Duplicate {
            message: duplicate_message(100_015_000, &default, "first_active"),
            job: default.clone(),
        })
    );
    // Nothing started, nothing changed.
    assert_eq!(node.job_list().len(), before + 1);
    assert_eq!(record(&node, &job).audio_policy, Some(AudioPolicy::Follow));
    assert_eq!(record(&node, &job).state, JobState::Running);
}

#[tokio::test]
async fn a_start_replacing_the_running_job_changes_its_policy() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let job = start_two_voice(&node, 100_000_000, Some(AudioPolicy::Follow)).await;
    let mut replacing = start_op(two_voice(), 100_000_000, Some(AudioPolicy::All));
    if let Op::JobStart { replace, .. } = &mut replacing {
        *replace = vec![job.clone()];
    }
    let new = start(&node, replacing).await.unwrap();
    assert_ne!(new, job);
    until("the replaced job stopped", || {
        record(&node, &job).state == JobState::Stopped
    })
    .await;
    assert_eq!(record(&node, &new).state, JobState::Running);
    assert_eq!(record(&node, &new).audio_policy, Some(AudioPolicy::All));
    assert_eq!(graph_policy(&node, &new), "all");
    assert_eq!(event(&node, &new, "two_voice_test").await["audio"], "all");
}

#[tokio::test]
async fn the_mix_reads_the_main_stream_only() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let job = start_two_voice(&node, 100_000_000, None).await;
    let mix = start(&node, Op::MixStart { sources: None }).await.unwrap();
    until("the mix reads one input", || {
        node.job_context(&mix).unwrap()["mix"]["inputs"] == 1
    })
    .await;
    let out = node.job_audio_stream(&mix).unwrap();
    let heard = listen(&node, &out, 1_500).await;
    assert!(!heard.is_empty(), "the mix played nothing");
    let [main, slot1, slot2] = tones(&heard);
    assert!(
        main > 100.0 * slot1 && main > 100.0 * slot2,
        "main {main:e}, slot1 {slot1:e}, slot2 {slot2:e}"
    );
    // The job's main stream is the mix's input, and the voices are none of the job's audio.
    assert_eq!(node.job_audio_stream(&job).unwrap(), {
        let outs = node.job_outputs(&job);
        outs[1].1.clone()
    });
}

#[test]
fn requests_without_the_new_fields_still_parse() {
    let op: Op = serde_json::from_value(json!({
        "op": "job_start", "kind": "fm_demod", "source": "tone",
        "center_hz": 100_000_000, "bandwidth_hz": 12_500,
        "deviation_hz": null, "sideband": null, "elastic": false, "replace": [],
    }))
    .unwrap();
    assert!(
        matches!(
            op,
            Op::JobStart {
                audio_policy: None,
                dwell_scale: None,
                ..
            }
        ),
        "{op:?}"
    );
    let op: Op = serde_json::from_value(json!({
        "op": "job_attach", "kind": "transcribe", "job": "j", "language": null,
    }))
    .unwrap();
    assert!(matches!(op, Op::JobAttach { stream: None, .. }), "{op:?}");
    let op: Op = serde_json::from_value(json!({
        "op": "job_start", "kind": "fm_demod", "source": "tone",
        "center_hz": 100_000_000, "bandwidth_hz": 12_500,
        "deviation_hz": null, "sideband": null, "audio_policy": "follow",
        "elastic": false, "replace": [],
    }))
    .unwrap();
    assert!(
        matches!(
            op,
            Op::JobStart {
                audio_policy: Some(AudioPolicy::Follow),
                dwell_scale: None,
                ..
            }
        ),
        "{op:?}"
    );
    let bad: Result<Op, _> = serde_json::from_value(json!({
        "op": "job_start", "kind": "fm_demod", "source": "tone",
        "center_hz": 100_000_000, "bandwidth_hz": 12_500,
        "deviation_hz": null, "sideband": null, "audio_policy": "loudest",
        "elastic": false, "replace": [],
    }));
    assert!(bad.is_err());
    // Both are required and nullable in the published schema.
    let schema = zeta_core::ops::schema();
    let variants = schema["op"]["oneOf"]
        .as_array()
        .or(schema["op"]["anyOf"].as_array())
        .expect("op variants");
    let variant = |op: &str| {
        variants
            .iter()
            .find(|v| v["properties"]["op"]["const"] == op)
            .unwrap_or_else(|| panic!("{op} variant"))
            .clone()
    };
    for (op, field, want) in [
        (
            "job_start",
            "audio_policy",
            json!(["first_active", "follow", "all", null]),
        ),
        ("job_attach", "stream", Value::Null),
    ] {
        let v = variant(op);
        assert!(
            v["required"].as_array().unwrap().contains(&field.into()),
            "{op}.{field}: {v}"
        );
        let p = &v["properties"][field];
        assert_eq!(p["type"], json!(["string", "null"]), "{op}.{field}");
        assert!(p.get("default").is_none(), "{op}.{field}: {p}");
        if !want.is_null() {
            assert_eq!(p["enum"], want, "{op}.{field}");
        }
    }
}
