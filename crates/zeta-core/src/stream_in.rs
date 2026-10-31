//! The `stream_in` stage: the root of an attached job's graph, reading another job's
//! output stream off the bus instead of the radio.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use crate::stream::{Frame, StreamBus};
use crate::types::{JobId, Provenance, StreamId, StreamKind};

/// The stage kind, as graphs name it.
pub const STREAM_IN_KIND: &str = "stream_in";

/// `stream_in` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct StreamInParams {
    /// The job whose stream is read (the attached job's `input`).
    pub job: JobId,
    /// The stream's kind; only `audio` can be attached so far.
    pub kind: StreamKind,
    /// The stream read. Resolved when the job is attached and again whenever the node
    /// launches the attached job (a resume), from the input job's streams then.
    pub stream: StreamId,
}

/// Subscribes to `params.stream` and republishes each frame on its one output with the
/// attached job's provenance (the output stream's, with the frame's span), so everything
/// downstream is pinned to the attached job. The stream closing for any reason (the input
/// job stopped, failed, was retuned or restarted by `graph_put`) ends the stage with
/// `Ok(())`. That clean end is what lets an attached job follow its input: an input
/// retuned by `job_retune` or restarted by `graph_put` keeps its attached jobs, which the
/// node holds `Running` and relaunches under their own ids on the input's new stream once
/// it runs again. Any other end of the input ends the attached job `Stopped`. The
/// exception is an input that died of its source's fault (its reason `source <id>: ...`,
/// the port still faulted): the node records the attached job `Failed` as `input job <id>
/// not running`, which a resume relaunches once the input runs again, so it comes back
/// when the source recovers (see the job supervisor). A stream already
/// gone at the start counts as closed. A lag is logged to stderr and skipped; the frames'
/// timestamps carry the gap downstream (`transcribe` closes an utterance on it).
///
/// `StageSpec::outputs` is fixed per stage, while this stage's output kind is
/// `params.kind`; it declares `[audio]`, the only kind attachable so far, and refuses any
/// other `kind`.
pub struct StreamInStage {
    spec: StageSpec,
}

impl StreamInStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: STREAM_IN_KIND,
                inputs: vec![],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(StreamInParams),
            },
        }
    }
}

impl Default for StreamInStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for StreamInStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: serde_json::Value,
        _inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: StreamInParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("stream_in params: {e}")))?;
        if p.kind != StreamKind::Audio {
            return Err(StageError::Failed(format!(
                "stream_in: only audio streams can be attached, not {}",
                serde_json::to_value(p.kind).map_or_else(|_| "?".into(), |v| v.to_string())
            )));
        }
        let Some(out) = outputs.first() else {
            return Err(StageError::Failed("stream_in needs one output".into()));
        };
        let Some(base) = bus
            .list()
            .into_iter()
            .find(|s| &s.id == out)
            .map(|s| s.provenance)
        else {
            return Err(StageError::Failed(format!(
                "stream_in: output stream {} is not registered",
                out.0
            )));
        };
        let Some(mut rx) = bus.subscribe(&p.stream) else {
            return Ok(());
        };
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        let provenance = Provenance {
                            span: frame.provenance.span,
                            ..base.clone()
                        };
                        bus.publish(Frame::new(out.clone(), provenance, frame.payload));
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!(
                            "zeta: stream_in lagged on job {}'s stream, {n} frames skipped",
                            p.job.0
                        );
                    }
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
    use crate::types::{Hz, SourceId, Span, Timestamp};

    fn prov(job: &str, t: u64) -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: Some(JobId(job.into())),
            channel: None,
            span: Span {
                start_ns: Timestamp(t),
                end_ns: Timestamp(t + 10),
            },
        }
    }

    #[tokio::test]
    async fn republishes_with_the_attached_provenance_and_ends_on_close() {
        let bus = Arc::new(StreamBus::new(8));
        let input = bus.register(StreamKind::Audio, prov("in", 0));
        let out = bus.register(StreamKind::Audio, prov("me", 0));
        let mut rx = bus.subscribe(&out).unwrap();
        let params = serde_json::json!({"job": "in", "kind": "audio", "stream": input.0});
        let (b, o) = (bus.clone(), out.clone());
        let h = tokio::spawn(async move {
            StreamInStage::new()
                .run(
                    params,
                    vec![],
                    vec![o],
                    b,
                    CancellationToken::new(),
                    StageContext::new(SourceId("s".into())),
                )
                .await
        });
        // Wait for the stage to subscribe.
        while bus.receiver_count(&input) != Some(1) {
            tokio::task::yield_now().await;
        }
        bus.publish(Frame::new(
            input.clone(),
            prov("in", 500),
            Payload::Audio {
                sample_rate_hz: Hz(48_000),
                channels: 1,
                samples: Arc::from(vec![0.5f32; 4]),
            },
        ));
        let f = rx.recv().await.unwrap();
        assert_eq!(f.stream, out);
        assert_eq!(f.provenance.job, Some(JobId("me".into())));
        assert_eq!(f.provenance.span.start_ns, Timestamp(500));
        bus.unregister(&input);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn refuses_a_kind_other_than_audio() {
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Audio, prov("me", 0));
        let params = serde_json::json!({"job": "in", "kind": "iq", "stream": "x"});
        let r = StreamInStage::new()
            .run(
                params,
                vec![],
                vec![out],
                bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await;
        assert!(matches!(r, Err(StageError::Failed(m)) if m.contains("only audio")));
    }
}
