//! `NodeClient` against a real server on loopback.

use serde_json::json;
use zeta_client::{ClientError, NodeClient, parse_op};
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{
    ChannelId, Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};
use zeta_ws::testing::spawn;

#[tokio::test]
async fn apply_roundtrips_ok_and_error() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let c = NodeClient::new(&ts.url, "s3cret").unwrap();
    match c.apply(&Op::SourceList {}).await.unwrap() {
        Ok(OpResult::Sources(v)) => assert_eq!(v[0].id.0, "tone"),
        other => panic!("unexpected {other:?}"),
    }
    let ensure = Op::SourceEnsure {
        source: SourceId("tone".to_string()),
        center_hz: Hz(100_000),
        bandwidth_hz: Hz(48_000),
    };
    match c.apply(&ensure).await.unwrap() {
        Err(OpError::OutOfRange { suggested, .. }) => assert_eq!(suggested, Some(100_000_000)),
        other => panic!("unexpected {other:?}"),
    }
    ts.stop().await;
}

#[tokio::test]
async fn wrong_secret_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let c = NodeClient::new(&ts.url, "wrong").unwrap();
    match c.apply(&Op::SourceList {}).await {
        Err(ClientError::Unauthorized { url }) => assert_eq!(url, ts.url),
        other => panic!("unexpected {other:?}"),
    }
    ts.stop().await;
}

#[tokio::test]
async fn stopped_node_is_unreachable_naming_url() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let url = ts.url.clone();
    let c = NodeClient::new(&url, "s3cret").unwrap();
    ts.stop().await;
    match c.apply(&Op::SourceList {}).await {
        Err(ClientError::Unreachable { url: u, .. }) => assert_eq!(u, url),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn schema_streams_agents_md_tail() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let c = NodeClient::new(&format!("{}/", ts.url), "s3cret").unwrap();
    assert_eq!(c.url(), ts.url);
    assert!(c.schema().await.unwrap()["op"]["oneOf"].is_array());
    assert_eq!(c.agents_md().await.unwrap(), "# test guide\n");
    assert_eq!(c.health().await.unwrap()["status"], "ok");

    let prov = Provenance {
        source: SourceId("tone".to_string()),
        job: None,
        channel: Some(ChannelId("c1".to_string())),
        span: Span {
            start_ns: Timestamp(1_000),
            end_ns: Timestamp(2_000),
        },
    };
    let id = ts.node.bus().register(StreamKind::Event, prov.clone());
    let streams = c.streams().await.unwrap();
    assert!(streams.iter().any(|s| s.id == id));

    let node = ts.node.clone();
    let sid = id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        node.bus().publish(Frame {
            stream: sid,
            kind: StreamKind::Event,
            provenance: prov,
            payload: Payload::Event(json!({"n": 1})),
        });
    });
    let frames = c.tail(&id, 1, 1000).await.unwrap().unwrap();
    assert_eq!(frames.as_array().unwrap().len(), 1);

    let bogus = c
        .tail(&StreamId("bogus".to_string()), 1, 10)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(bogus["error"], "unknown");
    ts.stop().await;
}

#[test]
fn parse_op_names_bad_field() {
    match parse_op(json!({"op":"job_stop","jobb":"x"})) {
        Err(OpError::Other { message }) => assert!(message.contains("jobb")),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn new_rejects_non_http_url() {
    assert!(matches!(
        NodeClient::new("ftp://x", "s"),
        Err(ClientError::BadUrl(_))
    ));
}
