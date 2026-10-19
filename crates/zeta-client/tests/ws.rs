//! `NodeClient::subscribe`: the binary stream mode with credits, against a loopback server.

use std::time::Duration;

use serde_json::json;
use tokio::time::timeout;
use zeta_client::{ClientError, NodeClient, Received};
use zeta_core::ops::Op;
use zeta_core::port::IqBlock;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_ws::testing::{TestServer, spawn};

/// Start a passthrough job on `tone` at 48 kHz and return its IQ output stream.
async fn iq_stream(ts: &TestServer) -> StreamId {
    let op: Op = serde_json::from_value(json!({"op":"job_start","kind":"passthrough",
        "source":"tone","center_hz":100_000_000,"bandwidth_hz":48_000,"deviation_hz":null,"sideband":null}))
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

#[tokio::test]
async fn subscribe_yields_decoded_iq_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = iq_stream(&ts).await;
    let c = NodeClient::new(&ts.url, "s3cret").unwrap();
    let mut sub = c.subscribe(&id).await.expect("subscribes");
    let mut t0s = Vec::new();
    while t0s.len() < 3 {
        let got = timeout(Duration::from_secs(5), sub.next())
            .await
            .expect("a message within 5 s")
            .expect("subscription open")
            .expect("decodes");
        match got {
            Received::Iq {
                t0_ns,
                meta,
                samples,
                missed_packets,
            } => {
                assert_eq!(meta.sample_rate_hz, 48_000);
                assert_eq!(samples.len(), 4800);
                assert_eq!(missed_packets, 0);
                t0s.push(t0_ns);
            }
            other => panic!("expected iq, got {other:?}"),
        }
    }
    assert_eq!(t0s[1] - t0s[0], 100_000_000);
    assert_eq!(t0s[2] - t0s[1], 100_000_000);
    drop(sub);
    ts.stop().await;
}

#[tokio::test]
async fn subscribe_keeps_credit_topped_up() {
    // The tone source is real time (10 blocks/s), so frames are published straight onto a
    // registered IQ stream, fast, to get well past the initial 256 credits within 15 s.
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let prov = Provenance {
        source: SourceId("tone".to_string()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(0),
        },
    };
    let id = ts.node.bus().register(StreamKind::Iq, prov.clone());
    let c = NodeClient::new(&ts.url, "s3cret").unwrap();
    let mut sub = c.subscribe(&id).await.expect("subscribes");

    let bus = ts.node.bus();
    let stream = id.clone();
    let producer = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(1));
        for n in 0u64.. {
            tick.tick().await;
            bus.publish(Frame {
                stream: stream.clone(),
                kind: StreamKind::Iq,
                provenance: prov.clone(),
                payload: Payload::Iq(IqBlock {
                    t0: Timestamp(n * 10_000_000),
                    sample_rate_hz: Hz(48_000),
                    center_hz: Hz(100_000_000),
                    samples: vec![Default::default(); 480].into(),
                }),
            });
        }
    });

    let mut iq = 0;
    let received = timeout(Duration::from_secs(15), async {
        while iq < 1000 {
            match sub.next().await {
                Some(Ok(Received::Iq { samples, .. })) => {
                    assert_eq!(samples.len(), 480);
                    iq += 1;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("stream error: {e}"),
                None => panic!("subscription ended: {:?}", sub.close_reason()),
            }
        }
    })
    .await;
    producer.abort();
    assert!(received.is_ok(), "only {iq} iq messages within 15 s");
    drop(sub);
    ts.stop().await;
}

#[tokio::test]
async fn unauthorized_subscribe_is_a_client_error() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = iq_stream(&ts).await;
    let c = NodeClient::new(&ts.url, "wrong").unwrap();
    match c.subscribe(&id).await {
        Err(ClientError::Unauthorized { url }) => assert_eq!(url, ts.url),
        other => panic!("expected unauthorized, got {other:?}"),
    }
    let c = NodeClient::new(&ts.url, "s3cret").unwrap();
    match c.subscribe(&StreamId("nope".to_string())).await {
        Err(ClientError::Stream { detail, .. }) => {
            assert!(detail.contains("nope"), "{detail}");
            assert!(detail.contains(&id.0), "{detail}");
        }
        other => panic!("expected a stream error, got {other:?}"),
    }
    ts.stop().await;
}

#[tokio::test]
async fn server_stop_ends_subscription_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = iq_stream(&ts).await;
    let c = NodeClient::new(&ts.url, "s3cret").unwrap();
    let mut sub = c.subscribe(&id).await.expect("subscribes");
    match timeout(Duration::from_secs(5), sub.next()).await {
        Ok(Some(Ok(Received::Iq { .. }))) => {}
        other => panic!("expected iq before stop, got {other:?}"),
    }
    assert_eq!(sub.close_reason(), None);
    let stopper = tokio::spawn(ts.stop());
    let ended = timeout(Duration::from_secs(10), async {
        loop {
            match sub.next().await {
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("stream error: {e}"),
                None => break,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "subscription did not end");
    assert_eq!(sub.close_reason(), Some("node shutting down"));
    assert!(sub.next().await.is_none(), "stays ended");
    stopper.await.unwrap();
}
