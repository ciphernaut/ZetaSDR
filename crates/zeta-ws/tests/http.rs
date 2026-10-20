//! HTTP API tests against a real server on loopback.

use std::sync::Arc;

use reqwest::StatusCode;
use serde_json::{Value, json};
use zeta_core::ops::{Op, OpResult};
use zeta_core::types::{Provenance, SourceId, Span, StreamKind, Timestamp};
use zeta_ws::auth::Secret;
use zeta_ws::testing::{TestOpts, TestServer, spawn, spawn_with};
use zeta_ws::{Health, Limits, ServerConfig, bind};

const BEARER: &str = "Bearer s3cret";

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn post_op(ts: &TestServer, auth: Option<&str>, body: &Value) -> reqwest::Response {
    let mut req = client().post(format!("{}/op", ts.url)).json(body);
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    req.send().await.unwrap()
}

async fn get(ts: &TestServer, path: &str) -> reqwest::Response {
    client()
        .get(format!("{}{path}", ts.url))
        .header("authorization", BEARER)
        .send()
        .await
        .unwrap()
}

#[test]
fn apply_future_is_send() {
    fn is_send<T: Send>(_: &T) {}
    async fn f(n: &zeta_core::node::Node) {
        let fut = n.apply(Op::SourceList {});
        is_send(&fut);
        fut.await.ok();
    }
    let _ = f;
}

#[tokio::test]
async fn health_with_secret_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = get(&ts, "/health").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let h: Health = resp.json().await.unwrap();
    assert_eq!(h.status, "ok");
    assert_eq!(h.version, zeta_core::VERSION);
    ts.stop().await;
}

/// `/health` names the build the node runs, so a page can notice an upgrade and reload
/// (the crate version alone does not change between builds).
#[tokio::test]
async fn health_reports_the_build_id() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let h: Health = get(&ts, "/health").await.json().await.unwrap();
    assert!(!h.build.is_empty(), "build id is empty");
    assert_eq!(h.build, zeta_ws::build_id());
    // Without ZETA_BUILD_ID the id is `<version>+<executable mtime>`; with it, the override.
    match std::env::var("ZETA_BUILD_ID") {
        Ok(id) if !id.trim().is_empty() => assert_eq!(h.build, id.trim()),
        _ => assert!(
            h.build.starts_with(&format!("{}+", zeta_core::VERSION)),
            "build id {:?} does not start with the version",
            h.build
        ),
    }
    ts.stop().await;
}

#[tokio::test]
async fn wrong_secret_is_401_and_op_not_applied() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let start = json!({"op":"job_start","kind":"passthrough","source":"tone",
        "center_hz":100_000_000,"bandwidth_hz":48_000,"deviation_hz":null,"sideband":null});
    for auth in [Some("Bearer wrong"), None, Some("Basic s3cret")] {
        let resp = post_op(&ts, auth, &start).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
        assert_eq!(resp.headers()["www-authenticate"], "Bearer");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "other", "{auth:?}");
    }
    let resp = client()
        .get(format!("{}/schema", ts.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        ts.node.apply(Op::JobList {}).await,
        Ok(OpResult::Jobs(vec![]))
    );
    ts.stop().await;
}

#[tokio::test]
async fn op_ok_is_200_with_result() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = post_op(&ts, Some(BEARER), &json!({"op":"source_list"})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["result"], "sources");
    assert_eq!(body["value"][0]["id"], "tone");
    ts.stop().await;
}

#[tokio::test]
async fn op_records_carry_the_callers_transport() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let mut ops = ts.node.bus().subscribe(&ts.node.ops_stream()).unwrap();
    let send = |client_tag: Option<&'static str>| {
        let mut req = client()
            .post(format!("{}/op", ts.url))
            .header("authorization", BEARER)
            .json(&json!({"op": "source_list"}));
        if let Some(t) = client_tag {
            req = req.header("x-zeta-client", t);
        }
        req.send()
    };
    for (tag, want) in [
        (Some("ui"), "ui"),
        (None, "http"),
        (Some("agent"), "http"),
        (Some("local"), "http"),
    ] {
        assert_eq!(send(tag).await.unwrap().status(), StatusCode::OK);
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), ops.recv())
            .await
            .expect("ops frame within 5 s")
            .expect("ops stream open");
        let zeta_core::stream::Payload::Event(p) = frame.payload else {
            panic!("ops frame is not an Event");
        };
        assert_eq!(p["transport"], want, "header {tag:?}");
    }
    ts.stop().await;
}

#[tokio::test]
async fn op_error_is_422_with_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let op =
        json!({"op":"source_ensure","source":"tone","center_hz":100_000,"bandwidth_hz":48_000});
    let resp = post_op(&ts, Some(BEARER), &op).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "out_of_range");
    assert_eq!(body["field"], "center_hz");
    assert_eq!(body["suggested"], 100_000_000);
    ts.stop().await;
}

#[tokio::test]
async fn malformed_op_is_422_naming_field() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let op = json!({"op":"source_ensure","source":"tone","centre_hz":1,"bandwidth_hz":1});
    let resp = post_op(&ts, Some(BEARER), &op).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "other");
    let msg = body["message"].as_str().unwrap();
    assert!(msg.contains("centre_hz"), "{msg}");
    ts.stop().await;
}

#[tokio::test]
async fn oversized_body_is_413_json() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = client()
        .post(format!("{}/op", ts.url))
        .header("authorization", BEARER)
        .header("content-type", "application/json")
        .body(vec![b' '; 2 << 20])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "other");
    ts.stop().await;
}

#[tokio::test]
async fn busy_is_429() {
    let dir = tempfile::tempdir().unwrap();
    let opts = TestOpts {
        limits: Limits {
            max_concurrent_ops: 0,
            ..Limits::default()
        },
        ..TestOpts::default()
    };
    let ts = spawn_with(dir.path(), opts).await;
    let resp = post_op(&ts, Some(BEARER), &json!({"op":"source_list"})).await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "other");
    let msg = body["message"].as_str().unwrap();
    assert!(msg.starts_with("busy"), "{msg}");
    ts.stop().await;
}

#[tokio::test]
async fn schema_is_node_schema_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = get(&ts, "/schema").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body, ts.node.schema());
    ts.stop().await;
}

#[tokio::test]
async fn streams_and_agents_md() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let prov = Provenance {
        source: SourceId("tone".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(0),
        },
    };
    let id = ts.node.bus().register(StreamKind::Event, prov);
    let resp = get(&ts, "/streams").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&id.0.as_str()), "{ids:?}");
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);

    let resp = get(&ts, "/agents-md").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/markdown")
    );
    assert_eq!(resp.text().await.unwrap(), "# test guide\n");
    ts.stop().await;
}

#[tokio::test]
async fn bind_refuses_non_lan() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let cfg = ServerConfig {
        listen: "8.8.8.8:0".parse().unwrap(),
        secret: Secret::new("s3cret").unwrap(),
        agents_md: String::new(),
        limits: Limits::default(),
        llm: None,
        speech: None,
    };
    let err = bind(Arc::clone(&ts.node), cfg).await.err().unwrap();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    ts.stop().await;
}
