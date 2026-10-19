//! Channel banks: the node keeps one per wide source, feeds it and hands it
//! to every stage of a job on that source.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::bank::{BankError, BankFactory, ChannelBank, PFB_MIN_RATE_HZ};
use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageRegistry, StageSpec};
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{
    Capabilities, Claim, Hz, SampleFormat, SourceId, StreamId, StreamKind, Timestamp,
};

/// A port fixed at `rate` around 100 MHz whose blocks the test sends by hand.
struct FixedPort {
    id: SourceId,
    caps: Capabilities,
    band: Tuning,
    tx: broadcast::Sender<IqBlock>,
}

impl FixedPort {
    fn new(id: &str, rate: u64) -> Arc<Self> {
        Arc::new(Self {
            id: SourceId(id.into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(rate)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: false,
            },
            band: Tuning {
                center_hz: Hz(100_000_000),
                bandwidth_hz: Hz(rate),
            },
            tx: broadcast::channel(16).0,
        })
    }

    fn send(&self, n: usize) {
        let _ = self.tx.send(IqBlock {
            t0: Timestamp(0),
            sample_rate_hz: self.band.bandwidth_hz,
            center_hz: self.band.center_hz,
            samples: vec![Complex::new(0.0, 0.0); n].into(),
        });
    }
}

#[async_trait]
impl Port for FixedPort {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, _: Tuning) -> Result<Tuning, PortError> {
        Ok(self.band)
    }
    fn current(&self) -> Option<Tuning> {
        Some(self.band)
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

/// Counts pushed blocks; subscriptions never deliver.
struct CountingBank {
    band: Tuning,
    pushes: Arc<AtomicUsize>,
    tx: broadcast::Sender<IqBlock>,
}

impl ChannelBank for CountingBank {
    fn band(&self) -> Tuning {
        self.band
    }
    fn bin_hz(&self) -> u64 {
        self.band.bandwidth_hz.0 / 8
    }
    fn subscribe(
        &self,
        center_hz: u64,
        bandwidth_hz: u64,
    ) -> Result<(IqReceiver, Hz, Hz), BankError> {
        if bandwidth_hz > self.bin_hz() {
            return Err(BankError::TooWide {
                bandwidth_hz,
                bin_hz: self.bin_hz(),
            });
        }
        Ok((self.tx.subscribe(), Hz(2 * self.bin_hz()), Hz(center_hz)))
    }
    fn consumers(&self) -> usize {
        self.tx.receiver_count()
    }
    fn push(&self, _: &IqBlock) {
        self.pushes.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct CountingFactory {
    makes: AtomicUsize,
    pushes: Arc<AtomicUsize>,
}

impl BankFactory for CountingFactory {
    fn make(&self, rate_hz: Hz, center_hz: Hz) -> Option<Arc<dyn ChannelBank>> {
        self.makes.fetch_add(1, Ordering::SeqCst);
        Some(Arc::new(CountingBank {
            band: Tuning {
                center_hz,
                bandwidth_hz: rate_hz,
            },
            pushes: self.pushes.clone(),
            tx: broadcast::channel(4).0,
        }))
    }
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

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn node_creates_a_bank_for_wide_sources_and_feeds_it() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(cfg(dir.path()), StageRegistry::new(), vec![])
        .await
        .unwrap();
    let factory = Arc::new(CountingFactory::default());
    node.set_bank_factory(factory.clone());

    let wide = FixedPort::new("wide", 4_000_000);
    node.add_port(wide.clone()).unwrap();
    let narrow = FixedPort::new("narrow", 48_000);
    node.add_port(narrow.clone()).unwrap();
    const { assert!(48_000 < PFB_MIN_RATE_HZ) };

    let bank = node.bank(&wide.id).expect("a bank for the 4 Msps source");
    assert_eq!(bank.band(), wide.band);
    assert!(node.bank(&narrow.id).is_none(), "no bank below 2 Msps");

    // Wait for the feeders to subscribe, then send.
    until("feeders", || wide.tx.receiver_count() >= 2).await;
    until("feeders", || narrow.tx.receiver_count() >= 2).await;
    for _ in 0..3 {
        wide.send(1024);
        narrow.send(1024);
    }
    until("3 pushes", || factory.pushes.load(Ordering::SeqCst) == 3).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        factory.pushes.load(Ordering::SeqCst),
        3,
        "narrow blocks pushed"
    );
    assert_eq!(factory.makes.load(Ordering::SeqCst), 1, "factory calls");
    assert!(Arc::ptr_eq(&bank, &node.bank(&wide.id).unwrap()));
    assert!(node.bank(&narrow.id).is_none());
}

/// Records the context it ran with, then waits for cancellation.
struct Recorder {
    spec: StageSpec,
    seen: Arc<Mutex<Option<(SourceId, bool)>>>,
}

#[async_trait]
impl Stage for Recorder {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _: serde_json::Value,
        _: Vec<broadcast::Receiver<Frame>>,
        _: Vec<StreamId>,
        _: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        *self.seen.lock().unwrap() = Some((ctx.source, ctx.bank.is_some()));
        cancel.cancelled().await;
        Err(StageError::Cancelled)
    }
}

#[tokio::test]
async fn stage_context_carries_source_and_bank() {
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(None));
    let mut reg = StageRegistry::new();
    reg.register(Arc::new(Recorder {
        spec: StageSpec {
            kind: "recorder",
            inputs: vec![StreamKind::Iq],
            outputs: vec![StreamKind::Event],
            params_schema: schemars::json_schema!({"type": "object"}),
        },
        seen: seen.clone(),
    }));
    let node = Node::open(cfg(dir.path()), reg, vec![]).await.unwrap();
    node.set_bank_factory(Arc::new(CountingFactory::default()));
    for (port, banked) in [
        FixedPort::new("wide", 4_000_000),
        FixedPort::new("narrow", 48_000),
    ]
    .into_iter()
    .zip([true, false])
    {
        node.add_port(port.clone()).unwrap();
        *seen.lock().unwrap() = None;
        let graph = Graph {
            id: GraphId::new(),
            nodes: vec![GraphNode {
                id: NodeId("r".into()),
                kind: "recorder".into(),
                params: serde_json::json!({}),
            }],
            edges: vec![],
        };
        let job = node
            .job_start("rec", port.id.clone(), port.band, graph)
            .await
            .unwrap();
        until("the stage to run", || seen.lock().unwrap().is_some()).await;
        assert_eq!(
            seen.lock().unwrap().clone(),
            Some((port.id.clone(), banked)),
            "{}",
            port.id.0
        );
        node.job_stop(&job).await.unwrap();
    }
}
