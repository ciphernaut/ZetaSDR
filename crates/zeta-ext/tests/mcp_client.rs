//! The MCP streamable-HTTP client against a stub server.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use zeta_ext::mcp_client::{McpClient, McpError};

#[derive(Default)]
struct Stub {
    initializes: AtomicUsize,
    calls: AtomicUsize,
    /// Sessions the stub knows.
    sessions: Mutex<Vec<String>>,
    /// Forget every session after this many tool calls (0: never).
    forget_after: AtomicUsize,
    /// Never accept a session.
    always_uninit: bool,
    sleep_ms: u64,
    html: bool,
}

const NOT_INIT: &str = r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"Bad Request: Server not initialized"},"id":null}"#;

fn sse(v: Value) -> Response {
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!("event: message\ndata: {v}\n\n"),
    )
        .into_response()
}

async fn mcp(State(s): State<Arc<Stub>>, headers: HeaderMap, body: String) -> Response {
    if s.sleep_ms > 0 {
        tokio::time::sleep(Duration::from_millis(s.sleep_ms)).await;
    }
    if s.html {
        return "<html>".into_response();
    }
    let req: Value = serde_json::from_str(&body).unwrap();
    let id = req["id"].clone();
    match req["method"].as_str().unwrap() {
        "initialize" => {
            let n = s.initializes.fetch_add(1, Ordering::SeqCst);
            let sid = format!("session-{n}");
            if !s.always_uninit {
                s.sessions.lock().unwrap().push(sid.clone());
            }
            let mut r =
                sse(json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-03-26"}}));
            r.headers_mut()
                .insert("mcp-session-id", sid.parse().unwrap());
            r
        }
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/call" => {
            let known = headers
                .get("mcp-session-id")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| s.sessions.lock().unwrap().iter().any(|x| x == h));
            if !known {
                return ([(header::CONTENT_TYPE, "application/json")], NOT_INIT).into_response();
            }
            let n = s.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if s.forget_after.load(Ordering::SeqCst) == n {
                s.sessions.lock().unwrap().clear();
            }
            let text = if req["params"]["name"] == "plain" {
                "just text".to_string()
            } else {
                json!({"tool": req["params"]["name"], "args": req["params"]["arguments"]})
                    .to_string()
            };
            sse(json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}]}}))
        }
        m => panic!("unexpected method {m}"),
    }
}

async fn serve(stub: Stub) -> (String, Arc<Stub>) {
    let stub = Arc::new(stub);
    let app = Router::new()
        .route("/mcp", post(mcp))
        .route(
            "/health",
            get(|| async { r#"{"status":"ok","version":"1.11.0","isSyncing":false}"# }),
        )
        .with_state(stub.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, stub)
}

#[tokio::test]
async fn call_initialises_once_and_parses_the_sse_body() {
    let (url, stub) = serve(Stub::default()).await;
    let c = McpClient::new(&url, Duration::from_secs(5)).unwrap();
    let a = c.call("describe_schema", json!({"x": 1})).await.unwrap();
    let b = c.call("describe_schema", json!({"x": 1})).await.unwrap();
    assert_eq!(a, json!({"tool": "describe_schema", "args": {"x": 1}}));
    assert_eq!(a, b);
    assert_eq!(stub.initializes.load(Ordering::SeqCst), 1);
    assert_eq!(
        c.call("plain", json!({})).await.unwrap(),
        json!("just text")
    );
}

#[tokio::test]
async fn a_lost_session_is_reopened_once() {
    let stub = Stub::default();
    stub.forget_after.store(1, Ordering::SeqCst);
    let (url, stub) = serve(stub).await;
    let c = McpClient::new(&url, Duration::from_secs(5)).unwrap();
    c.call("t", json!({})).await.unwrap();
    c.call("t", json!({})).await.unwrap();
    assert_eq!(stub.initializes.load(Ordering::SeqCst), 2);

    let (url, stub) = serve(Stub {
        always_uninit: true,
        ..Stub::default()
    })
    .await;
    let c = McpClient::new(&url, Duration::from_secs(5)).unwrap();
    let e = c.call("t", json!({})).await.unwrap_err();
    assert!(matches!(e, McpError::Rpc { code: -32000, .. }), "{e:?}");
    assert_eq!(stub.initializes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_slow_server_is_a_timeout_and_a_bad_body_is_bad_body() {
    let (url, _) = serve(Stub {
        sleep_ms: 6000,
        ..Stub::default()
    })
    .await;
    let c = McpClient::new(&url, Duration::from_secs(1)).unwrap();
    assert!(matches!(
        c.call("t", json!({})).await,
        Err(McpError::Timeout)
    ));

    let (url, _) = serve(Stub {
        html: true,
        ..Stub::default()
    })
    .await;
    let c = McpClient::new(&url, Duration::from_secs(5)).unwrap();
    assert!(matches!(
        c.call("t", json!({})).await,
        Err(McpError::BadBody(_))
    ));
}

#[tokio::test]
async fn health_reads_the_json() {
    let (url, _) = serve(Stub::default()).await;
    let c = McpClient::new(&url, Duration::from_secs(5)).unwrap();
    assert_eq!(
        c.health().await.unwrap(),
        json!({"status":"ok","version":"1.11.0","isSyncing":false})
    );
}
