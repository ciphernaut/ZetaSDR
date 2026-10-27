//! The plugin's data-frame header is the websocket's `WsMessage::Frame`, byte for byte, so
//! the two wires converge; `lagged` and `closed` controls are the websocket's messages too.

use std::sync::Arc;

use num_complex::Complex;
use serde_json::{Value, json};
use zeta_core::port::IqBlock;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{ChannelId, Hz, Provenance, SourceId, Span, StreamId, Timestamp};
use zeta_plugin::wire::{Control, PROTOCOL_MIN, encode_control, encode_frame};
use zeta_ws::wire::{WsMessage, ws_frame};

fn frame(payload: Payload) -> Frame {
    Frame::new(
        StreamId("s1".into()),
        Provenance {
            source: SourceId("src".into()),
            job: None,
            channel: Some(ChannelId("c1".into())),
            span: Span {
                start_ns: Timestamp(10),
                end_ns: Timestamp(20),
            },
        },
        payload,
    )
}

/// The header bytes of an encoded data frame.
fn header(bytes: &[u8]) -> &[u8] {
    assert_eq!(bytes[0], 0x00);
    let h = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
    &bytes[5..5 + h]
}

#[test]
fn header_is_plan3_ws_frame_json() {
    let frames = [
        frame(Payload::Iq(IqBlock {
            t0: Timestamp(10),
            sample_rate_hz: Hz(48_000),
            center_hz: Hz(7_100_000),
            samples: Arc::from(vec![Complex::new(0.5f32, -0.5); 3]),
        })),
        frame(Payload::Audio {
            sample_rate_hz: Hz(12_000),
            channels: 1,
            samples: Arc::from(vec![0.25f32, -0.5]),
        }),
        frame(Payload::Event(json!({"label":"ft8","freq_hz":7_074_000}))),
        frame(Payload::Text("hello".into())),
    ];
    for f in &frames {
        let bytes = encode_frame(f, PROTOCOL_MIN).unwrap();
        let ws = WsMessage::Frame(ws_frame(f));
        let ours: Value = serde_json::from_slice(header(&bytes)).unwrap();
        assert_eq!(ours, serde_json::to_value(&ws).unwrap());
        // Same bytes, not just the same value: a client can share one parser.
        assert_eq!(header(&bytes), serde_json::to_vec(&ws).unwrap());
        // And the websocket side parses our header.
        let back: WsMessage = serde_json::from_slice(header(&bytes)).unwrap();
        assert_eq!(back, ws);
    }

    let cases = [
        (
            Control::Lagged { missed: 7 },
            WsMessage::Lagged { missed: 7 },
        ),
        (
            Control::Closed {
                reason: "input closed".into(),
            },
            WsMessage::Closed {
                reason: "input closed".into(),
            },
        ),
    ];
    for (ours, ws) in cases {
        let mut want = serde_json::to_vec(&ws).unwrap();
        want.push(b'\n');
        assert_eq!(encode_control(&ours), want);
    }
}
