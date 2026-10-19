use std::sync::Arc;

use serde_json::json;
use zeta_core::port::IqBlock;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_vrt::envelope::{MAX_BODY, MAX_HEADER};
use zeta_vrt::{envelope, parse_envelope};

fn frame(payload: Payload) -> Frame {
    Frame {
        stream: StreamId("s1".into()),
        kind: payload.kind(),
        provenance: Provenance {
            source: SourceId("tone".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(5),
                end_ns: Timestamp(9),
            },
        },
        payload,
    }
}

#[test]
fn text_and_event_roundtrip_and_iq_is_refused() {
    let bytes = envelope(&frame(Payload::Text("héllo".into()))).unwrap();
    let (h, body) = parse_envelope(&bytes).unwrap();
    assert_eq!(h.kind, StreamKind::Text);
    assert_eq!(h.start_ns, Timestamp(5));
    assert_eq!(body, "héllo".as_bytes());

    let bytes = envelope(&frame(Payload::Event(json!({"n": 7})))).unwrap();
    let (h, body) = parse_envelope(&bytes).unwrap();
    assert_eq!(h.payload, json!({"n": 7}));
    assert!(body.is_empty());
    let header_len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    assert_eq!(bytes.len(), 4 + header_len + 4);

    let iq = Payload::Iq(IqBlock {
        t0: Timestamp(0),
        sample_rate_hz: Hz(48_000),
        center_hz: Hz(1),
        samples: Arc::from(vec![]),
    });
    assert!(envelope(&frame(iq)).is_err());
}

#[test]
fn parse_checks_lengths_before_allocating() {
    let good = envelope(&frame(Payload::Text("x".into()))).unwrap();
    assert!(parse_envelope(&good[..2]).is_err());
    assert!(parse_envelope(&good[..good.len() - 1]).is_err());
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(parse_envelope(&trailing).is_err());

    let mut big_header = ((MAX_HEADER + 1) as u32).to_le_bytes().to_vec();
    big_header.extend_from_slice(&[0; 8]);
    let e = parse_envelope(&big_header).unwrap_err();
    assert!(e.contains("65536-byte limit"), "{e}");

    let header_len = u32::from_le_bytes(good[..4].try_into().unwrap()) as usize;
    let mut big_body = good[..4 + header_len].to_vec();
    big_body.extend_from_slice(&((MAX_BODY + 1) as u32).to_le_bytes());
    let e = parse_envelope(&big_body).unwrap_err();
    assert!(e.contains("16777216-byte limit"), "{e}");
}
