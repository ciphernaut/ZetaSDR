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
    /// The stream's kind: `audio`, `packet` or `iq` (a rider reading its demodulator's
    /// channel).
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
/// gone at the start counts as closed.
///
/// A reader slower than its input never holds the input back: the bus keeps each
/// stream's newest frames, and the frames this stage did not read in time are lost to it
/// (a lag). It goes on with the next frame it can read, so downstream sees a gap, not a
/// reset: the frames' timestamps carry the gap (`transcribe` closes an utterance on it;
/// `rds_decode`, riding a demodulator's channel, loses its sync and finds it again). The
/// lag is logged to stderr, and the frames lost so far are noted in the job's context as
/// `stream_in.lagged_frames` (at most once a second).
///
/// `StageSpec::outputs` is fixed per stage, while this stage's output kind is
/// `params.kind`: it declares the kinds it carries, `[audio, packet, iq]`, and its one
/// output is of `params.kind` ([`Stage::outputs_for`]). Any other `kind` is refused.
pub struct StreamInStage {
    spec: StageSpec,
}

impl StreamInStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: STREAM_IN_KIND,
                inputs: vec![],
                outputs: vec![StreamKind::Audio, StreamKind::Packet, StreamKind::Iq],
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

    /// The one output, of `params.kind` (audio when the params do not say).
    fn outputs_for(&self, params: &serde_json::Value) -> Vec<StreamKind> {
        let kind = params
            .get("kind")
            .and_then(|k| serde_json::from_value(k.clone()).ok())
            .unwrap_or(StreamKind::Audio);
        vec![kind]
    }

    async fn run(
        &self,
        params: serde_json::Value,
        _inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: StreamInParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("stream_in params: {e}")))?;
        if !matches!(
            p.kind,
            StreamKind::Audio | StreamKind::Packet | StreamKind::Iq
        ) {
            return Err(StageError::Failed(format!(
                "stream_in: only audio, packet and iq streams can be attached, not {}",
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
        // The frames lost to lags so far, and when the count was last noted.
        let (mut lagged, mut noted): (u64, Option<std::time::Instant>) = (0, None);
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
                        lagged += n;
                        if noted.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1)) {
                            ctx.note(serde_json::json!({"stream_in": {"lagged_frames": lagged}}));
                            noted = Some(std::time::Instant::now());
                        }
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
    async fn forwards_packets_when_its_kind_is_packet() {
        use crate::stream::LinkType;
        let bus = Arc::new(StreamBus::new(8));
        let input = bus.register(StreamKind::Packet, prov("in", 0));
        let out = bus.register(StreamKind::Packet, prov("me", 0));
        let mut rx = bus.subscribe(&out).unwrap();
        let params = serde_json::json!({"job": "in", "kind": "packet", "stream": input.0});
        let stage = StreamInStage::new();
        assert_eq!(
            stage.spec().outputs,
            [StreamKind::Audio, StreamKind::Packet, StreamKind::Iq]
        );
        assert_eq!(stage.outputs_for(&params), [StreamKind::Packet]);
        let (b, o) = (bus.clone(), out.clone());
        let h = tokio::spawn(async move {
            stage
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
        while bus.receiver_count(&input) != Some(1) {
            tokio::task::yield_now().await;
        }
        let sent = prov("in", 1_700_000_000_000_000_000);
        bus.publish(Frame::new(
            input.clone(),
            sent.clone(),
            Payload::Packet {
                link_type: LinkType::Ax25Kiss,
                data: Arc::from(vec![0u8, 1, 2, 3]),
            },
        ));
        let f = rx.recv().await.unwrap();
        assert_eq!(f.stream, out);
        assert_eq!(f.provenance.job, Some(JobId("me".into())));
        assert_eq!(f.provenance.span, sent.span);
        match &f.payload {
            Payload::Packet { link_type, data } => {
                assert_eq!(*link_type, LinkType::Ax25Kiss);
                assert_eq!(&data[..], &[0, 1, 2, 3]);
            }
            p => panic!("not a packet: {p:?}"),
        }
        bus.unregister(&input);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn forwards_iq_when_its_kind_is_iq() {
        use crate::port::IqBlock;
        let bus = Arc::new(StreamBus::new(8));
        let input = bus.register(StreamKind::Iq, prov("in", 0));
        let out = bus.register(StreamKind::Iq, prov("me", 0));
        let mut rx = bus.subscribe(&out).unwrap();
        let params = serde_json::json!({"job": "in", "kind": "iq", "stream": input.0});
        let stage = StreamInStage::new();
        assert_eq!(stage.outputs_for(&params), [StreamKind::Iq]);
        let (b, o) = (bus.clone(), out.clone());
        let h = tokio::spawn(async move {
            stage
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
        while bus.receiver_count(&input) != Some(1) {
            assert!(!h.is_finished(), "the stage ended before reading");
            tokio::task::yield_now().await;
        }
        bus.publish(Frame::new(
            input.clone(),
            prov("in", 700),
            Payload::Iq(IqBlock {
                t0: Timestamp(700),
                sample_rate_hz: Hz(240_000),
                center_hz: Hz(100_000_000),
                samples: Arc::from(vec![num_complex::Complex::new(0.5f32, 0.0); 4]),
            }),
        ));
        let f = rx.recv().await.unwrap();
        assert_eq!(f.stream, out);
        assert_eq!(f.provenance.job, Some(JobId("me".into())));
        match &f.payload {
            Payload::Iq(b) => {
                assert_eq!(
                    (b.center_hz, b.sample_rate_hz),
                    (Hz(100_000_000), Hz(240_000))
                );
                assert_eq!(b.samples.len(), 4);
            }
            p => panic!("not iq: {p:?}"),
        }
        bus.unregister(&input);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    /// A reader slower than its input skips the frames it lost and goes on with the next
    /// ones; the count so far is noted in the job's context.
    #[tokio::test]
    async fn a_lag_skips_the_lost_frames_and_notes_the_count() {
        let bus = Arc::new(StreamBus::new(2));
        let input = bus.register(StreamKind::Audio, prov("in", 0));
        let out = bus.register(StreamKind::Audio, prov("me", 0));
        let mut rx = bus.subscribe(&out).unwrap();
        let params = serde_json::json!({"job": "in", "kind": "audio", "stream": input.0});
        let notes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_note(Arc::new(move |v| seen.lock().unwrap().push(v)));
        let (b, o) = (bus.clone(), out.clone());
        let h = tokio::spawn(async move {
            StreamInStage::new()
                .run(params, vec![], vec![o], b, CancellationToken::new(), ctx)
                .await
        });
        while bus.receiver_count(&input) != Some(1) {
            tokio::task::yield_now().await;
        }
        // Five frames before the stage reads one: it keeps the last two.
        for t in 0..5u64 {
            bus.publish(Frame::new(
                input.clone(),
                prov("in", 1000 + t),
                Payload::Audio {
                    sample_rate_hz: Hz(48_000),
                    channels: 1,
                    samples: Arc::from(vec![0.5f32; 4]),
                },
            ));
        }
        let f = rx.recv().await.unwrap();
        assert_eq!(f.provenance.span.start_ns, Timestamp(1003));
        assert_eq!(
            notes.lock().unwrap().as_slice(),
            [serde_json::json!({"stream_in": {"lagged_frames": 3}})]
        );
        bus.unregister(&input);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn refuses_a_kind_it_does_not_carry() {
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Audio, prov("me", 0));
        let params = serde_json::json!({"job": "in", "kind": "event", "stream": "x"});
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
        assert!(
            matches!(&r, Err(StageError::Failed(m)) if m.contains("only audio, packet and iq")),
            "{r:?}"
        );
    }
}
