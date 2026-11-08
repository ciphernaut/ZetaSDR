//! The IQ ring is sized by a memory budget, not a fixed number of seconds.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use tokio::sync::broadcast;
use zeta_core::node::{Node, NodeConfig, ring_seconds_for};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat, SourceId, Span, Timestamp};

const GIB: u64 = 1 << 30;

#[test]
fn ring_seconds_follow_the_budget() {
    assert_eq!(ring_seconds_for(GIB, 2_400_000, 10.0), 10.0);
    // 2^30 / (16e6 * 8) = 8.39 s (the brief rounds it to 8).
    assert!((ring_seconds_for(GIB, 16_000_000, 10.0) - 8.39).abs() < 0.01);
    assert!((ring_seconds_for(GIB, 50_000_000, 10.0) - 2.68).abs() < 0.01);
    assert_eq!(ring_seconds_for(GIB, 200_000_000, 10.0), 1.0);
}

struct Wide {
    id: SourceId,
    caps: Capabilities,
    tx: broadcast::Sender<IqBlock>,
}

#[async_trait]
impl Port for Wide {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        None
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

fn block(rate: u64, n: usize, value: f32) -> IqBlock {
    IqBlock {
        t0: Timestamp(0),
        sample_rate_hz: Hz(rate),
        center_hz: Hz(100_000_000),
        samples: vec![Complex::new(value, 0.0); n].into(),
    }
}

#[tokio::test]
async fn retune_resizes_ring_within_budget() {
    let dir = tempfile::tempdir().unwrap();
    let budget = 128 << 20;
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.path().join("state"),
        ring_seconds: 10.0,
        ring_budget_bytes: budget,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: false,
        bank_factory: None,
        modes_dir: None,
    };
    let node = Node::open(cfg, StageRegistry::new(), vec![]).await.unwrap();
    let port = Arc::new(Wide {
        id: SourceId("wide".into()),
        caps: Capabilities {
            tune_min_hz: Claim::Asserted(Hz(1_000_000)),
            tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
            max_bandwidth_hz: Claim::Asserted(Hz(8_000_000)),
            formats: vec![SampleFormat::Cf32],
            channels: 1,
            rx: true,
            tx: false,
            remote: false,
            push: false,
        },
        tx: broadcast::channel(16).0,
    });
    node.add_port(port.clone()).unwrap();
    for _ in 0..200 {
        if port.tx.receiver_count() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = port.tx.send(block(48_000, 48_000, 1.0));
    let ring = loop {
        if let Some(r) = node.ring(&port.id) {
            break r;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(ring.read().unwrap().capacity_samples(), 480_000);
    let _ = port.tx.send(block(8_000_000, 1_000_000, 7.0));
    for _ in 0..500 {
        if ring.read().unwrap().sample_rate_hz() == Hz(8_000_000) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    {
        let r = ring.read().unwrap();
        assert_eq!(r.sample_rate_hz(), Hz(8_000_000));
        assert!(r.capacity_samples() as u64 * 8 <= budget);
        let t = r.latest().unwrap();
        let got = r
            .read(Span {
                start_ns: r.earliest().unwrap(),
                end_ns: Timestamp(t.0 + 1),
            })
            .unwrap();
        assert!(got.iter().all(|c| c.re == 7.0), "the wide block survived");
        assert!(r.capacity_bytes() as u64 <= budget);
    }
    // Retune back down: the wide allocation must be released.
    let _ = port.tx.send(block(48_000, 48_000, 1.0));
    for _ in 0..500 {
        if ring.read().unwrap().sample_rate_hz() == Hz(48_000) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let r = ring.read().unwrap();
    assert_eq!(r.sample_rate_hz(), Hz(48_000));
    assert!(
        r.capacity_bytes() as u64 <= budget,
        "{} bytes",
        r.capacity_bytes()
    );
}
