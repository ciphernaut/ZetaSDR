//! The `ops` event stream: every `Node::apply` is logged after it completes.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use zeta_core::node::{NODE_SOURCE, Node, NodeConfig, OPS_RESULT_LIMIT_BYTES};
use zeta_core::ops::{Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{JobId, SourceId, StreamKind};
use zeta_source_file::{FileOpts, FileSource, fixtures};

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        backends: vec![Backend::Cpu],
        resume: true,
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r
}

async fn open(dir: &Path) -> Node {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: false,
        block_samples: 4800,
        repeat: true,
    };
    let src: Arc<dyn Port> =
        Arc::new(FileSource::open(SourceId("file0".into()), &meta, opts).unwrap());
    Node::open(cfg(dir), registry(), vec![src]).await.unwrap()
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
        },
        OpResult::Labels(vec![]),
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
        },
        Op::JobStart {
            kind: JobKind::Spectrum,
            source: SourceId("s".into()),
            center_hz: Hz(1),
            bandwidth_hz: Hz(1),
            deviation_hz: None,
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
