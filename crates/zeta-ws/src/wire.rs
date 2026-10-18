//! The websocket wire shapes: `WsFrame`, `WsMessage` and the `Frame` to `WsFrame` summary.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeta_core::port::IqBlock;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{ChannelId, JobId, SourceId, StreamId, StreamKind, Timestamp};

/// One frame as it crosses the wire: provenance flattened, sample data summarised.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WsFrame {
    pub stream: StreamId,
    pub kind: StreamKind,
    pub source: SourceId,
    pub job: Option<JobId>,
    pub channel: Option<ChannelId>,
    pub start_ns: Timestamp,
    pub end_ns: Timestamp,
    pub payload: Value,
}

/// Every text message a stream websocket sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsMessage {
    Frame(WsFrame),
    Lagged { missed: u64 },
    Closed { reason: String },
}

fn iq_summary(b: &IqBlock) -> Value {
    json!({
        "sample_rate_hz": b.sample_rate_hz,
        "center_hz": b.center_hz,
        "samples": b.samples.len(),
    })
}

/// Summarise a bus frame for the wire; sample data does not cross it.
pub fn ws_frame(f: &Frame) -> WsFrame {
    let payload = match &f.payload {
        Payload::Event(v) => v.clone(),
        Payload::Text(s) => Value::String(s.clone()),
        Payload::Audio {
            sample_rate_hz,
            samples,
        } => {
            let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            json!({
                "sample_rate_hz": sample_rate_hz,
                "samples": samples.len(),
                "peak": peak,
            })
        }
        Payload::Iq(b) | Payload::Sink(b) => iq_summary(b),
        Payload::Video => json!({}),
    };
    let p = &f.provenance;
    WsFrame {
        stream: f.stream.clone(),
        kind: f.kind,
        source: p.source.clone(),
        job: p.job.clone(),
        channel: p.channel.clone(),
        start_ns: p.span.start_ns,
        end_ns: p.span.end_ns,
        payload,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use zeta_core::types::{Hz, Provenance, Span};

    use super::*;

    fn frame(kind: StreamKind, payload: Payload) -> Frame {
        Frame {
            stream: StreamId("s1".into()),
            kind,
            provenance: Provenance {
                source: SourceId("tone".into()),
                job: None,
                channel: Some(ChannelId("c1".into())),
                span: Span {
                    start_ns: Timestamp(10),
                    end_ns: Timestamp(20),
                },
            },
            payload,
        }
    }

    #[test]
    fn audio_and_iq_payloads_are_summarised() {
        let audio = ws_frame(&frame(
            StreamKind::Audio,
            Payload::Audio {
                sample_rate_hz: Hz(48_000),
                samples: Arc::from(vec![0.5f32, -0.75]),
            },
        ));
        assert_eq!(
            audio.payload,
            json!({"sample_rate_hz":48000,"samples":2,"peak":0.75})
        );
        assert_eq!(audio.kind, StreamKind::Audio);
        assert_eq!(audio.channel, Some(ChannelId("c1".into())));
        assert_eq!(audio.source, SourceId("tone".into()));
        assert_eq!(audio.job, None);
        assert_eq!(audio.start_ns, Timestamp(10));
        assert_eq!(audio.end_ns, Timestamp(20));

        let iq = ws_frame(&frame(
            StreamKind::Iq,
            Payload::Iq(IqBlock {
                t0: Timestamp(10),
                sample_rate_hz: Hz(48_000),
                center_hz: Hz(100_000_000),
                samples: vec![Default::default(); 10].into(),
            }),
        ));
        assert_eq!(
            iq.payload,
            json!({"sample_rate_hz":48000,"center_hz":100000000,"samples":10})
        );

        let event = ws_frame(&frame(StreamKind::Event, Payload::Event(json!({"n":1}))));
        assert_eq!(event.payload, json!({"n":1}));
    }

    #[test]
    fn ws_message_is_type_tagged() {
        let lagged = serde_json::to_value(WsMessage::Lagged { missed: 3 }).unwrap();
        assert_eq!(lagged, json!({"type":"lagged","missed":3}));

        let f = ws_frame(&frame(StreamKind::Text, Payload::Text("hi".into())));
        let v = serde_json::to_value(WsMessage::Frame(f)).unwrap();
        assert_eq!(v["type"], "frame");
        assert_eq!(v["stream"], "s1");
        assert_eq!(v["payload"], "hi");
    }
}
