//! The `ops` event stream: every `Node::apply` is logged after it completes.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use zeta_core::node::{NODE_SOURCE, Node, NodeConfig, OPS_RESULT_LIMIT_BYTES};
use zeta_core::ops::JobKind;
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Hz, JobId, SourceId, StreamKind};
use zeta_source_file::{FileOpts, FileSource, fixtures};

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

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

async fn open(dir: &Path) -> Node {
    open_with(dir, None, false).await
}

/// `run` starts the file source (its samples flow) once the node is open.
async fn open_with(dir: &Path, tune_range: Option<(Hz, Hz)>, run: bool) -> Node {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
        tune_range,
    };
    let file = Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    let src: Arc<dyn Port> = file.clone();
    let node = Node::open(cfg(dir), registry(), vec![src]).await.unwrap();
    if run {
        file.start();
    }
    node
}

async fn next(rx: &mut tokio::sync::broadcast::Receiver<Frame>) -> (Frame, Value) {
    let f = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("ops frame within 5 s")
        .expect("ops stream open");
    let Payload::Event(v) = f.payload.clone() else {
        panic!("ops frame is not an Event: {:?}", f.payload);
    };
    (f, v)
}

#[test]
fn frame_summary_for_video_carries_dimensions() {
    use zeta_core::ops::FrameSummary;
    use zeta_core::stream::VideoFormat;
    use zeta_core::types::{Provenance, Span, StreamId, Timestamp};

    let frame = Frame::new(
        StreamId("s1".into()),
        Provenance {
            source: SourceId("file0".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(1),
                end_ns: Timestamp(2),
            },
        },
        Payload::Video {
            width: 4,
            height: 2,
            format: VideoFormat::Rgb8,
            frame: Arc::from(vec![0u8; 24]),
        },
    );
    let s = FrameSummary::from_frame(&frame);
    assert_eq!(s.kind, StreamKind::Video);
    assert_eq!(
        s.payload,
        serde_json::json!({"width": 4, "height": 2, "format": "rgb8", "bytes": 24})
    );
}

#[tokio::test]
async fn every_apply_is_logged_with_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut rx = node.bus().subscribe(&node.ops_stream()).unwrap();

    node.apply(Op::SourceList {}).await.unwrap();
    node.apply(Op::JobStop {
        job: JobId("nope".into()),
    })
    .await
    .unwrap_err();

    let (f0, p0) = next(&mut rx).await;
    assert_eq!(f0.kind, StreamKind::Event);
    assert_eq!(f0.stream, node.ops_stream());
    assert_eq!(f0.provenance.source, SourceId(NODE_SOURCE.into()));
    assert_eq!(f0.provenance.job, None);
    assert_eq!(f0.provenance.channel, None);
    assert_eq!(p0["op"]["op"], "source_list");
    assert_eq!(p0["outcome"], "ok");
    assert_eq!(p0["result"]["result"], "sources");
    assert!(p0.get("error").is_none());
    assert!(p0["elapsed_ms"].is_u64());
    assert_eq!(p0["transport"], "local");

    let (f1, p1) = next(&mut rx).await;
    assert_eq!(f1.provenance.source, SourceId("node".into()));
    assert_eq!(p1["op"]["op"], "job_stop");
    assert_eq!(p1["op"]["job"], "nope");
    assert_eq!(p1["outcome"], "error");
    assert_eq!(p1["error"]["error"], "unknown");
    assert!(p1.get("result").is_none());
    assert!(p1["elapsed_ms"].is_u64());
    assert_eq!(p1["transport"], "local");
}

#[tokio::test]
async fn apply_from_tags_the_record_with_its_transport() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut rx = node.bus().subscribe(&node.ops_stream()).unwrap();

    node.apply_from(Op::SourceList {}, "ui").await.unwrap();
    node.apply_from(
        Op::JobStop {
            job: JobId("nope".into()),
        },
        "agent",
    )
    .await
    .unwrap_err();
    node.apply(Op::SourceList {}).await.unwrap();

    let (_, p) = next(&mut rx).await;
    assert_eq!(p["transport"], "ui");
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["outcome"], "error");
    assert_eq!(p["transport"], "agent");
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["transport"], "local");
}

#[tokio::test]
async fn large_results_are_summarised_on_the_ops_stream() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut rx = node.bus().subscribe(&node.ops_stream()).unwrap();

    let schema = node.apply(Op::Schema {}).await.unwrap();
    let full = serde_json::to_vec(&schema).unwrap().len();
    assert!(full > OPS_RESULT_LIMIT_BYTES);
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["op"]["op"], "schema");
    assert_eq!(
        p["result"],
        serde_json::json!({"result": "schema", "truncated": true, "bytes": full})
    );

    let sources = node.apply(Op::SourceList {}).await.unwrap();
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["result"], serde_json::to_value(&sources).unwrap());
    assert!(p["result"].get("truncated").is_none());
}

#[tokio::test]
async fn large_errors_are_summarised_on_the_ops_stream() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut rx = node.bus().subscribe(&node.ops_stream()).unwrap();

    let err = node
        .apply(Op::JobStop {
            job: JobId("j".repeat(20_000)),
        })
        .await
        .unwrap_err();
    let full = serde_json::to_vec(&err).unwrap().len();
    assert!(full > OPS_RESULT_LIMIT_BYTES);
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["outcome"], "error");
    assert_eq!(
        p["error"],
        serde_json::json!({"error": err.name(), "truncated": true, "bytes": full})
    );

    let err = node
        .apply(Op::JobStop {
            job: JobId("nope".into()),
        })
        .await
        .unwrap_err();
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["error"], serde_json::to_value(&err).unwrap());
    assert!(p["error"].get("truncated").is_none());
}

#[test]
fn op_error_name_matches_the_serialised_tag() {
    use zeta_core::graph::GraphError;
    use zeta_core::ops::OpError;
    use zeta_core::port::Tuning;
    use zeta_core::types::Hz;
    // Every OpError variant: extend this list whenever the enum grows.
    let all = vec![
        OpError::OutOfRange {
            field: "f".into(),
            value: 1,
            min: 0,
            max: 0,
            why: None,
            suggested: None,
        },
        OpError::Conflict {
            source: SourceId("s".into()),
            holders: vec![],
            current: Tuning {
                center_hz: Hz(1),
                bandwidth_hz: Hz(1),
            },
            options: vec![],
        },
        OpError::Unknown {
            field: "f".into(),
            value: "v".into(),
            known: vec![],
        },
        OpError::Ring {
            detail: zeta_core::ring::RingError::Empty,
        },
        OpError::Graph {
            detail: GraphError::Empty,
        },
        OpError::Other {
            message: "m".into(),
        },
    ];
    for e in &all {
        let v = serde_json::to_value(e).unwrap();
        assert_eq!(v["error"], e.name(), "{e:?}");
    }
}

#[tokio::test]
async fn ops_stream_is_listed_with_node_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let OpResult::Streams(streams) = node.apply(Op::StreamList {}).await.unwrap() else {
        panic!("stream_list answers Streams");
    };
    let node_streams: Vec<_> = streams
        .iter()
        .filter(|s| s.provenance.source.0 == NODE_SOURCE)
        .collect();
    assert_eq!(node_streams.len(), 1, "{streams:?}");
    assert_eq!(node_streams[0].kind, StreamKind::Event);
    assert_eq!(node_streams[0].id, node.ops_stream());
    assert_eq!(node_streams[0].provenance.job, None);
    assert_eq!(node_streams[0].provenance.channel, None);
}

async fn list(node: &Node) -> Vec<zeta_core::stream::StreamInfo> {
    let OpResult::Streams(s) = node.apply(Op::StreamList {}).await.unwrap() else {
        panic!("stream_list answers Streams");
    };
    s
}

fn feeds_of(
    streams: &[zeta_core::stream::StreamInfo],
    source: &str,
) -> Vec<zeta_core::types::StreamId> {
    streams
        .iter()
        .filter(|s| s.provenance.source.0 == source && s.provenance.job.is_none())
        .map(|s| {
            assert_eq!(s.kind, StreamKind::Event);
            s.id.clone()
        })
        .collect()
}

#[tokio::test]
async fn stream_list_has_one_feed_per_source_and_a_job_adds_none() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let before = feeds_of(&list(&node).await, "file0");
    assert_eq!(before.len(), 1);
    assert_eq!(feeds_of(&list(&node).await, NODE_SOURCE).len(), 1);
    started(&node, job_start(JobKind::Measure, 48_000)).await;
    assert_eq!(feeds_of(&list(&node).await, "file0"), before);
}

#[tokio::test]
async fn a_port_added_after_open_has_a_feed() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    std::fs::create_dir_all(dir.path().join("second")).unwrap();
    let meta = fixtures::write_tone(&dir.path().join("second"));
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let second = Arc::new(FileSource::open(SourceId("second".into()), &meta, opts).unwrap());
    node.add_port(second).unwrap();
    let feed = node.bus().feed_of(&SourceId("second".into()));
    assert!(feed.is_some());
    let OpResult::Streams(s) = node.apply(Op::StreamList {}).await.unwrap() else {
        panic!()
    };
    assert_eq!(feeds_of(&s, "second"), vec![feed.unwrap()]);
}

#[tokio::test]
async fn a_tail_of_the_ops_stream_never_sees_its_own_record() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut rx = node.bus().subscribe(&node.ops_stream()).unwrap();
    let OpResult::Frames { frames, .. } = node
        .apply(Op::StreamTail {
            stream: node.ops_stream(),
            max_frames: 1,
            timeout_ms: 100,
        })
        .await
        .unwrap()
    else {
        panic!("stream_tail answers Frames");
    };
    assert!(frames.is_empty(), "{frames:?}");
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["op"]["op"], "stream_tail");
    assert_eq!(p["result"]["result"], "frames");
}

#[test]
fn variant_name_matches_the_serialised_tag() {
    use zeta_core::graph::{Graph, GraphId};
    use zeta_core::port::Tuning;
    use zeta_core::types::Hz;
    let tuning = Tuning {
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(2_000_000),
    };
    // Every OpResult variant: extend this list whenever the enum grows.
    let all = vec![
        OpResult::Sources(vec![]),
        OpResult::Tuning(tuning),
        OpResult::Job(JobId("j".into())),
        OpResult::Jobs(vec![]),
        OpResult::Context(serde_json::json!({})),
        OpResult::Iq {
            sample_rate_hz: Hz(1),
            center_hz: Hz(1),
            bandwidth_hz: Hz(1),
            samples: 0,
            path: "x.sigmf-meta".into(),
        },
        OpResult::Streams(vec![]),
        OpResult::Frames {
            frames: vec![],
            missed: 0,
        },
        OpResult::Graph(Graph {
            id: GraphId("g".into()),
            nodes: vec![],
            edges: vec![],
        }),
        OpResult::Knowledge {
            allocations: vec![],
            signatures: vec![],
            reach: None,
        },
        OpResult::Classification {
            labels: vec![],
            tried: vec![],
            allocations: vec![],
            origins: None,
        },
        OpResult::Ok,
        OpResult::Schema(serde_json::json!({})),
    ];
    for r in &all {
        let v = serde_json::to_value(r).unwrap();
        assert_eq!(v["result"], r.variant_name(), "{r:?}");
    }
}

#[tokio::test]
async fn large_ops_are_summarised_on_the_ops_stream() {
    use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut rx = node.bus().subscribe(&node.ops_stream()).unwrap();

    let big = "x".repeat(1024);
    let op = Op::GraphPut {
        job: JobId("nope".into()),
        graph: Graph {
            id: GraphId("g".into()),
            nodes: (0..16)
                .map(|i| GraphNode {
                    id: NodeId(format!("n{i}")),
                    kind: "passthrough".into(),
                    params: serde_json::json!({ "pad": big }),
                })
                .collect(),
            edges: vec![],
        },
    };
    let full = serde_json::to_vec(&op).unwrap().len();
    assert!(full > OPS_RESULT_LIMIT_BYTES);
    // Fails (unknown job); the record is logged either way.
    let _ = node.apply(op).await;
    let (_, p) = next(&mut rx).await;
    assert_eq!(
        p["op"],
        serde_json::json!({"op": "graph_put", "truncated": true, "bytes": full})
    );

    node.apply(Op::SourceList {}).await.unwrap();
    let (_, p) = next(&mut rx).await;
    assert_eq!(p["op"], serde_json::json!({"op": "source_list"}));
}

#[test]
fn op_name_matches_the_serialised_tag() {
    use zeta_core::graph::{Graph, GraphId};
    use zeta_core::ops::JobKind;
    use zeta_core::types::{Hz, Selection, Span, StreamId, Timestamp};
    let sel = Selection {
        source: SourceId("s".into()),
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(1),
        },
        center_hz: Hz(1),
        bandwidth_hz: Hz(1),
    };
    // Every Op variant: extend this list whenever the enum grows.
    let all = vec![
        Op::SourceList {},
        Op::SourceEnsure {
            source: SourceId("s".into()),
            center_hz: Hz(1),
            bandwidth_hz: Hz(1),
            replace: vec![],
        },
        Op::JobStart {
            kind: JobKind::Spectrum,
            source: SourceId("s".into()),
            center_hz: Hz(1),
            bandwidth_hz: Hz(1),
            deviation_hz: None,
            sideband: None,
            dwell_ms: None,
            average: None,
            elastic: false,
            replace: vec![],
        },
        Op::JobRetune {
            job: JobId("j".into()),
            center_hz: Hz(1),
            bandwidth_hz: Hz(1),
            replace: vec![],
        },
        Op::JobList {},
        Op::JobStop {
            job: JobId("j".into()),
        },
        Op::JobContext {
            job: JobId("j".into()),
        },
        Op::IqRead {
            selection: sel.clone(),
        },
        Op::StreamList {},
        Op::StreamTail {
            stream: StreamId("x".into()),
            max_frames: 1,
            timeout_ms: 1,
        },
        Op::GraphGet {
            job: JobId("j".into()),
        },
        Op::GraphPut {
            job: JobId("j".into()),
            graph: Graph {
                id: GraphId("g".into()),
                nodes: vec![],
                edges: vec![],
            },
        },
        Op::KnowledgeLookup {
            region: "AU".into(),
            center_hz: Hz(1),
            bandwidth_hz: Hz(1),
        },
        Op::Classify { selection: sel },
        Op::Schema {},
    ];
    for op in &all {
        let v = serde_json::to_value(op).unwrap();
        assert_eq!(v["op"], op.name(), "{op:?}");
    }
}

#[tokio::test]
async fn unwritable_ops_log_does_not_fail_ops() {
    let dir = tempfile::tempdir().unwrap();
    // A directory where the log file belongs: every append fails.
    std::fs::create_dir_all(dir.path().join("state").join("ops.log")).unwrap();
    let node = open(dir.path()).await;

    node.apply(Op::SourceList {}).await.unwrap();
    node.apply(Op::SourceList {}).await.unwrap();

    let errors = node.persist_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("ops.log"), "{errors:?}");
}

/// The lines of the state directory's `ops.log`, as JSON.
fn log_lines(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("state").join("ops.log"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn read_only_ops_are_logged_without_their_result() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    node.apply(Op::JobList {}).await.unwrap();
    node.apply(Op::ThemesList {}).await.unwrap();
    let id = match node
        .apply(Op::JobStart {
            kind: zeta_core::ops::JobKind::Passthrough,
            source: SourceId("file0".into()),
            center_hz: zeta_core::types::Hz(100_000_000),
            bandwidth_hz: zeta_core::types::Hz(48_000),
            deviation_hz: None,
            sideband: None,
            dwell_ms: None,
            average: None,
            elastic: false,
            replace: vec![],
        })
        .await
        .unwrap()
    {
        OpResult::Job(id) => id,
        r => panic!("job_start: {r:?}"),
    };
    node.apply(Op::JobContext {
        job: JobId("nope".into()),
    })
    .await
    .unwrap_err();
    node.apply(Op::JobStop { job: id }).await.unwrap();

    let lines = log_lines(dir.path());
    let ops: Vec<&str> = lines
        .iter()
        .map(|l| l["op"]["op"].as_str().unwrap())
        .collect();
    assert_eq!(
        ops,
        [
            "job_list",
            "themes_list",
            "job_start",
            "job_context",
            "job_stop"
        ]
    );
    // A successful read: op, outcome, transport and time, no result.
    assert!(lines[0].get("result").is_none(), "{}", lines[0]);
    assert_eq!(lines[0]["outcome"], "ok");
    assert_eq!(lines[0]["transport"], "local");
    assert!(lines[0]["t_ns"].is_u64());
    // A mutating op keeps its result; a failed read keeps its error.
    assert!(lines[1].get("result").is_none(), "{}", lines[1]);
    assert_eq!(lines[2]["result"]["result"], "job");
    assert_eq!(lines[3]["outcome"], "error");
    assert_eq!(lines[3]["error"]["error"], "unknown");
    assert!(lines[4].get("result").is_some(), "{}", lines[4]);
}

fn job_start(kind: JobKind, bandwidth_hz: u64) -> Op {
    Op::JobStart {
        kind,
        source: SourceId("file0".into()),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(bandwidth_hz),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        elastic: false,
        replace: vec![],
    }
}

async fn started(node: &Node, op: Op) -> JobId {
    match node.apply(op).await.unwrap() {
        OpResult::Job(id) => id,
        other => panic!("expected a job, got {other:?}"),
    }
}

/// Every event frame the feed of `file0` carries for `secs`.
async fn feed_for(node: &Node, secs: u64) -> Vec<(Frame, Value)> {
    let feed = node.bus().feed_of(&SourceId("file0".into())).expect("feed");
    let mut rx = node.bus().subscribe(&feed).unwrap();
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut out = Vec::new();
    while let Ok(r) = tokio::time::timeout_at(end, rx.recv()).await {
        match r {
            Ok(f) => {
                if let Payload::Event(ref v) = f.payload {
                    let v = v.clone();
                    out.push((f, v));
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spectrum_jobs_rows_never_reach_the_feed_but_a_measure_event_does() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_with(dir.path(), None, true).await;
    let spectrum = started(&node, job_start(JobKind::Spectrum, 48_000)).await;
    let measure = started(&node, job_start(JobKind::Measure, 48_000)).await;
    let frames = feed_for(&node, 2).await;
    assert!(
        frames
            .iter()
            .any(|(f, v)| f.provenance.job.as_ref() == Some(&measure)
                && v.get("label").is_some()
                && v.get("center_hz").is_some()),
        "no measure event in the feed: {} frames",
        frames.len()
    );
    for (f, v) in &frames {
        assert!(v.get("q").is_none() && v.get("db").is_none(), "{v}");
        assert_ne!(f.provenance.job.as_ref(), Some(&spectrum));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scans_rows_never_reach_the_feed() {
    let dir = tempfile::tempdir().unwrap();
    let node = open_with(dir.path(), Some((Hz(90_000_000), Hz(110_000_000))), true).await;
    let scan = started(&node, job_start(JobKind::Scan, 48_000)).await;
    let frames = feed_for(&node, 2).await;
    for (f, v) in &frames {
        assert_ne!(f.provenance.job.as_ref(), Some(&scan), "{v}");
    }
}
