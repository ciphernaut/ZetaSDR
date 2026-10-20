//! The unauthenticated static UI and the browser websocket bearer subprotocol.

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use serde_json::json;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use zeta_core::ops::Op;
use zeta_core::types::{StreamId, StreamKind};
use zeta_ws::testing::{TestOpts, TestServer, spawn_with};

const SECRET: &str = "ui-secret-0123456789";

async fn spawn(dir: &std::path::Path) -> TestServer {
    spawn_with(
        dir,
        TestOpts {
            secret: SECRET.to_string(),
            ..TestOpts::default()
        },
    )
    .await
}

fn subprotocols(secret: &str) -> String {
    format!("zeta, zeta.bearer.{}", URL_SAFE_NO_PAD.encode(secret))
}

fn no_redirect() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
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

#[tokio::test]
async fn ui_index_is_served_without_auth() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let client = no_redirect();

    let resp = client.get(format!("{}/ui/", ts.url)).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(resp.headers()[CACHE_CONTROL], "no-cache");
    assert!(resp.text().await.unwrap().contains("ZetaSDR"));

    let resp = client
        .get(format!("{}/ui/index.html", ts.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[CONTENT_TYPE], "text/html; charset=utf-8");

    let resp = client.get(format!("{}/", ts.url)).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()[LOCATION], "/ui/");

    let resp = client
        .get(format!("{}/ui/nope", ts.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"], "other", "{v}");

    // Everything else still requires auth.
    let resp = client
        .get(format!("{}/health", ts.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    ts.stop().await;
}

#[tokio::test]
async fn websocket_accepts_bearer_in_subprotocol() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = iq_stream(&ts).await;
    let mut req = format!("ws://{}/stream/{}", ts.addr, id.0)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        subprotocols(SECRET).parse().unwrap(),
    );
    let (mut ws, resp) = connect_async(req).await.expect("upgrade accepted");
    assert_eq!(resp.status().as_u16(), 101);
    assert_eq!(resp.headers()["sec-websocket-protocol"], "zeta");
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("no frame in time")
        .expect("socket ended")
        .expect("socket error");
    assert!(msg.is_text(), "expected a text frame, got {msg:?}");
    drop(ws);
    ts.stop().await;
}

#[tokio::test]
async fn websocket_subprotocol_with_wrong_secret_is_401() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let id = iq_stream(&ts).await;
    let mut req = format!("ws://{}/stream/{}", ts.addr, id.0)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        subprotocols("wrong-secret").parse().unwrap(),
    );
    let started = Instant::now();
    match connect_async(req).await {
        Err(WsError::Http(resp)) => assert_eq!(resp.status().as_u16(), 401),
        Err(e) => panic!("expected an HTTP refusal, got {e}"),
        Ok(_) => panic!("expected an HTTP refusal, got an open socket"),
    }
    assert!(started.elapsed() >= zeta_ws::Limits::default().auth_fail_delay);
    ts.stop().await;
}

#[tokio::test]
async fn op_route_still_requires_header_auth() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/op", ts.url))
        .header("sec-websocket-protocol", subprotocols(SECRET))
        .header(CONTENT_TYPE, "application/json")
        .body(r#"{"op":"source_list"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    ts.stop().await;
}

#[tokio::test]
async fn subprotocol_is_not_accepted_on_plain_routes() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = reqwest::Client::new()
        .get(format!("{}/health", ts.url))
        .header("upgrade", "websocket")
        .header("connection", "upgrade")
        .header("sec-websocket-protocol", subprotocols(SECRET))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    ts.stop().await;
}

#[tokio::test]
async fn ui_files_carry_a_content_security_policy() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    for path in ["/ui/", "/ui/app.js", "/ui/style.css"] {
        let resp = no_redirect()
            .get(format!("{}{path}", ts.url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        let h = resp.headers();
        let csp = h
            .get("content-security-policy")
            .unwrap_or_else(|| panic!("{path}: no CSP"))
            .to_str()
            .unwrap();
        assert!(csp.contains("default-src 'self'"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert!(csp.contains("connect-src 'self' ws: wss:"), "{csp}");
        assert_eq!(
            h.get("x-content-type-options").unwrap(),
            "nosniff",
            "{path}"
        );
    }
    ts.stop().await;
}
