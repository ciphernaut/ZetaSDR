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

    // The run's pre-read `source_list` first, then the model's one call: two records.
    for which in ["the pre-read", "the model's call"] {
        let frame = tokio::time::timeout(Duration::from_secs(5), ops.recv())
            .await
            .unwrap_or_else(|_| panic!("ops frame for {which} within 5 s"))
            .expect("ops stream open");
        let Payload::Event(p) = frame.payload else {
            panic!("ops frame is not an Event");
        };
        assert_eq!(p["op"]["op"], "source_list", "{which}");
        assert_eq!(p["outcome"], "ok", "{which}");
        assert_eq!(p["transport"], "agent", "{which}");
    }
    // The run has answered, so every record it made is already on the stream.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), ops.recv())
            .await
            .is_err(),
        "a third ops record"
    );
    ts.stop().await;
}

#[tokio::test]
async fn ask_answer_carries_the_selection() {
    let dir = tempfile::tempdir().unwrap();
    let fake = scripted(vec![]).await;
    let ts = with_llm(
        dir.path(),
        llm(&fake, false, Duration::from_millis(10)),
        Limits::default(),
    )
    .await;
    let resp = ask(&ts, Some(BEARER), &json!({"request": "what sources?"})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let selection = &body["selection"];
    assert_eq!(selection["policy"], "free-first", "{body}");
    assert_eq!(selection["caller"]["transport"], "http", "{body}");
    assert_eq!(selection["radio"], Value::Null, "{body}");
    assert!(selection.get("focus").is_none(), "{body}");
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

#[tokio::test]
async fn ask_allow_defaults_to_false_and_rejects_an_unknown_key() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeLlm::start(
        vec![
            (200, tool_call_response(&[("source_list", json!({}))])),
            (200, text_response("two sources", "stop")),
            (200, tool_call_response(&[("source_list", json!({}))])),
            (200, text_response("two sources", "stop")),
        ],
        vec![],
    )
    .await;
    let ts = with_llm(
        dir.path(),
        llm(&fake, false, Duration::from_millis(10)),
        Limits::default(),
    )
    .await;

    let resp = ask(&ts, Some(BEARER), &json!({"request": "what sources?"})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let t: Value = resp.json().await.unwrap();
    assert_eq!(
        t["allow"],
        json!({"stop_jobs": false, "replace": false, "delete_captures": false, "accept_signatures": false})
    );
    assert_eq!(t["turn_log"].as_array().unwrap().len(), 2);

    let resp = ask(
        &ts,
        Some(BEARER),
        &json!({"request": "what sources?", "allow": {"stop_jobs": true}}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let t: Value = resp.json().await.unwrap();
    assert_eq!(
        t["allow"],
        json!({"stop_jobs": true, "replace": false, "delete_captures": false, "accept_signatures": false})
    );

    let resp = ask(
        &ts,
        Some(BEARER),
        &json!({"request": "what sources?", "allow": {"stops": true}}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    ts.stop().await;
}

#[tokio::test]
async fn ask_refuses_job_stop_without_the_allowance() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeLlm::start(
        vec![
            (
                200,
                tool_call_response(&[("job_stop", json!({"job": "01X"}))]),
            ),
            (200, text_response("refused", "stop")),
        ],
        vec![],
    )
    .await;
    let ts = with_llm(
        dir.path(),
        llm(&fake, false, Duration::from_millis(10)),
        Limits::default(),
    )
    .await;
    let mut ops = ts.node.bus().subscribe(&ts.node.ops_stream()).unwrap();

    let resp = ask(&ts, Some(BEARER), &json!({"request": "stop job 01X"})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let t: Value = resp.json().await.unwrap();
    assert_eq!(t["calls"][0]["result"]["error"], "not_allowed");

    while let Ok(Ok(frame)) = tokio::time::timeout(Duration::from_millis(500), ops.recv()).await {
        let Payload::Event(p) = frame.payload else {
            continue;
        };
        assert_ne!(p["op"]["op"], "job_stop", "job_stop reached the node");
    }
    ts.stop().await;
}

#[tokio::test]
async fn ask_page_tools_default_off_and_on_offers_view_set_with_the_ready_themes() {
    let dir = tempfile::tempdir().unwrap();
    let view_set =
        |value: &str| tool_call_response(&[("view_set", json!({"key": "theme", "value": value}))]);
    let fake = FakeLlm::start(
        vec![
            (200, view_set("plain")),
            (200, text_response("cannot", "stop")),
            (200, view_set("plain")),
            (200, view_set("neon")),
            (200, text_response("Switched to plain.", "stop")),
        ],
        vec![],
    )
    .await;
    let ts = with_llm(
        dir.path(),
        llm(&fake, false, Duration::from_millis(10)),
        Limits::default(),
    )
    .await;
    let mut ops = ts.node.bus().subscribe(&ts.node.ops_stream()).unwrap();
    let tool_names = |req: &Value| -> Vec<String> {
        req["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
            .collect()
    };

    // Absent: accepted, and the tool is not offered.
    let resp = ask(
        &ts,
        Some(BEARER),
        &json!({"request": "switch to the plain theme"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let t: Value = resp.json().await.unwrap();
    assert_eq!(t["calls"][0]["ok"], false);
    assert_eq!(t["calls"][0]["result"]["field"], "tool");
    assert!(!tool_names(&fake.requests.lock().unwrap()[0]).contains(&"view_set".to_string()));

    // On: offered, answered for the page, checked against the ready themes.
    let resp = ask(
        &ts,
        Some(BEARER),
        &json!({"request": "switch to the plain theme", "page_tools": true}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let t: Value = resp.json().await.unwrap();
    assert!(tool_names(&fake.requests.lock().unwrap()[2]).contains(&"view_set".to_string()));
    assert_eq!(t["calls"][0]["tool"], "view_set");
    assert_eq!(t["calls"][0]["ok"], true);
    assert_eq!(t["calls"][0]["result"]["page"], true);
    assert_eq!(t["calls"][1]["ok"], false);
    let known = t["calls"][1]["result"]["known"].as_array().unwrap().clone();
    assert!(
        known.contains(&json!("console")) && known.contains(&json!("plain")),
        "{known:?}"
    );
    assert_eq!(t["answer"], "Switched to plain.");

    // An unknown field is still refused.
    let resp = ask(
        &ts,
        Some(BEARER),
        &json!({"request": "switch", "page_tools": true, "page": true}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Only the two runs' pre-read `source_list` reached the node, never a `view_set`.
    for run in ["the run without page tools", "the run with them"] {
        let frame = tokio::time::timeout(Duration::from_secs(5), ops.recv())
            .await
            .unwrap_or_else(|_| panic!("the pre-read of {run} within 5 s"))
            .expect("ops stream open");
        let Payload::Event(p) = frame.payload else {
            panic!("ops frame is not an Event");
        };
        assert_eq!(p["op"]["op"], "source_list", "{run}");
        assert_eq!(p["transport"], "agent", "{run}");
    }
    if let Ok(Ok(frame)) = tokio::time::timeout(Duration::from_millis(300), ops.recv()).await {
        panic!("an op reached the node: {:?}", frame.payload);
    }
    ts.stop().await;
}
