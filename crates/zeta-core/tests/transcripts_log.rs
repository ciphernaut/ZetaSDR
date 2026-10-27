//! The transcript log: the node's tee writes every `transcript` event of a `transcribe`
//! job to `<state_dir>/transcripts.log` with the job, its input, the source and the
//! channel's tuning. The real `denoise`, `vad` and `transcribe` stages live in other
//! crates; inline stand-ins take their kinds here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::graph::{Edge, Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::{JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{Hz, JobId, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// What the emitter publishes on its first input frame, then it idles.
#[derive(Clone, Copy)]
enum Emit {
    /// A text event, a dropped one, an error one, and a `vad`-like non-transcript one.
    Three,
    /// `n` text events in one burst, with no await between them.
    Burst(usize),
}

/// The span of the `i`th event.
fn span(i: u64) -> Span {
    Span {
        start_ns: Timestamp(1_000_000 + i * 10),
        end_ns: Timestamp(1_000_005 + i * 10),
    }
}

fn events(emit: Emit) -> Vec<serde_json::Value> {
    let with_span = |i: u64, mut v: serde_json::Value| {
        let s = span(i);
        v["start_ns"] = s.start_ns.0.into();
        v["end_ns"] = s.end_ns.0.into();
        v
    };
    match emit {
        Emit::Three => vec![
            with_span(
                0,
                serde_json::json!({"protocol": "transcript", "text": "hello", "language": "en",
                    "backend": "stand-in", "duration_ms": 5}),
            ),
            with_span(1, serde_json::json!({"protocol": "vad", "speech": true})),
            with_span(
                1,
                serde_json::json!({"protocol": "transcript", "dropped": true}),
            ),
            with_span(
                2,
                serde_json::json!({"protocol": "transcript", "error": "backend: boom"}),
            ),
        ],
        Emit::Burst(n) => (0..n as u64)
            .map(|i| {
                with_span(
                    i,
                    serde_json::json!({"protocol": "transcript", "text": format!("t{i}")}),
                )
            })
            .collect(),
    }
}

/// A stage that, on its first input frame, publishes [`events`] on its one event
/// output (each frame carrying the event's span), then drains its input until cancelled.
struct Emitter {
    spec: StageSpec,
    emit: Emit,
    /// Set once the events are published.
    sent: Arc<AtomicBool>,
}

#[async_trait]
impl Stage for Emitter {
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
        let (Some(rx), Some(out)) = (inputs.first_mut(), outputs.first()) else {
            return Err(StageError::Failed("expected 1 input, 1 output".into()));
        };
        let mut sent = false;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) if !sent => {
                        sent = true;
                        for v in events(self.emit) {
                            let mut prov = frame.provenance.clone();
                            prov.span = Span {
                                start_ns: Timestamp(v["start_ns"].as_u64().unwrap()),
                                end_ns: Timestamp(v["end_ns"].as_u64().unwrap()),
                            };
                            bus.publish(Frame::new(out.clone(), prov, Payload::Event(v)));
                        }
                        self.sent.store(true, Ordering::SeqCst);
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

/// A stage that maps every input frame to one `payload` on its first output.
struct MapStage {
    spec: StageSpec,
    audio_out: bool,
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
                        let payload = match self.audio_out {
                            true => Payload::Audio {
                                sample_rate_hz: Hz(48_000),
                                channels: 1,
                                samples: Arc::from(vec![0.1f32; 480]),
                            },
                            false => frame.payload.clone(),
                        };
                        if let Some(out) = outputs.first() {
                            bus.publish(Frame::new(out.clone(), frame.provenance.clone(), payload));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

fn spec(kind: &'static str, input: StreamKind, outputs: &[StreamKind]) -> StageSpec {
    StageSpec {
        kind,
        inputs: vec![input],
        outputs: outputs.to_vec(),
        params_schema: schemars::json_schema!({"type": "object"}),
    }
}

/// The attached chain's stand-ins; the emitter is registered as `emitter_kind`.
fn registry(emitter_kind: &'static str, emit: Emit, sent: Arc<AtomicBool>) -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(StreamInStage::new()));
    // The input job's demodulator stand-in: one audio frame per IQ block.
    r.register(Arc::new(MapStage {
        spec: spec("audio_src", StreamKind::Iq, &[StreamKind::Audio]),
        audio_out: true,
    }));
    r.register(Arc::new(MapStage {
        spec: spec("denoise", StreamKind::Audio, &[StreamKind::Audio]),
        audio_out: false,
    }));
    // `vad` has an event output the job does not consume: the tee reads it too and
    // must keep only `transcript` events (the emitter's `vad` event is on its own output).
    r.register(Arc::new(MapStage {
        spec: spec(
            "vad",
            StreamKind::Audio,
            &[StreamKind::Audio, StreamKind::Event],
        ),
        audio_out: false,
    }));
    r.register(Arc::new(Emitter {
        spec: spec(emitter_kind, StreamKind::Audio, &[StreamKind::Event]),
        emit,
        sent,
    }));
    r
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
    NodeConfig {
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

fn log_path(dir: &Path) -> PathBuf {
    dir.join("state").join("transcripts.log")
}

/// A graph of `kinds` in a chain, the first reading the job's IQ.
fn chain(kinds: &[&str]) -> Graph {
    let nodes: Vec<_> = kinds
        .iter()
        .map(|k| GraphNode {
            id: NodeId::new(),
            kind: (*k).into(),
            params: serde_json::json!({}),
        })
        .collect();
    let edges = nodes
        .windows(2)
        .map(|w| Edge {
            from: w[0].id.clone(),
            to: w[1].id.clone(),
            kind: StreamKind::Audio,
        })
        .collect();
    Graph {
        id: GraphId::new(),
        nodes,
        edges,
    }
}

/// A node on the tone with an audio job running and a `transcribe` job attached to it.
async fn attached(dir: &Path, emit: Emit) -> (Node, JobId, JobId) {
    let (src, port) = port(dir);
    let node = Node::open(
        cfg(dir),
        registry("transcribe", emit, Arc::default()),
        vec![port],
    )
    .await
    .unwrap();
    src.start();
    let input = node
        .job_start(
            "audio",
            tone(),
            tuning(),
            chain(&["audio_src"]),
            None,
            false,
        )
        .await
        .unwrap();
    let id = match node
        .apply(Op::JobAttach {
            kind: JobKind::Transcribe,
            job: input.clone(),
            language: None,
        })
        .await
    {
        Ok(OpResult::Job(id)) => id,
        other => panic!("attach: {other:?}"),
    };
    (node, input, id)
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Poll until `f` holds or `ms` pass; whether it held.
async fn until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        if f() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn every_transcript_event_is_one_line_with_the_jobs_tuning() {
    let dir = tempfile::tempdir().unwrap();
    let (node, input, id) = attached(dir.path(), Emit::Three).await;
    let path = log_path(dir.path());
    assert!(
        until(2_000, || lines(&path).len() >= 3).await,
        "3 lines within 2 s: {:?}",
        lines(&path)
    );
    // Nothing more comes: the `vad` events are not logged.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let got = lines(&path);
    assert_eq!(got.len(), 3, "{got:?}");

    let keys = [
        "t_ns",
        "job",
        "input",
        "source",
        "center_hz",
        "bandwidth_hz",
        "start_ns",
        "end_ns",
        "text",
        "language",
        "dropped",
        "error",
    ];
    let inrec = record(&node, &input);
    assert_eq!(record(&node, &id).tuning, inrec.tuning);
    for (i, l) in got.iter().enumerate() {
        let m = l.as_object().unwrap();
        let mut have: Vec<_> = m.keys().map(String::as_str).collect();
        have.sort_unstable();
        let mut want = keys.to_vec();
        want.sort_unstable();
        assert_eq!(have, want, "line {i}");
        assert!(l["t_ns"].as_u64().unwrap() > 0);
        assert_eq!(l["job"], id.0);
        assert_eq!(l["input"], input.0);
        assert_eq!(l["source"], "tone");
        assert_eq!(l["center_hz"], inrec.tuning.center_hz.0);
        assert_eq!(l["bandwidth_hz"], inrec.tuning.bandwidth_hz.0);
        let s = span(i as u64);
        assert_eq!(l["start_ns"], s.start_ns.0, "line {i}");
        assert_eq!(l["end_ns"], s.end_ns.0, "line {i}");
    }
    let null = serde_json::Value::Null;
    assert_eq!(got[0]["text"], "hello");
    assert_eq!(got[0]["language"], "en");
    assert_eq!(got[0]["dropped"], false);
    assert_eq!(got[0]["error"], null);
    assert_eq!(got[1]["text"], null);
    assert_eq!(got[1]["language"], null);
    assert_eq!(got[1]["dropped"], true);
    assert_eq!(got[1]["error"], null);
    assert_eq!(got[2]["text"], null);
    assert_eq!(got[2]["dropped"], false);
    assert_eq!(got[2]["error"], "backend: boom");
    assert_eq!(node.persist_errors(), Vec::<String>::new());
}

#[tokio::test]
async fn non_transcript_events_and_other_kinds_are_not_logged() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(dir.path());
    let sent = Arc::new(AtomicBool::new(false));
    let node = Node::open(
        cfg(dir.path()),
        registry("measure", Emit::Three, sent.clone()),
        vec![port],
    )
    .await
    .unwrap();
    src.start();
    // Job kind `audio` (a built-in kind such as `measure` would be expanded for
    // the radio); the emitter runs under stage kind `measure`.
    node.job_start(
        "audio",
        tone(),
        tuning(),
        chain(&["audio_src", "measure"]),
        None,
        false,
    )
    .await
    .unwrap();
    assert!(
        until(2_000, || sent.load(Ordering::SeqCst)).await,
        "the stage published its transcript events"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!log_path(dir.path()).exists());
    assert_eq!(node.debug_tee_count(), 0);
}

#[tokio::test]
async fn tee_ends_with_the_job_and_a_lag_is_recorded_once() {
    let dir = tempfile::tempdir().unwrap();
    let (node, _input, id) = attached(dir.path(), Emit::Three).await;
    // One tee per event output of the attached job: `vad`'s and the emitter's.
    assert_eq!(node.debug_tee_count(), 2);
    node.job_stop(&id).await.unwrap();
    assert!(
        until(1_000, || node.debug_tee_count() == 0).await,
        "tees left: {}",
        node.debug_tee_count()
    );

    let dir = tempfile::tempdir().unwrap();
    let (node, _input, id) = attached(dir.path(), Emit::Burst(600)).await;
    let path = log_path(dir.path());
    assert!(until(2_000, || !node.persist_errors().is_empty()).await);
    // The tee goes on after the lag: what was still buffered is written.
    assert!(until(2_000, || !lines(&path).is_empty()).await);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let errors = node.persist_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    let prefix = format!("transcripts.log: job {}: ", id.0);
    assert!(
        errors[0].starts_with(&prefix) && errors[0].ends_with(" events skipped"),
        "{errors:?}"
    );
    assert!(lines(&path).len() < 600);
}

#[tokio::test]
async fn unwritable_transcripts_log_does_not_fail_the_job() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(log_path(dir.path())).unwrap();
    let (node, _input, id) = attached(dir.path(), Emit::Three).await;
    assert!(
        until(2_000, || !node.persist_errors().is_empty()).await,
        "a write failure is recorded"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let errors = node.persist_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("transcripts.log: "), "{errors:?}");
    assert_eq!(record(&node, &id).state, JobState::Running);
}
