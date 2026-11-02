//! Stage trait and registry: how CPU, GPU and FPGA implementations of one stage are chosen.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::bank::ChannelBank;
use crate::modes::ModeRow;
use crate::stream::{Frame, StreamBus};
use crate::types::{JobId, SourceId, StreamId, StreamKind};

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

/// A stage's ports. `outputs` lists one output per entry, in the order the stage
/// receives their streams (a job gives each its own stream: see
/// [`StageSpec::emits_any_kind`] for the exception).
pub struct StageSpec {
    pub kind: &'static str,
    pub inputs: Vec<StreamKind>,
    pub outputs: Vec<StreamKind>,
    pub params_schema: schemars::Schema,
}

impl StageSpec {
    /// Whether `outputs` lists every stream kind (`passthrough`): such a stage has one
    /// output of whatever kind its edge carries, not one output per kind, so a job gives
    /// it the streams of the edges leaving it, or one stream of its first kind.
    pub fn emits_any_kind(&self) -> bool {
        ALL_KINDS.iter().all(|k| self.outputs.contains(k))
    }
}

/// A stage's note to its job: merged into the job's context (shallow, as
/// `job_context_merge`).
pub type StageNote = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

/// One audio output a `mix` stage may read: a job's stream, the job's source and centre.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixInput {
    pub job: JobId,
    pub stream: StreamId,
    pub source: SourceId,
    pub center_hz: u64,
}

/// The audio outputs of the jobs a node's `mix` stage may read, asked anew on each poll.
pub type MixInputs = Arc<dyn Fn() -> Vec<MixInput> + Send + Sync>;

/// What a stage knows of the job it runs in beyond its params and streams:
/// the job's source, when the node keeps one for it, the source's channel
/// bank, and the job's [`StageNote`] when it runs in a node.
#[derive(Clone)]
pub struct StageContext {
    pub source: SourceId,
    pub bank: Option<Arc<dyn ChannelBank>>,
    pub note: Option<StageNote>,
    /// The mode row the job runs, when its kind resolves to one: a decoder stage then
    /// publishes its events in the row's envelope (`protocol`, `summary`, `t_ns`).
    pub mode: Option<Arc<ModeRow>>,
    /// The absolute frequency of the receiver's DC (the capture's centre), `Some`
    /// only for a job on a local hardware source (`ops::local_hardware`).
    pub dc_hz: Option<u64>,
    /// The node's list of audio outputs, `Some` for every job in a node (only a `mix`
    /// stage reads it).
    pub mix_inputs: Option<MixInputs>,
    /// The node's station (position, region, antennas), when one is set.
    pub station: Option<crate::station::Station>,
    /// The node's burst store, `Some` for every job in a node (only a `burst_capture`
    /// stage writes to it).
    pub bursts: Option<Arc<crate::bursts::BurstStore>>,
}

impl StageContext {
    /// A context for `source` with no bank and no note.
    pub fn new(source: SourceId) -> Self {
        Self {
            source,
            bank: None,
            note: None,
            mode: None,
            dc_hz: None,
            mix_inputs: None,
            station: None,
            bursts: None,
        }
    }

    /// This context with the node's burst store.
    pub fn with_bursts(self, bursts: Arc<crate::bursts::BurstStore>) -> Self {
        Self {
            bursts: Some(bursts),
            ..self
        }
    }

    /// This context with the node's `station`.
    pub fn with_station(self, station: Option<crate::station::Station>) -> Self {
        Self { station, ..self }
    }

    /// This context with the receiver's DC at `hz` (`None` for none).
    pub fn with_dc_hz(self, hz: Option<u64>) -> Self {
        Self { dc_hz: hz, ..self }
    }

    /// This context with `note`.
    pub fn with_note(self, note: StageNote) -> Self {
        Self {
            note: Some(note),
            ..self
        }
    }

    /// This context with the mode `row`.
    pub fn with_mode(self, row: Arc<ModeRow>) -> Self {
        Self {
            mode: Some(row),
            ..self
        }
    }

    /// This context with the node's list of audio outputs `f`.
    pub fn with_mix_inputs(self, f: MixInputs) -> Self {
        Self {
            mix_inputs: Some(f),
            ..self
        }
    }

    /// Merge `value` into the job's context; nothing without a note.
    pub fn note(&self, value: serde_json::Value) {
        if let Some(n) = &self.note {
            n(value);
        }
    }
}

/// Whether a stage can run on this node now, and with what capacity.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Readiness {
    /// The stage can run.
    pub ready: bool,
    /// Why it cannot, when `ready` is false.
    pub reason: Option<String>,
    /// How many runs it serves at once, when something bounds that (`None`: unbounded or
    /// not known).
    pub capacity: Option<u32>,
}

impl Readiness {
    /// Ready, with no bound on its capacity: what a stage with no external need answers.
    pub fn always() -> Self {
        Self {
            ready: true,
            reason: None,
            capacity: None,
        }
    }

    /// Not ready, for `reason`.
    pub fn unready(reason: impl Into<String>) -> Self {
        Self {
            ready: false,
            reason: Some(reason.into()),
            capacity: None,
        }
    }
}

#[async_trait]
pub trait Stage: Send + Sync {
    fn spec(&self) -> &StageSpec;
    fn backend(&self) -> Backend;
    /// The outputs a node of this stage with `params` is given streams for, in order: by
    /// default every one in `spec().outputs`. A stage whose trailing output stays silent
    /// under some params (a squelch turned off) leaves it out, so its job lists no stream
    /// that never carries a frame.
    fn outputs_for(&self, _params: &serde_json::Value) -> Vec<StreamKind> {
        self.spec().outputs.clone()
    }
    /// Check `params` (already valid against `spec().params_schema`) for what the schema
    /// cannot say, so a bad node is refused when the graph is validated instead of
    /// failing its job at run time; the error names the field. By default nothing more.
    fn check_params(&self, _params: &serde_json::Value) -> Result<(), String> {
        Ok(())
    }
    /// Whether this stage can run on this node now (a speech backend present, a program
    /// installed) and with what capacity, so a request is refused before a job is made
    /// that would fail at launch. By default always ready.
    fn readiness(&self) -> Readiness {
        Readiness::always()
    }
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
        ctx: StageContext,
    ) -> Result<(), StageError>;
}

#[derive(Clone, Default)]
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

    /// [`Stage::check_params`] of the first implementation registered for `kind` (`Ok` for
    /// an unknown kind).
    pub fn check_params(&self, kind: &str, params: &serde_json::Value) -> Result<(), String> {
        self.stages
            .iter()
            .find(|s| s.spec().kind == kind)
            .map_or(Ok(()), |s| s.check_params(params))
    }

    /// [`Stage::readiness`] of the first implementation registered for `kind` (`None` for
    /// an unknown kind).
    pub fn readiness(&self, kind: &str) -> Option<Readiness> {
        self.stages
            .iter()
            .find(|s| s.spec().kind == kind)
            .map(|s| s.readiness())
    }

    /// Every registered stage kind, sorted, each once.
    pub fn kinds(&self) -> Vec<String> {
        let mut v: Vec<String> = self.stages.iter().map(|s| s.spec().kind.into()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// One `{kind, inputs, outputs, params_schema, readiness}` object per stage kind,
    /// sorted by kind, from the first implementation registered for it.
    pub fn describe(&self) -> serde_json::Value {
        let stages = self
            .kinds()
            .iter()
            .filter_map(|k| self.spec(k))
            .map(|s| {
                serde_json::json!({
                    "kind": s.kind,
                    "inputs": s.inputs,
                    "outputs": s.outputs,
                    "params_schema": s.params_schema,
                    "readiness": self.readiness(s.kind),
                })
            })
            .collect();
        serde_json::Value::Array(stages)
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
        _ctx: StageContext,
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
            _: StageContext,
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
        Frame::new(stream, prov(), Payload::Text(s.into()))
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
                    StageContext::new(SourceId("s".into())),
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
