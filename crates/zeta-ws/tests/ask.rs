//! `POST /ask`: the agent run against the serving node.

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};
use zeta_agent::LlmConfig;
use zeta_agent::fake::{FakeLlm, text_response, tool_call_response};
use zeta_core::stream::Payload;
use zeta_ws::Limits;
use zeta_ws::testing::{TestOpts, TestServer, spawn, spawn_with};

const BEARER: &str = "Bearer s3cret";

fn llm(fake: &FakeLlm, status: bool, backoff: Duration) -> LlmConfig {
    let mut c = LlmConfig::new(fake.url.clone(), "fake");
    c.backoff_initial = backoff;
    c.backoff_max = backoff;
    c.ready_timeout = Duration::from_secs(5);
    c.request_timeout = Duration::from_secs(5);
    if status {
        c.status_url = Some(format!("{}/status", fake.url));
    }
    c
}

/// A tool call to `source_list`, then the answer "two sources".
async fn scripted(status: Vec<(u16, Value)>) -> FakeLlm {
    FakeLlm::start(
        vec![
            (200, tool_call_response(&[("source_list", json!({}))])),
            (200, text_response("two sources", "stop")),
        ],
        status,
    )
    .await
}

async fn ask(ts: &TestServer, auth: Option<&str>, body: &Value) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .post(format!("{}/ask", ts.url))
        .json(body);
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    req.send().await.unwrap()
}

async fn with_llm(dir: &std::path::Path, cfg: LlmConfig, limits: Limits) -> TestServer {
    spawn_with(
        dir,
        TestOpts {
            llm: Some(cfg),
            limits,
            ..TestOpts::default()
        },
    )
    .await
}

#[tokio::test]
async fn ask_runs_the_agent_and_its_calls_appear_on_the_ops_stream() {
    let dir = tempfile::tempdir().unwrap();
    let fake = scripted(vec![]).await;
    let ts = with_llm(
        dir.path(),
        llm(&fake, false, Duration::from_millis(10)),
        Limits::default(),
    )
    .await;
    let mut ops = ts.node.bus().subscribe(&ts.node.ops_stream()).unwrap();

    let resp = ask(&ts, Some(BEARER), &json!({"request": "what sources?"})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let t: Value = resp.json().await.unwrap();
    assert_eq!(t["request"], "what sources?");
    assert_eq!(t["answer"], "two sources");
    assert_eq!(t["calls"][0]["tool"], "source_list");
    assert_eq!(t["calls"][0]["ok"], true);
    assert_eq!(t["turns"], 2);

    let frame = tokio::time::timeout(Duration::from_secs(5), ops.recv())
        .await
        .expect("ops frame within 5 s")
        .expect("ops stream open");
    let Payload::Event(p) = frame.payload else {
        panic!("ops frame is not an Event");
    };
    assert_eq!(p["op"]["op"], "source_list");
    assert_eq!(p["outcome"], "ok");
    ts.stop().await;
}

#[tokio::test]
async fn ask_without_llm_is_503() {
    let dir = tempfile::tempdir().unwrap();
    let ts = spawn(dir.path()).await;
    let resp = ask(&ts, Some(BEARER), &json!({"request": "what sources?"})).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body,
        json!({"error": "other", "message": "serve started without --llm-url"})
    );
    ts.stop().await;
}

#[tokio::test]
async fn concurrent_ask_is_409() {
    let dir = tempfile::tempdir().unwrap();
    // Not healthy on the first poll: the run waits one 500 ms backoff before its first turn.
    let fake = scripted(vec![(503, json!({})), (200, json!({"healthy": true}))]).await;
    let ts = with_llm(
        dir.path(),
        llm(&fake, true, Duration::from_millis(500)),
        Limits::default(),
    )
    .await;

    let url = ts.url.clone();
    let first = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{url}/ask"))
            .header("authorization", BEARER)
            .json(&json!({"request": "what sources?"}))
            .send()
            .await
            .unwrap()
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while *fake.status_polls.lock().unwrap() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "first run never polled status"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let second = ask(&ts, Some(BEARER), &json!({"request": "what sources?"})).await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let body: Value = second.json().await.unwrap();
    assert_eq!(
        body,
        json!({"error": "other", "message": "an agent run is in progress"})
    );

    let first = first.await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let t: Value = first.json().await.unwrap();
    assert_eq!(t["answer"], "two sources");
    ts.stop().await;
}

#[tokio::test]
async fn ask_requires_auth() {
    let dir = tempfile::tempdir().unwrap();
    let fake = scripted(vec![]).await;
    let ts = with_llm(
        dir.path(),
        llm(&fake, false, Duration::from_millis(10)),
        Limits::default(),
    )
    .await;
    let resp = ask(&ts, None, &json!({"request": "what sources?"})).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(fake.requests.lock().unwrap().is_empty());
    ts.stop().await;
}

#[tokio::test]
async fn ask_times_out_with_504() {
    let dir = tempfile::tempdir().unwrap();
    // Never healthy: the run waits until `ask_timeout` cuts it off.
    let fake = scripted(vec![(503, json!({}))]).await;
    let limits = Limits {
        ask_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let ts = with_llm(
        dir.path(),
        llm(&fake, true, Duration::from_millis(50)),
        limits,
    )
    .await;
    let resp = ask(&ts, Some(BEARER), &json!({"request": "what sources?"})).await;
    assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "other");
    // The lock is released: the next run is refused for the same reason, not as busy.
    let again = ask(&ts, Some(BEARER), &json!({"request": "what sources?"})).await;
    assert_eq!(again.status(), StatusCode::GATEWAY_TIMEOUT);
    ts.stop().await;
}

#[tokio::test]
async fn ask_failure_is_502_and_bad_max_turns_is_422() {
    let dir = tempfile::tempdir().unwrap();
    // A command answered in prose: the agent fails with `no_tool_call`.
    let fake = FakeLlm::start(vec![(200, text_response("sure", "stop"))], vec![]).await;
    let ts = with_llm(
        dir.path(),
        llm(&fake, false, Duration::from_millis(10)),
        Limits::default(),
    )
    .await;

    let resp = ask(
        &ts,
        Some(BEARER),
        &json!({"request": "start a job", "max_turns": 0}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "out_of_range");
    assert_eq!(body["field"], "max_turns");
    assert_eq!(body["min"], 1);
    assert_eq!(body["max"], 32);
    assert!(fake.requests.lock().unwrap().is_empty());

    let resp = ask(&ts, Some(BEARER), &json!({"request": "start a job"})).await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "no_tool_call");
    assert_eq!(body["transcript"]["request"], "start a job");
    ts.stop().await;
}

#[tokio::test]
async fn ask_body_over_the_limit_is_413() {
    let dir = tempfile::tempdir().unwrap();
    let fake = scripted(vec![]).await;
    let limits = Limits {
        body_bytes: 64,
        ..Limits::default()
    };
    let ts = with_llm(
        dir.path(),
        llm(&fake, false, Duration::from_millis(10)),
        limits,
    )
    .await;
    let resp = ask(&ts, Some(BEARER), &json!({"request": "x".repeat(100)})).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(fake.requests.lock().unwrap().is_empty());
    ts.stop().await;
}
