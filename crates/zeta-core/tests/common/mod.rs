//! Fixtures the node test targets share (`mod common;`). Each target uses a subset.

#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::node::NodeConfig;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

/// What a stand-in stage emits for one input frame: (output index, payload) pairs.
pub type MapFn = fn(&Frame) -> Vec<(usize, Payload)>;

/// A stage that maps every frame of its one input through `f`, keeping its provenance.
pub struct MapStage {
    spec: StageSpec,
    f: MapFn,
}

impl MapStage {
    pub fn new(
        kind: &'static str,
        input: StreamKind,
        outputs: &[StreamKind],
        f: MapFn,
    ) -> Arc<Self> {
        Arc::new(Self {
            spec: StageSpec {
                kind,
                inputs: vec![input],
                outputs: outputs.to_vec(),
                params_schema: schemars::json_schema!({"type": "object"}),
            },
            f,
        })
    }
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
                        for (i, payload) in (self.f)(&frame) {
                            if let Some(out) = outputs.get(i) {
                                bus.publish(Frame::new(out.clone(), frame.provenance.clone(), payload));
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

pub fn audio() -> Payload {
    Payload::Audio {
        sample_rate_hz: Hz(48_000),
        channels: 1,
        samples: Arc::from(vec![0.1f32; 480]),
    }
}

pub fn same(f: &Frame) -> Vec<(usize, Payload)> {
    vec![(0, f.payload.clone())]
}

/// A node config over `dir`'s `state` directory: one second of ring, the CPU backend.
pub fn cfg(dir: &Path, resume: bool) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume,
        owns_state_dir: true,
        bank_factory: None,
        modes_dir: None,
        handlers_dir: None,
    }
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
pub async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
