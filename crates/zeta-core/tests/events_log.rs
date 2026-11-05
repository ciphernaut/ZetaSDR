//! The event log: the node's tee on each source's feed writes every event frame of
//! every job on that source to `<state_dir>/events.log` with the job, the source,
//! the channel's tuning and the event itself, whether or not anything else reads
//! the feed. An inline stand-in stage emits the events here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::graph::{Edge, Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{
    Backend, PassthroughStage, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, JobId, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_source_file::{FileOpts, FileSource, fixtures};

/// The span of the `i`th event.
fn span(i: u64) -> Span {
    Span {
        start_ns: Timestamp(1_000_000 + i * 10),
        end_ns: Timestamp(1_000_005 + i * 10),
    }
}

/// What the emitter publishes on its first input frame: an `acars` event, a
/// `pocsag` one and a `transcript` one (every protocol is logged alike).
fn events() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"protocol": "acars", "reg": "C-FGEO", "flight": "AC0035"}),
        serde_json::json!({"protocol": "pocsag", "address": 1234, "text": "page"}),
        serde_json::json!({"protocol": "transcript", "text": "hello"}),
    ]
}

/// A stage that, on its first input frame, publishes [`events`] on its one event
/// output (the `i`th with [`span`]`(i)`), then drains its input until cancelled.
struct Emitter {
    spec: StageSpec,
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
                        for (i, v) in events().into_iter().enumerate() {
                            let mut prov = frame.provenance.clone();
                            prov.span = span(i as u64);
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

fn spec(kind: &'static str, input: StreamKind, outputs: &[StreamKind]) -> StageSpec {
    StageSpec {
        kind,
        inputs: vec![input],
        outputs: outputs.to_vec(),
        params_schema: schemars::json_schema!({"type": "object"}),
    }
}

/// The emitter under stage kind `measure`, reading the job's IQ.
fn registry(sent: Arc<AtomicBool>) -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    r.register(Arc::new(Emitter {
        spec: spec("measure", StreamKind::Iq, &[StreamKind::Event]),
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

fn log_path(dir: &Path) -> PathBuf {
    dir.join("state").join("events.log")
}

/// A one-node graph of `kind`, reading the job's IQ.
fn lone(kind: &str) -> Graph {
    Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId::new(),
            kind: kind.into(),
            params: serde_json::json!({}),
        }],
        edges: Vec::<Edge>::new(),
    }
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

/// A node on the tone with the emitter running as job kind `audio` (a built-in
/// kind such as `measure` would be expanded for the radio).
async fn running(dir: &Path) -> (Node, JobId) {
    let (src, port) = port(dir);
    let node = Node::open(cfg(dir), registry(Arc::default()), vec![port])
        .await
        .unwrap();
    src.start();
    let id = node
        .job_start("audio", tone(), tuning(), lone("measure"), None, false)
        .await
        .unwrap();
    (node, id)
}

#[tokio::test]
async fn every_event_of_a_job_is_one_line_with_nobody_reading_the_feed() {
    let dir = tempfile::tempdir().unwrap();
    let (node, id) = running(dir.path()).await;
    let path = log_path(dir.path());
    assert!(
        until(2_000, || lines(&path).len() >= 3).await,
        "3 lines within 2 s: {:?}",
        lines(&path)
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let got = lines(&path);
    assert_eq!(got.len(), 3, "{got:?}");

    let keys = [
        "t_ns",
        "source",
        "job",
        "channel",
        "center_hz",
        "bandwidth_hz",
        "start_ns",
        "end_ns",
        "event",
    ];
    let null = serde_json::Value::Null;
    for (i, l) in got.iter().enumerate() {
        let m = l.as_object().unwrap();
        let mut have: Vec<_> = m.keys().map(String::as_str).collect();
        have.sort_unstable();
        let mut want = keys.to_vec();
        want.sort_unstable();
        assert_eq!(have, want, "line {i}");
        assert!(l["t_ns"].as_u64().unwrap() > 0);
        assert_eq!(l["source"], "tone");
        assert_eq!(l["job"], id.0);
        assert_eq!(l["channel"], null);
        assert_eq!(l["center_hz"], tuning().center_hz.0);
        assert_eq!(l["bandwidth_hz"], tuning().bandwidth_hz.0);
        let s = span(i as u64);
        assert_eq!(l["start_ns"], s.start_ns.0, "line {i}");
        assert_eq!(l["end_ns"], s.end_ns.0, "line {i}");
        assert_eq!(l["event"], events()[i], "line {i}");
    }
    assert_eq!(node.persist_errors(), Vec::<String>::new());

    // The job's end leaves the log as it is; the tee outlives the job.
    node.job_stop(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(lines(&path).len(), 3);
}

#[tokio::test]
async fn a_second_job_on_the_source_is_logged_by_the_same_tee() {
    let dir = tempfile::tempdir().unwrap();
    let (node, first) = running(dir.path()).await;
    let path = log_path(dir.path());
    assert!(until(2_000, || lines(&path).len() >= 3).await);
    node.job_stop(&first).await.unwrap();
    let second = node
        .job_start("audio", tone(), tuning(), lone("measure"), None, false)
        .await
        .unwrap();
    assert!(
        until(2_000, || lines(&path).len() >= 6).await,
        "6 lines within 2 s: {:?}",
        lines(&path)
    );
    let got = lines(&path);
    assert!(got[..3].iter().all(|l| l["job"] == first.0), "{got:?}");
    assert!(got[3..].iter().all(|l| l["job"] == second.0), "{got:?}");
}
