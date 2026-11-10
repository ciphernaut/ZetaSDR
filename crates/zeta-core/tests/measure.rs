//! A source's worker measuring its link at a start: the node puts one `measuring`
//! event on the source's feed when it begins and one `measured` when it ends, none for
//! each step between, and none at the end of a worker that died measuring (its fault
//! says why). Run over a [`FaultablePort`] whose worker record the tests set.

#![cfg(feature = "test-fixtures")]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::broadcast;

use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::{Port, Starting, measuring_line};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::{Frame, Payload};
use zeta_core::test_fixtures::FaultablePort;
use zeta_core::types::{Hz, SourceId};
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
