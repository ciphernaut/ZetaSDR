//! Resume on the real chain: a built-in graph persisted for one capture rate is
//! re-expanded for the rate the source actually runs at; a `job_graph_put` graph is not.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zeta_core::graph::Graph;
use zeta_core::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId, Timestamp};
use zeta_source_file::{FileOpts, FileSource};

const CENTER_HZ: u64 = 104_700_000;
/// Narrow enough for a 160 kHz channel at 480 000 Hz (a divisor of it) that does not
/// divide 240 000 Hz.
const BANDWIDTH_HZ: u64 = 160_000;

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
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
    }
}

/// A looping, real-time file source `speech` of a 1 kHz tone, wideband FM at `rate_hz`.
fn wfm_port(dir: &Path, rate_hz: u32) -> (Arc<FileSource>, Arc<dyn Port>) {
    let audio = zeta_dsp::fixtures::tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = zeta_dsp::fixtures::wfm_modulate(&audio, 48_000, rate_hz, 75_000.0, 0.0, true);
    let meta =
        zeta_dsp::fixtures::write_fixture(dir, &format!("wfm{rate_hz}"), &iq, rate_hz, CENTER_HZ);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
    };
    let src = Arc::new(FileSource::open(SourceId("speech".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

fn tuning() -> Tuning {
    Tuning {
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
    }
}

fn on_disk(dir: &Path, id: &JobId) -> JobRecord {
    let p = dir
        .join("state")
        .join("jobs")
        .join(format!("{}.json", id.0));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn record(node: &Node, id: &JobId) -> JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
}

fn out_rate(graph: &Graph) -> u64 {
    let ch = graph.nodes.iter().find(|n| n.kind == "channel").unwrap();
    ch.params["out_rate_hz"].as_u64().unwrap()
}

#[tokio::test]
async fn resume_reexpands_a_builtin_graph_for_the_actual_rate() {
    let dir = tempfile::tempdir().unwrap();
    let source = SourceId("speech".into());
    let graph = JobKind::WfmDemod
        .expand(&source, tuning(), Hz(480_000), None, None, None)
        .unwrap();
    assert_eq!(out_rate(&graph), 160_000);
    assert_ne!(
        240_000 % out_rate(&graph),
        0,
        "the stored graph cannot run at 240 kHz"
    );
    let rec = JobRecord {
        format: JOB_RECORD_FORMAT,
        id: JobId::new(),
        kind: "wfm_demod".into(),
        graph,
        source,
        tuning: tuning(),
        state: JobState::Running,
        context: serde_json::json!({}),
        created_ns: Timestamp(1),
        input: None,
        capture_rate_hz: Some(480_000),
    };
    let jobs = dir.path().join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join(format!("{}.json", rec.id.0)),
        serde_json::to_vec(&rec).unwrap(),
    )
    .unwrap();

    let (src, port) = wfm_port(dir.path(), 240_000);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    // Long enough for the channel stage to have seen blocks: a graph it cannot run fails.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(record(&node, &rec.id).state, JobState::Running);
    let disk = on_disk(dir.path(), &rec.id);
    assert_eq!(disk.capture_rate_hz, Some(240_000));
    assert_eq!(240_000 % out_rate(&disk.graph), 0, "{:?}", disk.graph);
    assert_eq!(disk.graph, record(&node, &rec.id).graph);
    // The demodulator keeps its parameters.
    let demod = |g: &Graph| {
        g.nodes
            .iter()
            .find(|n| n.kind == "wfm_demod")
            .unwrap()
            .params
            .clone()
    };
    assert_eq!(demod(&disk.graph), demod(&rec.graph));
}

#[tokio::test]
async fn resume_leaves_a_graph_put_graph_alone() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let (src, port) = wfm_port(dir.path(), 240_000);
        let node = Node::open(cfg(dir.path()), registry(), vec![port])
            .await
            .unwrap();
        src.start();
        let op = Op::JobStart {
            kind: JobKind::WfmDemod,
            source: SourceId("speech".into()),
            center_hz: Hz(CENTER_HZ),
            bandwidth_hz: Hz(BANDWIDTH_HZ),
            deviation_hz: None,
            sideband: None,
        };
        let id = match node.apply(op).await.unwrap() {
            OpResult::Job(id) => id,
            r => panic!("job_start: {r:?}"),
        };
        assert_eq!(on_disk(dir.path(), &id).capture_rate_hz, Some(240_000));
        let graph = record(&node, &id).graph;
        node.apply(Op::GraphPut {
            job: id.clone(),
            graph,
        })
        .await
        .unwrap();
        assert_eq!(on_disk(dir.path(), &id).capture_rate_hz, None);
        id
    };
    let put = on_disk(dir.path(), &id);
    assert_eq!(put.state, JobState::Running);

    let (src, port) = wfm_port(dir.path(), 480_000);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let disk = on_disk(dir.path(), &id);
    assert_eq!(disk.graph, put.graph, "launched as stored");
    assert_eq!(disk.capture_rate_hz, None);
    assert_eq!(record(&node, &id).graph, put.graph);
}
