//! Resume on the real chain: a built-in graph persisted for one capture rate is
//! re-expanded for the rate the source actually runs at; a `job_graph_put` graph is not,
//! unless the put changed only a demodulator's carried params.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use zeta_core::graph::{Edge, Graph, Node as GraphNode, NodeId};
use zeta_core::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId, StreamKind, Timestamp};
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
        bank_factory: None,
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
        tune_range: None,
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

fn nbfm(graph: &Graph) -> serde_json::Value {
    graph
        .nodes
        .iter()
        .find(|n| n.kind == "nbfm_demod")
        .unwrap()
        .params
        .clone()
}

/// Start a `kind` job on `speech` at [`tuning`].
async fn start(node: &Node, kind: JobKind) -> JobId {
    let op = Op::JobStart {
        kind,
        source: SourceId("speech".into()),
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await.unwrap() {
        OpResult::Job(id) => id,
        r => panic!("job_start: {r:?}"),
    }
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
        elastic: false,
        operator_graph: false,
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

/// A record written before `capture_rate_hz` existed (an upgrade) carries no rate, but its
/// graph was still expanded by the node: it is re-expanded for the actual rate, not failed.
#[tokio::test]
async fn resume_reexpands_a_record_written_before_the_rate_was_kept() {
    let dir = tempfile::tempdir().unwrap();
    let source = SourceId("speech".into());
    let graph = JobKind::WfmDemod
        .expand(&source, tuning(), Hz(480_000), None, None, None)
        .unwrap();
    assert_ne!(240_000 % out_rate(&graph), 0);
    let id = JobId::new();
    let legacy = serde_json::json!({
        "format": 1,
        "id": id.0,
        "kind": "wfm_demod",
        "graph": graph,
        "source": source,
        "tuning": tuning(),
        "state": {"status": "running"},
        "context": {},
        "created_ns": 1,
        "input": null
    });
    let jobs = dir.path().join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join(format!("{}.json", id.0)),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();

    let (src, port) = wfm_port(dir.path(), 240_000);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(record(&node, &id).state, JobState::Running);
    let disk = on_disk(dir.path(), &id);
    assert_eq!(disk.capture_rate_hz, Some(240_000));
    assert_eq!(240_000 % out_rate(&disk.graph), 0, "{:?}", disk.graph);
}

/// A `graph_put` that changes the topology (a node added) is the operator's graph: it is
/// launched as stored, never re-expanded.
#[tokio::test]
async fn graph_put_that_changes_the_topology_is_an_operator_graph() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let (src, port) = wfm_port(dir.path(), 240_000);
        let node = Node::open(cfg(dir.path()), registry(), vec![port])
            .await
            .unwrap();
        src.start();
        let id = start(&node, JobKind::WfmDemod).await;
        assert_eq!(on_disk(dir.path(), &id).capture_rate_hz, Some(240_000));
        let mut graph = record(&node, &id).graph;
        let demod = graph
            .nodes
            .iter()
            .find(|n| n.kind == "wfm_demod")
            .unwrap()
            .id
            .clone();
        let tap = NodeId::new();
        graph.nodes.push(GraphNode {
            id: tap.clone(),
            kind: "passthrough".into(),
            params: serde_json::json!({"kind": "audio"}),
        });
        graph.edges.push(Edge {
            from: demod,
            to: tap,
            kind: StreamKind::Audio,
        });
        node.apply(Op::GraphPut {
            job: id.clone(),
            graph,
        })
        .await
        .unwrap();
        assert_eq!(on_disk(dir.path(), &id).capture_rate_hz, None);
        assert!(on_disk(dir.path(), &id).operator_graph);
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

/// A `graph_put` that changes only a demodulator's carried params (here `squelch_db`) keeps
/// the job the node's: a resume at another capture rate re-expands it, carrying the value.
#[tokio::test]
async fn graph_put_of_only_the_squelch_keeps_the_graph_reexpandable() {
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let (src, port) = wfm_port(dir.path(), 480_000);
        let node = Node::open(cfg(dir.path()), registry(), vec![port])
            .await
            .unwrap();
        src.start();
        let id = start(&node, JobKind::FmDemod).await;
        let mut graph = record(&node, &id).graph;
        assert_eq!(out_rate(&graph), 160_000);
        for n in graph.nodes.iter_mut().filter(|n| n.kind == "nbfm_demod") {
            n.params["squelch_db"] = serde_json::json!(-50);
        }
        node.apply(Op::GraphPut {
            job: id.clone(),
            graph,
        })
        .await
        .unwrap();
        let disk = on_disk(dir.path(), &id);
        assert!(!disk.operator_graph);
        assert_eq!(disk.capture_rate_hz, Some(480_000));
        assert_eq!(nbfm(&disk.graph)["squelch_db"], -50);
        id
    };

    let (src, port) = wfm_port(dir.path(), 240_000);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    // Long enough for the channel stage to have seen blocks: a graph it cannot run fails.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(record(&node, &id).state, JobState::Running);
    let disk = on_disk(dir.path(), &id);
    assert!(!disk.operator_graph);
    assert_eq!(disk.capture_rate_hz, Some(240_000));
    assert_eq!(240_000 % out_rate(&disk.graph), 0, "{:?}", disk.graph);
    assert_eq!(nbfm(&disk.graph)["squelch_db"], -50);
    assert_eq!(nbfm(&disk.graph)["squelch"], true);
}

/// Records written before the squelch: their demodulator nodes carry no `squelch` param.
/// On resume an `aprs_decode` graph (its demodulator feeds the decoder) gets `squelch:
/// false` and so one event stream; an `fm_demod` graph gets `squelch: true` and `squelch_db:
/// null`, both persisted and shown in the record (`graph_get`).
#[tokio::test]
async fn resume_fills_in_the_squelch_params_of_a_pre_squelch_record() {
    let dir = tempfile::tempdir().unwrap();
    let source = SourceId("speech".into());
    let narrow = Tuning {
        center_hz: Hz(CENTER_HZ),
        bandwidth_hz: Hz(12_500),
    };
    let jobs = dir.path().join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let mut ids = Vec::new();
    for kind in [JobKind::AprsDecode, JobKind::FmDemod] {
        let mut graph = kind
            .expand(&source, narrow, Hz(240_000), None, None, None)
            .unwrap();
        for n in graph.nodes.iter_mut().filter(|n| n.kind == "nbfm_demod") {
            let p = n.params.as_object_mut().unwrap();
            p.remove("squelch");
            p.remove("squelch_db");
        }
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.name().into(),
            graph,
            source: source.clone(),
            tuning: narrow,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: Timestamp(1),
            input: None,
            // The rate the source runs at: no re-expansion, the stored graph runs as is.
            capture_rate_hz: Some(240_000),
            elastic: false,
            operator_graph: false,
        };
        std::fs::write(
            jobs.join(format!("{}.json", rec.id.0)),
            serde_json::to_vec(&rec).unwrap(),
        )
        .unwrap();
        ids.push(rec.id);
    }
    let (src, port) = wfm_port(dir.path(), 240_000);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let nbfm = |g: &Graph| {
        g.nodes
            .iter()
            .find(|n| n.kind == "nbfm_demod")
            .unwrap()
            .params
            .clone()
    };
    let events = |id: &JobId| {
        node.bus()
            .list()
            .into_iter()
            .filter(|s| {
                s.kind == zeta_core::types::StreamKind::Event
                    && s.provenance.job.as_ref() == Some(id)
            })
            .count()
    };
    let (aprs, fm) = (&ids[0], &ids[1]);
    for id in [aprs, fm] {
        assert_eq!(record(&node, id).state, JobState::Running, "{id:?}");
    }
    let p = nbfm(&on_disk(dir.path(), aprs).graph);
    assert_eq!(p["squelch"], false, "{p}");
    assert!(p.get("squelch_db").is_some_and(|v| v.is_null()), "{p}");
    assert_eq!(nbfm(&record(&node, aprs).graph), p);
    assert_eq!(
        events(aprs),
        1,
        "aprs_decode: the decoder's event stream only"
    );
    let p = nbfm(&on_disk(dir.path(), fm).graph);
    assert_eq!(p["squelch"], true, "{p}");
    assert!(p.get("squelch_db").is_some_and(|v| v.is_null()), "{p}");
    assert_eq!(nbfm(&record(&node, fm).graph), p);
    assert_eq!(events(fm), 1, "fm_demod: its squelch events");
    // `graph_put` of a graph without the params fills them in the same way.
    let mut g = record(&node, aprs).graph;
    for n in g.nodes.iter_mut().filter(|n| n.kind == "nbfm_demod") {
        n.params.as_object_mut().unwrap().remove("squelch");
    }
    node.apply(Op::GraphPut {
        job: aprs.clone(),
        graph: g,
    })
    .await
    .unwrap();
    assert_eq!(nbfm(&on_disk(dir.path(), aprs).graph)["squelch"], false);
}

/// ch20 of the AU UHF CB plan, where the [`cb_port`] capture is centred.
const CB_CENTER_HZ: u64 = 476_900_000;
/// ch59, the CB channel the guard tests listen on.
const CH59_HZ: u64 = 476_887_500;

/// A looping, real-time file source `speech` centred on ch20 at `rate_hz`: an FM tone on ch20.
fn cb_port(dir: &Path, rate_hz: u32) -> (Arc<FileSource>, Arc<dyn Port>) {
    let audio = zeta_dsp::fixtures::tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = zeta_dsp::fixtures::nbfm_modulate(&audio, 48_000, rate_hz, 5_000.0, 0.0);
    let meta =
        zeta_dsp::fixtures::write_fixture(dir, &format!("cb{rate_hz}"), &iq, rate_hz, CB_CENTER_HZ);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("speech".into()), &meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

/// A CB `fm_demod` recorded before the adjacent-channel guard gains it when resumed at
/// another rate; a `graph_put` that turns it off (`guard_hz: null`) keeps the job the
/// node's, and the `null` survives the next resume.
#[tokio::test]
async fn a_resumed_cb_job_gains_the_guard_and_keeps_it_off_once_put_off() {
    let dir = tempfile::tempdir().unwrap();
    let source = SourceId("speech".into());
    let ch59 = Tuning {
        center_hz: Hz(CH59_HZ),
        bandwidth_hz: Hz(12_500),
    };
    let mut graph = JobKind::FmDemod
        .expand(&source, ch59, Hz(480_000), None, None, None)
        .unwrap();
    for n in &mut graph.nodes {
        let p = n.params.as_object_mut().unwrap();
        for k in ["wide_hz", "guard_hz", "guard_db", "channel_hz"] {
            p.remove(k);
        }
    }
    let rec = JobRecord {
        format: JOB_RECORD_FORMAT,
        id: JobId::new(),
        kind: JobKind::FmDemod.name().into(),
        graph,
        source: source.clone(),
        tuning: ch59,
        state: JobState::Running,
        context: serde_json::json!({}),
        created_ns: Timestamp(1),
        input: None,
        capture_rate_hz: Some(480_000),
        elastic: false,
        operator_graph: false,
    };
    let jobs = dir.path().join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join(format!("{}.json", rec.id.0)),
        serde_json::to_vec(&rec).unwrap(),
    )
    .unwrap();
    let id = rec.id;
    let channel = |g: &Graph| {
        g.nodes
            .iter()
            .find(|n| n.kind == "channel")
            .unwrap()
            .params
            .clone()
    };
    {
        let (src, port) = cb_port(dir.path(), 240_000);
        let node = Node::open(cfg(dir.path()), registry(), vec![port])
            .await
            .unwrap();
        src.start();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(record(&node, &id).state, JobState::Running);
        let disk = on_disk(dir.path(), &id);
        assert_eq!(disk.capture_rate_hz, Some(240_000));
        let p = nbfm(&disk.graph);
        assert_eq!(
            (&p["guard_hz"], &p["guard_db"], &p["channel_hz"]),
            (&12_500.into(), &12.into(), &12_500.into()),
            "{p}"
        );
        assert_eq!(channel(&disk.graph)["wide_hz"], 37_500);

        let mut graph = record(&node, &id).graph;
        for n in graph.nodes.iter_mut().filter(|n| n.kind == "nbfm_demod") {
            n.params["guard_hz"] = serde_json::Value::Null;
        }
        node.apply(Op::GraphPut {
            job: id.clone(),
            graph,
        })
        .await
        .unwrap();
        let disk = on_disk(dir.path(), &id);
        assert!(!disk.operator_graph);
        assert!(nbfm(&disk.graph)["guard_hz"].is_null());
    }

    let (src, port) = cb_port(dir.path(), 480_000);
    let node = Node::open(cfg(dir.path()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(record(&node, &id).state, JobState::Running);
    let disk = on_disk(dir.path(), &id);
    assert!(!disk.operator_graph);
    assert_eq!(disk.capture_rate_hz, Some(480_000));
    let p = nbfm(&disk.graph);
    assert!(p.get("guard_hz").is_some_and(|v| v.is_null()), "{p}");
    assert_eq!(p["channel_hz"], 12_500, "{p}");
}
