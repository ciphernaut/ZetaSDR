//! A source's worker measuring its link at a start: the node puts one `measuring`
//! event on the source's feed when it begins and one `measured` when it ends, none for
//! each step between, and none at the end of a worker that died measuring (its fault
//! says why). `source_measure` drops the source's cached figures and has its worker
//! measure again, refused while jobs or a pin hold the source; a respawn's figures are
//! cached with the link speed they were measured at. Run over a [`FaultablePort`] whose
//! worker record the tests set.

#![cfg(feature = "test-fixtures")]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::broadcast;

use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig, load_source};
use zeta_core::ops::{Op, OpError, OpResult, SourceInfo};
use zeta_core::port::{LinkMeasure, Port, Starting, Tuning, measuring_line};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::{Frame, Payload};
use zeta_core::test_fixtures::FaultablePort;
use zeta_core::types::{Capabilities, Claim, Depth, Hz, JobId, SourceId};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// What a `FaultablePort` reports while the device is away, as a worker port does.
const UNPLUGGED: &str = "no samples for 1000 ms (device unplugged?)";
/// The tone recording's centre.
const TONE_HZ: u64 = 100_000_000;
/// How long a test listens for an event that must not come.
const QUIET: Duration = Duration::from_millis(500);

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
    r
}

/// A looping, real-time tone source `id`.
fn tone(dir: &Path, id: &str) -> Arc<FileSource> {
    let meta = fixtures::write_tone(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: Some((Hz(TONE_HZ - 1_000_000), Hz(TONE_HZ + 1_000_000))),
    };
    Arc::new(FileSource::open(SourceId(id.into()), &meta, opts).unwrap())
}

/// A node over `port`, and a subscription to the feed of source `s`.
async fn node_on(dir: &Path, port: Arc<dyn Port>) -> (Node, broadcast::Receiver<Frame>) {
    let node = Node::open(cfg(dir), registry(), vec![port]).await.unwrap();
    let feed = node.bus().feed_of(&s()).expect("the source's feed");
    let rx = node.bus().subscribe(&feed).unwrap();
    (node, rx)
}

fn s() -> SourceId {
    SourceId("s".into())
}

/// Step `step` of a six-rung probe.
fn starting(step: u32) -> Starting {
    Starting {
        what: "measuring".into(),
        detail: "12-bit at 30720000 Hz".into(),
        step,
        of: 6,
        eta_ms: u64::from(6 - step + 1) * 6000,
    }
}

/// The source events (`source_event` set) the feed carries within `within`, with
/// their frames; stops early once `stop` holds for the last one.
async fn source_events(
    rx: &mut broadcast::Receiver<Frame>,
    within: Duration,
    stop: impl Fn(&Value) -> bool,
) -> Vec<(Frame, Value)> {
    let end = tokio::time::Instant::now() + within;
    let mut out = Vec::new();
    while let Ok(r) = tokio::time::timeout_at(end, rx.recv()).await {
        match r {
            Ok(f) => {
                let Payload::Event(v) = &f.payload else {
                    continue;
                };
                if v.get("source_event").is_none() {
                    continue;
                }
                let v = v.clone();
                let done = stop(&v);
                out.push((f, v));
                if done {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    out
}

fn kind(v: &Value) -> &str {
    v["source_event"].as_str().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_measuring_events_pair() {
    let dir = tempfile::tempdir().unwrap();
    let port = FaultablePort::new(tone(dir.path(), "s"));
    let (_node, mut rx) = node_on(dir.path(), port.clone()).await;
    let first = starting(1);
    port.set_starting(Some(first.clone()));
    let begun = source_events(&mut rx, Duration::from_secs(5), |_| true).await;
    assert_eq!(begun.len(), 1, "{begun:?}");
    port.set_starting(Some(starting(2)));
    port.set_starting(Some(starting(3)));
    port.set_starting(None);
    let mut events = begun;
    events.extend(source_events(&mut rx, Duration::from_secs(5), |v| kind(v) == "measured").await);
    // Nothing after the end.
    events.extend(source_events(&mut rx, QUIET, |_| false).await);
    let kinds: Vec<&str> = events.iter().map(|(_, v)| kind(v)).collect();
    assert_eq!(kinds, ["measuring", "measured"], "{events:?}");
    for (f, _) in &events {
        assert_eq!(f.provenance.job, None);
        assert_eq!(f.provenance.channel, None);
        assert_eq!(f.provenance.source, s());
    }
    let (measuring, measured) = (&events[0].1, &events[1].1);
    assert_eq!(measuring["text"], measuring_line(&s(), &first));
    // Step 1 of 6: six rungs, each tried twice, 3 s an attempt.
    assert_eq!(
        measuring["text"],
        "source s: measuring what this radio's link can sustain (up to about 36 s; the host will be busy; max_rate_hz= skips it)"
    );
    assert_eq!(measuring["starting"], serde_json::to_value(&first).unwrap());
    let text = measured["text"].as_str().unwrap();
    assert!(text.starts_with("source s: measured in "), "{text}");
    assert!(text.ends_with(" s"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_that_dies_measuring_sends_no_measured_event() {
    let dir = tempfile::tempdir().unwrap();
    let port = FaultablePort::new(tone(dir.path(), "s"));
    let (_node, mut rx) = node_on(dir.path(), port.clone()).await;
    port.set_starting(Some(starting(1)));
    let begun = source_events(&mut rx, Duration::from_secs(5), |_| true).await;
    assert_eq!(begun.len(), 1, "{begun:?}");
    port.set_fault(Some(UNPLUGGED.into()));
    port.set_starting(None);
    let after = source_events(&mut rx, QUIET, |_| false).await;
    assert!(after.is_empty(), "{after:?}");
    assert_eq!(kind(&begun[0].1), "measuring");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_port_without_progress_sends_no_events() {
    // A worker that measures nothing: a fault and a heal, no start reported.
    let dir = tempfile::tempdir().unwrap();
    let port = FaultablePort::new(tone(dir.path(), "s"));
    let (_node, mut rx) = node_on(dir.path(), port.clone()).await;
    port.set_fault(Some(UNPLUGGED.into()));
    port.heal();
    let events = source_events(&mut rx, QUIET, |_| false).await;
    assert!(events.is_empty(), "{events:?}");
    // A port that runs no worker has nothing to report.
    let file = tone(dir.path(), "plain");
    assert!(file.starting_changed().is_none());
}

/// The tone recording's sample rate.
const TONE_RATE_HZ: u64 = 48_000;

/// A 16-bit depth whose cap is measured at `hz`.
fn depth16(hz: u64) -> Depth {
    Depth {
        bits: 16,
        lossless: true,
        max_bandwidth_hz: Claim::Measured(Hz(hz)),
    }
}

/// What the worker's hello measured, at link speed `speed`.
fn measure(hz: u64, speed: &str) -> LinkMeasure {
    LinkMeasure {
        max_bandwidth_hz: Claim::Measured(Hz(hz)),
        depths: vec![depth16(hz)],
        at_speed: Some(speed.into()),
    }
}

fn sources_dir(dir: &Path) -> std::path::PathBuf {
    dir.join("state").join("sources")
}

/// A faultable tone source `s` whose worker measured `measure(20_480_000, "5000")`
/// (cached when the node attaches it), under a node; the tone is started.
async fn measured_node(dir: &Path) -> (Arc<Node>, Arc<FaultablePort>, Arc<FileSource>) {
    let src = tone(dir, "s");
    let port = FaultablePort::new(src.clone());
    port.set_latest(Some(measure(20_480_000, "5000")));
    let node = Node::open(cfg(dir), registry(), vec![port.clone() as Arc<dyn Port>])
        .await
        .unwrap();
    src.start();
    let rec = load_source(&sources_dir(dir), &s()).expect("the source's record");
    assert_eq!(rec.measured_at_speed.as_deref(), Some("5000"));
    assert!(rec.capabilities.is_some());
    (Arc::new(node), port, src)
}

fn measure_op(replace: Vec<JobId>) -> Op {
    Op::SourceMeasure {
        source: s(),
        replace,
    }
}

/// A passthrough job on `s` at the tone's band, `elastic` or rigid.
async fn passthrough(node: &Node, elastic: bool) -> JobId {
    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("n".into()),
            kind: "passthrough".into(),
            params: serde_json::json!({"kind": "iq"}),
        }],
        edges: vec![],
    };
    let band = Tuning {
        center_hz: Hz(TONE_HZ),
        bandwidth_hz: Hz(TONE_RATE_HZ),
    };
    node.job_start("passthrough", s(), band, graph, None, elastic)
        .await
        .unwrap()
}

fn state_of(node: &Node, id: &JobId) -> JobState {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .expect("job listed")
        .state
}

async fn source_info(node: &Node) -> SourceInfo {
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => v.into_iter().find(|i| i.id == s()).expect("source s"),
        r => panic!("source_list: {r:?}"),
    }
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_measure_clears_the_figures_and_respawns() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _src) = measured_node(dir.path()).await;
    assert_eq!(node.apply(measure_op(vec![])).await, Ok(OpResult::Ok));
    assert_eq!(port.remeasure_calls(), 1);
    // The record stays, so the source is still known at a restart with the device
    // away; its figures are no longer measurements, so the next start probes.
    let rec = load_source(&sources_dir(dir.path()), &s()).expect("the record still loads");
    assert_eq!(rec.measured_at_speed, None, "{rec:?}");
    let caps = rec.capabilities.expect("the capabilities stay");
    assert_eq!(caps.max_bandwidth_hz, Claim::Asserted(Hz(20_480_000)));
    assert_eq!(
        caps.depths,
        vec![Depth {
            bits: 16,
            lossless: true,
            max_bandwidth_hz: Claim::Asserted(Hz(20_480_000)),
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_measure_is_a_conflict_naming_the_holders() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _src) = measured_node(dir.path()).await;
    let job = passthrough(&node, false).await;
    match node.apply(measure_op(vec![])).await {
        Err(OpError::Conflict {
            source,
            holders,
            options,
            ..
        }) => {
            assert_eq!(source, s());
            assert_eq!(holders, vec![job.clone()]);
            assert!(
                options
                    .iter()
                    .any(|o| o == &format!("retry with replace: [{}]", job.0)),
                "{options:?}"
            );
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(port.remeasure_calls(), 0);
    // Refused, the figures stay.
    let rec = load_source(&sources_dir(dir.path()), &s()).unwrap();
    assert_eq!(rec.measured_at_speed.as_deref(), Some("5000"));
    assert_eq!(state_of(&node, &job), JobState::Running);

    assert_eq!(
        node.apply(measure_op(vec![job.clone()])).await,
        Ok(OpResult::Ok)
    );
    assert_eq!(state_of(&node, &job), JobState::Stopped);
    assert_eq!(port.remeasure_calls(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pin_blocks_source_measure() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _src) = measured_node(dir.path()).await;
    node.apply(Op::SourcePin {
        source: s(),
        center_hz: Hz(TONE_HZ),
        bandwidth_hz: Hz(TONE_RATE_HZ),
        replace: vec![],
    })
    .await
    .unwrap();
    match node.apply(measure_op(vec![])).await {
        Err(OpError::Conflict {
            holders, options, ..
        }) => {
            assert!(holders.is_empty(), "{holders:?}");
            assert!(
                options.iter().any(|o| o == "unpin source s (source_unpin)"),
                "{options:?}"
            );
            // A measure asks for no band: no band to choose or widen to.
            assert!(
                !options
                    .iter()
                    .any(|o| o.contains("band") || o.contains("widen")),
                "{options:?}"
            );
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(port.remeasure_calls(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_elastic_job_does_not_block_source_measure() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _src) = measured_node(dir.path()).await;
    let job = passthrough(&node, true).await;
    assert_eq!(node.apply(measure_op(vec![])).await, Ok(OpResult::Ok));
    assert_eq!(port.remeasure_calls(), 1);
    // Not stopped: it fails with the source's fault and is relaunched at the heal.
    assert_ne!(state_of(&node, &job), JobState::Stopped);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_measure_on_a_source_without_a_worker_is_refused_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let plain = tone(dir.path(), "plain") as Arc<dyn Port>;
    let node = Node::open(cfg(dir.path()), registry(), vec![plain])
        .await
        .unwrap();
    let r = node
        .apply(Op::SourceMeasure {
            source: SourceId("plain".into()),
            replace: vec![],
        })
        .await;
    assert_eq!(
        r,
        Err(OpError::Other {
            message: "source plain has no worker".into()
        })
    );
    let r = node
        .apply(Op::SourceMeasure {
            source: SourceId("nope".into()),
            replace: vec![],
        })
        .await;
    assert!(
        matches!(&r, Err(OpError::Unknown { field, .. }) if field == "source"),
        "{r:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_respawn_persists_its_figures_with_their_speed() {
    let dir = tempfile::tempdir().unwrap();
    let (node, port, _src) = measured_node(dir.path()).await;
    let top = source_info(&node).await.capabilities.max_bandwidth_hz;
    // A respawn on a USB 2 port measured other figures; its hello heals the port.
    port.set_fault(Some(UNPLUGGED.into()));
    port.set_latest(Some(measure(8_000_000, "480")));
    port.heal();
    let stored = || {
        let rec = load_source(&sources_dir(dir.path()), &s())?;
        Some((rec.capabilities?, rec.measured_at_speed))
    };
    until(
        "the respawn's figures are cached with their speed",
        Duration::from_secs(10),
        || {
            stored().is_some_and(|(c, speed): (Capabilities, _)| {
                c.depths == vec![depth16(8_000_000)]
                    && c.max_bandwidth_hz == Claim::Measured(Hz(8_000_000))
                    && speed.as_deref() == Some("480")
            })
        },
    )
    .await;
    // The record for this run is unchanged until the next start.
    assert_eq!(source_info(&node).await.capabilities.max_bandwidth_hz, top);
    let worker = source_info(&node).await.worker.expect("a worker record");
    assert_eq!(worker.link_measure, Some(measure(8_000_000, "480")));
}
