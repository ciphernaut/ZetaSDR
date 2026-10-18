//! Stage trait and registry: how CPU, GPU and FPGA implementations of one stage are chosen.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::stream::{Frame, StreamBus};
use crate::types::{StreamId, StreamKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Gpu,
    Fpga,
}

impl Backend {
    fn rank(self) -> u8 {
        match self {
            Backend::Cpu => 0,
            Backend::Gpu => 1,
            Backend::Fpga => 2,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StageError {
    #[error("cancelled")]
    Cancelled,
    #[error("stage failed: {0}")]
    Failed(String),
}

pub struct StageSpec {
    pub kind: &'static str,
    pub inputs: Vec<StreamKind>,
    pub outputs: Vec<StreamKind>,
    pub params_schema: schemars::Schema,
}

#[async_trait]
pub trait Stage: Send + Sync {
    fn spec(&self) -> &StageSpec;
    fn backend(&self) -> Backend;
    /// Run until the inputs close or `cancel` fires.
    ///
    /// Cancellation returns `Err(StageError::Cancelled)`; the runtime treats it as a clean
    /// stop. A closed input ends the stage with `Ok(())`.
    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
    ) -> Result<(), StageError>;
}

#[derive(Default)]
pub struct StageRegistry {
    stages: Vec<Arc<dyn Stage>>,
}

impl StageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, stage: Arc<dyn Stage>) {
        self.stages.push(stage);
    }

    /// Spec of the first implementation registered for `kind`.
    pub fn spec(&self, kind: &str) -> Option<&StageSpec> {
        self.stages
            .iter()
            .map(|s| s.spec())
            .find(|s| s.kind == kind)
    }

    /// Best implementation of `kind` on an available backend (Fpga > Gpu > Cpu);
    /// ties go to the first registered.
    pub fn select(&self, kind: &str, available: &[Backend]) -> Option<Arc<dyn Stage>> {
        let mut best: Option<&Arc<dyn Stage>> = None;
        for s in self
            .stages
            .iter()
            .filter(|s| s.spec().kind == kind && available.contains(&s.backend()))
        {
            if best.is_none_or(|b| s.backend().rank() > b.backend().rank()) {
                best = Some(s);
            }
        }
        best.cloned()
    }
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
struct PassthroughParams {
    kind: StreamKind,
}

const ALL_KINDS: [StreamKind; 6] = [
    StreamKind::Iq,
    StreamKind::Audio,
    StreamKind::Video,
    StreamKind::Text,
    StreamKind::Event,
    StreamKind::Sink,
];

/// Copies frames from its one input to its one output, rewriting the stream id.
/// The `kind` param is informational; edge kinds are checked by graph validation.
pub struct PassthroughStage {
    spec: StageSpec,
}

impl PassthroughStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "passthrough",
                inputs: ALL_KINDS.to_vec(),
                outputs: ALL_KINDS.to_vec(),
                params_schema: schemars::schema_for!(PassthroughParams),
            },
        }
    }
}

impl Default for PassthroughStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for PassthroughStage {
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
    ) -> Result<(), StageError> {
        let (Some(rx), Some(out)) = (inputs.first_mut(), outputs.first()) else {
            return Err(StageError::Failed(
                "passthrough needs one input and one output".into(),
            ));
        };
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(mut frame) => {
                        frame.stream = out.clone();
                        bus.publish(frame);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::Payload;
    use crate::types::{Provenance, SourceId, Span, Timestamp};

    struct Fake(StageSpec, Backend);
    #[async_trait]
    impl Stage for Fake {
        fn spec(&self) -> &StageSpec {
            &self.0
        }
        fn backend(&self) -> Backend {
            self.1
        }
        async fn run(
            &self,
            _: serde_json::Value,
            _: Vec<broadcast::Receiver<Frame>>,
            _: Vec<StreamId>,
            _: Arc<StreamBus>,
            _: CancellationToken,
        ) -> Result<(), StageError> {
            Ok(())
        }
    }

    fn fake(b: Backend) -> Arc<Fake> {
        Arc::new(Fake(
            StageSpec {
                kind: "fft",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::json_schema!({"type": "object"}),
            },
            b,
        ))
    }

    #[test]
    fn registry_prefers_gpu_when_available() {
        let mut r = StageRegistry::new();
        r.register(fake(Backend::Cpu));
        r.register(fake(Backend::Gpu));
        assert_eq!(
            r.select("fft", &[Backend::Cpu, Backend::Gpu])
                .unwrap()
                .backend(),
            Backend::Gpu
        );
        assert_eq!(
            r.select("fft", &[Backend::Cpu]).unwrap().backend(),
            Backend::Cpu
        );
        assert!(r.select("fft", &[]).is_none());
        assert!(r.select("other", &[Backend::Cpu]).is_none());
    }

    fn prov() -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        }
    }

    fn text(stream: StreamId, s: &str) -> Frame {
        Frame {
            stream,
            kind: StreamKind::Text,
            provenance: prov(),
            payload: Payload::Text(s.into()),
        }
    }

    type Run = tokio::task::JoinHandle<Result<(), StageError>>;

    /// Spawn a passthrough fed by a local broadcast channel, so the test can drop the sender.
    fn spawn(
        cancel: CancellationToken,
    ) -> (
        broadcast::Sender<Frame>,
        Arc<StreamBus>,
        StreamId,
        broadcast::Receiver<Frame>,
        Run,
    ) {
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Text, prov());
        let rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(8);
        let (b2, o2) = (bus.clone(), out.clone());
        let h = tokio::spawn(async move {
            PassthroughStage::new()
                .run(
                    serde_json::json!({"kind": "text"}),
                    vec![rx_in],
                    vec![o2],
                    b2,
                    cancel,
                )
                .await
        });
        (tx, bus, out, rx_out, h)
    }

    #[tokio::test]
    async fn passthrough_rewrites_stream_id() {
        let (tx, _bus, out, mut rx_out, h) = spawn(CancellationToken::new());
        tx.send(text(StreamId::new(), "x")).unwrap();
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.stream, out);
        assert!(matches!(f.payload, Payload::Text(ref s) if s == "x"));
        h.abort();
    }

    #[tokio::test]
    async fn passthrough_returns_cancelled_on_cancel() {
        let cancel = CancellationToken::new();
        let (_tx, _bus, _out, _rx, h) = spawn(cancel.clone());
        cancel.cancel();
        assert!(matches!(h.await.unwrap(), Err(StageError::Cancelled)));
    }

    #[tokio::test]
    async fn passthrough_returns_ok_when_input_closed() {
        let (tx, _bus, _out, _rx, h) = spawn(CancellationToken::new());
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }
}
