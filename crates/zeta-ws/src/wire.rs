//! The websocket wire shapes: `WsMessage`, and `WsFrame`, which is core's `FrameSummary`.
//! Binary mode adds the typed envelope (from `zeta_vrt::envelope`) and credits.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeta_core::stream::Frame;

/// One frame as it crosses the wire: provenance flattened, sample data summarised. The same
/// shape as a `stream_tail` frame, so the websocket and the operation agree.
pub type WsFrame = zeta_core::ops::FrameSummary;

pub use zeta_vrt::envelope::{envelope, parse_envelope};

/// Most packets a binary-mode socket may hold credit for.
pub const CREDIT_CAP: u32 = 65_536;
/// Largest single `credit` grant; a grant outside `1..=CREDIT_MAX_GRANT` is ignored.
pub const CREDIT_MAX_GRANT: u32 = 4_096;

/// Every text message on a stream websocket. The server sends `Frame`, `Lagged` and
/// `Closed`; `Credit` is inbound only, from a binary-mode client, and never sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsMessage {
    Frame(WsFrame),
    Lagged { missed: u64 },
    Closed { reason: String },
    Credit { packets: u32 },
}

/// Summarise a bus frame for the wire; sample data does not cross it.
pub fn ws_frame(f: &Frame) -> WsFrame {
    WsFrame::from_frame(f)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use zeta_core::port::IqBlock;
    use zeta_core::stream::Payload;
    use zeta_core::types::{
        ChannelId, Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
    };

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
        let back: WsMessage = serde_json::from_value(v).unwrap();
        assert!(matches!(back, WsMessage::Frame(f) if f.payload == "hi"));
    }
}
