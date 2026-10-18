//! Stream websocket and tail tests against a real server on loopback.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{ChannelId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_ws::testing::{TestOpts, TestServer, spawn, spawn_with};
use zeta_ws::{Limits, WsMessage};

const BEARER: &str = "Bearer s3cret";

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn prov() -> Provenance {
    Provenance {
        source: SourceId("tone".to_string()),
        job: None,
        channel: Some(ChannelId("c1".to_string())),
        span: Span {
            start_ns: Timestamp(1_000),
            end_ns: Timestamp(2_000),
        },
    }
}

fn register(ts: &TestServer) -> StreamId {
    ts.node.bus().register(StreamKind::Event, prov())
}

fn publish(ts: &TestServer, id: &StreamId, v: Value) {
    ts.node.bus().publish(Frame {
        stream: id.clone(),
        kind: StreamKind::Event,
        provenance: prov(),
        payload: Payload::Event(v),
    });
}

async fn connect(ts: &TestServer, id: &str, auth: Option<&str>) -> Result<Ws, WsError> {
    let mut req = format!("ws://{}/stream/{id}", ts.addr)
        .into_client_request()
        .unwrap();
    if let Some(a) = auth {
        req.headers_mut()
            .insert("authorization", a.parse().unwrap());
    }
    connect_async(req).await.map(|(ws, _)| ws)
}

fn status_of(r: Result<Ws, WsError>) -> u16 {
    match r {
        Err(WsError::Http(resp)) => resp.status().as_u16(),
        Err(e) => panic!("expected an HTTP refusal, got {e}"),
        Ok(_) => panic!("expected an HTTP refusal, got an open socket"),
    }
}

/// The next message, which must arrive within `within`.
async fn next(ws: &mut Ws, within: Duration) -> Message {
    tokio::time::timeout(within, ws.next())
        .await
        .expect("no message in time")
        .expect("socket ended")
        .expect("socket error")
}

fn parse(m: Message) -> WsMessage {
    match m {
        Message::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected text, got {other:?}"),
    }
}

/// The `frames` of a tail response, which is the `stream_tail` operation's result.
async fn frames_of(resp: reqwest::Response) -> Vec<zeta_ws::WsFrame> {
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["result"], "frames", "{v}");
    serde_json::from_value(v["value"]["frames"].clone()).unwrap()
}

async fn tail(ts: &TestServer, id: &str, query: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}/streams/{id}/tail?{query}", ts.url))
        .header("authorization", BEARER)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn ws_pushes_published_frames_as_wsframe() {
    // A frame published after connecting arrives as WsMessage::Frame with the Event payload.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = register(&ts);
    let mut ws = connect(&ts, &id.0, Some(BEARER)).await.unwrap();
    publish(&ts, &id, json!({"n": 1}));
    match parse(next(&mut ws, Duration::from_secs(5)).await) {
        WsMessage::Frame(f) => {
            assert_eq!(f.payload, json!({"n": 1}));
            assert_eq!(f.kind, StreamKind::Event);
            assert_eq!(f.stream, id);
            assert_eq!(f.channel, Some(ChannelId("c1".to_string())));
        }
        other => panic!("expected a frame, got {other:?}"),
    }
    drop(ws);
    ts.stop().await;
}

#[tokio::test]
async fn ws_without_secret_is_401() {
    // The auth middleware refuses an upgrade with no bearer secret.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = register(&ts);
    assert_eq!(status_of(connect(&ts, &id.0, None).await), 401);
    ts.stop().await;
}

#[tokio::test]
async fn unknown_stream_is_404_listing_known() {
    // Both the upgrade and the tail answer 404 `unknown` on field `stream`, listing known ids.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = register(&ts);
    assert_eq!(status_of(connect(&ts, "nope", Some(BEARER)).await), 404);
    let resp = tail(&ts, "nope", "max_frames=1&timeout_ms=10").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "unknown");
    assert_eq!(body["field"], "stream");
    assert_eq!(body["value"], "nope");
    assert!(
        body["known"]
            .as_array()
            .unwrap()
            .contains(&Value::String(id.0.clone())),
        "{body}"
    );
    ts.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_client_does_not_stall_bus_or_others() {
    // A client that never reads is dropped after ws_send_timeout; the publisher and another
    // reader carry on: 4000 x ~16 KiB frames publish within 2 s, the reader sees n == 3999
    // within 10 s, and the stats settle at one connection and one slow drop within 5 s.
    let dir = tempfile::tempdir().unwrap();
    let opts = TestOpts {
        limits: Limits {
            ws_send_timeout: Duration::from_millis(500),
            ..Limits::default()
        },
        ..TestOpts::default()
    };
    let ts = spawn_with(dir.path(), opts).await;
    let id = register(&ts);

    let stalled = connect(&ts, &id.0, Some(BEARER)).await.unwrap();
    let mut reader = connect(&ts, &id.0, Some(BEARER)).await.unwrap();
    let reader_task = tokio::spawn(async move {
        loop {
            match parse(next(&mut reader, Duration::from_secs(10)).await) {
                WsMessage::Frame(f) if f.payload["n"] == 3999 => return reader,
                WsMessage::Frame(_) | WsMessage::Lagged { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    });

    let pad = "x".repeat(16384);
    let started = Instant::now();
    for i in 0..4000u32 {
        publish(&ts, &id, json!({"n": i, "pad": pad}));
        if i % 100 == 0 {
            tokio::task::yield_now().await;
        }
    }
    let publish_time = started.elapsed();
    assert!(
        publish_time < Duration::from_secs(2),
        "publishing took {publish_time:?}"
    );

    let reader = tokio::time::timeout(Duration::from_secs(10), reader_task)
        .await
        .expect("reader did not see n == 3999 within 10 s")
        .expect("reader task panicked");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let s = ts.stats.get();
        if s.ws_connections == 1 && s.ws_dropped_slow == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "stats did not settle: {s:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(stalled);
    drop(reader);
    ts.stop().await;
}

#[tokio::test]
async fn stream_end_sends_closed_then_close_1000() {
    // Unregistering the stream sends Closed{"stream ended"}, then a Close frame with 1000.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = register(&ts);
    let mut ws = connect(&ts, &id.0, Some(BEARER)).await.unwrap();
    ts.node.bus().unregister(&id);
    assert_eq!(
        parse(next(&mut ws, Duration::from_secs(5)).await),
        WsMessage::Closed {
            reason: "stream ended".to_string()
        }
    );
    match next(&mut ws, Duration::from_secs(5)).await {
        Message::Close(Some(cf)) => assert_eq!(cf.code, CloseCode::Normal),
        other => panic!("expected a close frame, got {other:?}"),
    }
    ts.stop().await;
}

#[tokio::test]
async fn shutdown_sends_closed_then_going_away_and_new_node_404s_old_id() {
    // Stopping the server sends Closed{"node shutting down"} then Close 1001 within 2 s, and
    // `run` waits for the websocket task to finish before returning; a new
    // node on the same dir and address does not know the old stream id.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let addr = ts.addr;
    let id = register(&ts);
    let mut ws = connect(&ts, &id.0, Some(BEARER)).await.unwrap();
    let stats = ts.stats.clone();
    tokio::time::timeout(Duration::from_secs(2), ts.stop())
        .await
        .expect("stop took longer than 2 s");
    // `run` returned only after the websocket task had sent its close and ended.
    assert_eq!(stats.get().ws_connections, 0);
    assert_eq!(
        parse(next(&mut ws, Duration::from_secs(2)).await),
        WsMessage::Closed {
            reason: "node shutting down".to_string()
        }
    );
    match next(&mut ws, Duration::from_secs(2)).await {
        Message::Close(Some(cf)) => assert_eq!(cf.code, CloseCode::Away),
        other => panic!("expected a close frame, got {other:?}"),
    }
    drop(ws);

    let ts = spawn_with(
        dir.path(),
        TestOpts {
            addr: Some(addr),
            ..TestOpts::default()
        },
    )
    .await;
    let resp = tail(&ts, &id.0, "max_frames=1&timeout_ms=10").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    ts.stop().await;
}

#[tokio::test]
async fn tail_collects_until_max_or_timeout() {
    // Tail returns at max_frames, or the frames so far at the timeout; max_frames=0 is 422.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = register(&ts);

    let bus = ts.node.bus();
    let pid = id.clone();
    let publisher = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        for n in 0..3 {
            bus.publish(Frame {
                stream: pid.clone(),
                kind: StreamKind::Event,
                provenance: prov(),
                payload: Payload::Event(json!({"n": n})),
            });
        }
    });
    let resp = tail(&ts, &id.0, "max_frames=2&timeout_ms=1000").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let frames = frames_of(resp).await;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].payload, json!({"n": 0}));
    assert_eq!(frames[1].payload, json!({"n": 1}));
    publisher.await.unwrap();

    let started = Instant::now();
    let resp = tail(&ts, &id.0, "max_frames=5&timeout_ms=200").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let frames = frames_of(resp).await;
    assert!(frames.is_empty());
    assert!(started.elapsed() >= Duration::from_millis(200));

    let resp = tail(&ts, &id.0, "max_frames=0&timeout_ms=10").await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "out_of_range");
    assert_eq!(body["field"], "max_frames");

    // The limits are the operation's (1..=1000 frames, 1..=10_000 ms), not a second HTTP set.
    let resp = tail(&ts, &id.0, "max_frames=1001&timeout_ms=10").await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["max"], 1000);
    let resp = tail(&ts, &id.0, "max_frames=1000&timeout_ms=10").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = tail(&ts, &id.0, "max_frames=1&timeout_ms=10001").await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["field"], "timeout_ms");
    ts.stop().await;
}

#[tokio::test]
async fn tail_route_matches_stream_tail_op() {
    // GET /streams/{id}/tail and POST /op stream_tail return the same `frames` result.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = register(&ts);
    let get = tail(&ts, &id.0, "max_frames=1&timeout_ms=50").await;
    assert_eq!(get.status(), StatusCode::OK);
    let get: Value = get.json().await.unwrap();
    let post: Value = reqwest::Client::new()
        .post(format!("{}/op", ts.url))
        .header("authorization", BEARER)
        .json(&json!({"op": "stream_tail", "stream": id.0, "max_frames": 1, "timeout_ms": 50}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(get, post);
    assert_eq!(
        get,
        json!({"result": "frames", "value": {"frames": [], "missed": 0}})
    );
    ts.stop().await;
}
