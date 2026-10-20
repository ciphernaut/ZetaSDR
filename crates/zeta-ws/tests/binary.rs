//! Binary stream mode: VRT for IQ, the typed envelope for the rest, and credits.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use zeta_core::ops::Op;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{
    ChannelId, Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};
use zeta_vrt::{ContextPacket, VrtDecoder};
use zeta_ws::WsMessage;
use zeta_ws::testing::{TestServer, spawn};
use zeta_ws::wire::parse_envelope;

const BEARER: &str = "Bearer s3cret";

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect(ts: &TestServer, id: &str, query: &str) -> Result<Ws, WsError> {
    let mut req = format!("ws://{}/stream/{id}{query}", ts.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", BEARER.parse().unwrap());
    connect_async(req).await.map(|(ws, _)| ws)
}

/// Start a passthrough job on `tone` at 48 kHz and return its IQ output stream.
async fn iq_stream(ts: &TestServer) -> StreamId {
    let op: Op = serde_json::from_value(json!({"op":"job_start","kind":"passthrough",
        "source":"tone","center_hz":100_000_000,"bandwidth_hz":48_000,"deviation_hz":null,"sideband":null,"elastic":false}))
    .unwrap();
    ts.node.apply(op).await.expect("job starts");
    ts.node
        .bus()
        .list()
        .into_iter()
        .find(|i| i.kind == StreamKind::Iq && i.provenance.job.is_some())
        .expect("job iq stream")
        .id
}

async fn credit(ws: &mut Ws, packets: i64) {
    let text = json!({"type":"credit","packets":packets}).to_string();
    ws.send(Message::Text(text.into())).await.unwrap();
}

/// The next text or binary message within `within`, or `None` if none came.
async fn next(ws: &mut Ws, within: Duration) -> Option<Message> {
    loop {
        match tokio::time::timeout(within, ws.next()).await {
            Err(_) => return None,
            Ok(m) => match m.expect("socket ended").expect("socket error") {
                m @ (Message::Text(_) | Message::Binary(_)) => return Some(m),
                _ => continue,
            },
        }
    }
}

/// The `missed` of the next message, which must be `lagged`.
async fn lagged(ws: &mut Ws) -> u64 {
    match next(ws, Duration::from_secs(5)).await {
        Some(Message::Text(t)) => match serde_json::from_str(&t).unwrap() {
            WsMessage::Lagged { missed } => missed,
            other => panic!("expected lagged, got {other:?}"),
        },
        other => panic!("expected lagged text, got {other:?}"),
    }
}

/// The next binary message and the `missed` of a `lagged` before it (0 if none).
async fn binary_after_lag(ws: &mut Ws) -> (u64, Vec<u8>) {
    match next(ws, Duration::from_secs(5)).await {
        Some(Message::Binary(b)) => (0, b.to_vec()),
        Some(Message::Text(t)) => match serde_json::from_str(&t).unwrap() {
            WsMessage::Lagged { missed } => (missed, binary(ws).await),
            other => panic!("expected lagged, got {other:?}"),
        },
        other => panic!("expected a message, got {other:?}"),
    }
}

async fn binary(ws: &mut Ws) -> Vec<u8> {
    match next(ws, Duration::from_secs(5)).await {
        Some(Message::Binary(b)) => b.to_vec(),
        other => panic!("expected binary, got {other:?}"),
    }
}

#[tokio::test]
async fn binary_iq_stream_is_vrt_after_credit() {
    // No credit, nothing; credit 4 yields `lagged` for the ~3 frames skipped meanwhile, then a
    // context packet and data packets of the 4800-sample blocks.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = iq_stream(&ts).await;
    let mut ws = connect(&ts, &id.0, "?format=binary").await.unwrap();
    assert!(next(&mut ws, Duration::from_millis(300)).await.is_none());
    credit(&mut ws, 4).await;
    let skipped = lagged(&mut ws).await;
    assert!((1..=6).contains(&skipped), "skipped {skipped}");
    let packets = [
        binary(&mut ws).await,
        binary(&mut ws).await,
        binary(&mut ws).await,
        binary(&mut ws).await,
    ];
    let ctx = ContextPacket::decode(&packets[0]).expect("first is a context packet");
    assert_eq!(ctx.sample_rate_hz, 48_000);
    assert_eq!(ctx.bandwidth_hz, 48_000);
    assert_eq!(ctx.stream_id, zeta_vrt::stream_id_of(&id.0));
    let mut d = VrtDecoder::new();
    let mut decoded = Vec::new();
    for p in &packets {
        if let Some(got) = d.feed(p).unwrap() {
            decoded.push(got);
        }
    }
    assert!(!decoded.is_empty());
    for got in &decoded {
        assert_eq!(got.samples.len(), 4800);
        assert_eq!(got.meta.sample_rate_hz, 48_000);
        assert_eq!(got.missed_packets, 0);
    }
    // Credit spent: nothing more arrives.
    assert!(next(&mut ws, Duration::from_millis(300)).await.is_none());
    drop(ws);
    ts.stop().await;
}

#[tokio::test]
async fn credit_zero_skips_and_reports_lagged() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = iq_stream(&ts).await;
    let mut text = connect(&ts, &id.0, "").await.unwrap();
    let mut ws = connect(&ts, &id.0, "?format=binary").await.unwrap();
    credit(&mut ws, 2).await;
    // Frames published before the credit arrived, if any, were skipped too.
    let (early, context) = binary_after_lag(&mut ws).await;
    let first = [context, binary(&mut ws).await];
    assert!(ContextPacket::decode(&first[0]).is_ok());
    tokio::time::sleep(Duration::from_millis(600)).await;
    credit(&mut ws, 4).await;
    let missed = lagged(&mut ws).await;
    assert!(missed >= 4, "missed {missed}");
    let resumed = binary(&mut ws).await;
    let mut d = VrtDecoder::new();
    d.feed(&first[0]).unwrap();
    d.feed(&first[1]).unwrap().expect("data");
    let got = d.feed(&resumed).unwrap().expect("data after lag");
    assert_eq!(got.samples.len(), 4800);
    assert!(ts.stats.get().ws_credit_skipped >= early + 4);

    // The text socket was never throttled: every frame of the period is there.
    let mut frames = 0;
    while let Some(m) = next(&mut text, Duration::from_millis(50)).await {
        match m {
            Message::Text(t) => match serde_json::from_str(&t).unwrap() {
                WsMessage::Frame(f) => {
                    assert_eq!(f.kind, StreamKind::Iq);
                    frames += 1;
                }
                other => panic!("text socket got {other:?}"),
            },
            other => panic!("text socket got {other:?}"),
        }
        if frames >= 6 {
            break;
        }
    }
    assert!(frames >= 6, "text socket got {frames} frames");
    drop((ws, text));
    ts.stop().await;
}

#[tokio::test]
async fn audio_binary_frames_are_envelopes() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let prov = Provenance {
        source: SourceId("tone".to_string()),
        job: None,
        channel: Some(ChannelId("c1".to_string())),
        span: Span {
            start_ns: Timestamp(1_000),
            end_ns: Timestamp(2_000),
        },
    };
    let id = ts.node.bus().register(StreamKind::Audio, prov.clone());
    let mut ws = connect(&ts, &id.0, "?format=binary").await.unwrap();
    credit(&mut ws, 1).await;
    // The credit is read before the frame arrives.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let samples: Vec<f32> = vec![0.5, -0.25, 1.0e-7, -1.0, 0.123_456_79];
    ts.node.bus().publish(Frame {
        stream: id.clone(),
        kind: StreamKind::Audio,
        provenance: prov.clone(),
        payload: Payload::Audio {
            sample_rate_hz: Hz(48_000),
            channels: 1,
            samples: Arc::from(samples.clone()),
        },
    });
    let bytes = binary(&mut ws).await;
    let (header, body) = parse_envelope(&bytes).unwrap();
    assert_eq!(header.kind, StreamKind::Audio);
    assert_eq!(header.stream, id);
    assert_eq!(header.payload["samples"], 5);
    assert_eq!(header.payload["sample_rate_hz"], 48_000);
    assert_eq!(header.payload["channels"], 1);
    assert_eq!(body.len(), samples.len() * 4);
    let back: Vec<f32> = body
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(back, samples);

    // Two interleaved channels: `samples` stays per channel, the body holds both.
    credit(&mut ws, 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stereo: Vec<f32> = vec![0.5, -0.5, 0.25, -0.25, 0.125, -0.125];
    ts.node.bus().publish(Frame {
        stream: id.clone(),
        kind: StreamKind::Audio,
        provenance: prov,
        payload: Payload::Audio {
            sample_rate_hz: Hz(48_000),
            channels: 2,
            samples: Arc::from(stereo.clone()),
        },
    });
    let bytes = binary(&mut ws).await;
    let (header, body) = parse_envelope(&bytes).unwrap();
    assert_eq!(header.payload["channels"], 2);
    assert_eq!(header.payload["samples"], 3);
    assert_eq!(body.len(), 3 * 2 * 4);
    let back: Vec<f32> = body
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(back, stereo);
    drop(ws);
    ts.stop().await;
}

#[tokio::test]
async fn bad_format_is_400_and_bad_credit_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = iq_stream(&ts).await;
    match connect(&ts, &id.0, "?format=vrt").await {
        Err(WsError::Http(resp)) => assert_eq!(resp.status().as_u16(), 400),
        Err(e) => panic!("expected 400, got {e}"),
        Ok(_) => panic!("expected 400, got an open socket"),
    }
    let mut ws = connect(&ts, &id.0, "?format=binary").await.unwrap();
    credit(&mut ws, 0).await;
    credit(&mut ws, 4097).await;
    credit(&mut ws, -1).await;
    ws.send(Message::Text("{not json".into())).await.unwrap();
    ws.send(Message::Text(r#"{"type":"lagged","missed":9}"#.into()))
        .await
        .unwrap();
    assert!(next(&mut ws, Duration::from_millis(300)).await.is_none());
    credit(&mut ws, 1).await;
    // Every frame of the last 300 ms was skipped: none of the bad grants gave credit.
    assert!(lagged(&mut ws).await >= 1);
    let only = binary(&mut ws).await;
    assert!(ContextPacket::decode(&only).is_ok());
    assert!(next(&mut ws, Duration::from_millis(400)).await.is_none());
    drop(ws);
    ts.stop().await;
}
