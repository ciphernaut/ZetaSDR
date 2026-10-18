//! The chat client against the fake OpenAI-compatible server over loopback.
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use zeta_agent::fake::{FakeLlm, text_response, tool_call_response};
use zeta_agent::{AgentError, ChatMessage, LlmClient, LlmConfig};
use zeta_client::ToolDef;

fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "source_list".into(),
            description: "List sources.".into(),
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        },
        ToolDef {
            name: "job_stop".into(),
            description: "Stop a job.".into(),
            input_schema: json!({"type":"object","properties":{"job":{"type":"string"}},"required":["job"]}),
        },
    ]
}

fn cfg(fake: &FakeLlm) -> LlmConfig {
    let mut c = LlmConfig::new(fake.url.clone(), "floor-model");
    c.backoff_initial = Duration::from_millis(10);
    c.backoff_max = Duration::from_millis(10);
    c.ready_timeout = Duration::from_secs(5);
    c.request_timeout = Duration::from_secs(5);
    c
}

fn msgs() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("be terse"),
        ChatMessage::user("what is on 144.8?"),
    ]
}

#[tokio::test]
async fn request_disables_thinking_and_sets_budget() {
    let ok = tool_call_response(&[("source_list", json!({}))]);
    let fake = FakeLlm::start(vec![(200, ok.clone()), (200, ok)], vec![]).await;
    let tools_in = tools();

    let client = LlmClient::new(cfg(&fake)).unwrap();
    client.complete(&msgs(), &tools_in).await.unwrap();
    let mut thinking = cfg(&fake);
    thinking.thinking = true;
    LlmClient::new(thinking)
        .unwrap()
        .complete(&msgs(), &tools_in)
        .await
        .unwrap();

    let reqs = fake.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    let body = &reqs[0];
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        json!(false)
    );
    assert_eq!(body["max_tokens"], json!(1024));
    assert_eq!(body["temperature"], json!(0.0));
    assert_eq!(body["tool_choice"], json!("auto"));
    assert_eq!(body["model"], json!("floor-model"));
    assert_eq!(body["messages"][1]["content"], json!("what is on 144.8?"));
    let tools_out = body["tools"].as_array().unwrap();
    assert_eq!(tools_out.len(), tools_in.len());
    assert_eq!(tools_out[0]["type"], json!("function"));
    for (out, t) in tools_out.iter().zip(&tools_in) {
        assert_eq!(out["function"]["name"], json!(t.name));
        assert_eq!(out["function"]["description"], json!(t.description));
        assert_eq!(out["function"]["parameters"], t.input_schema);
    }
    assert!(reqs[1].get("chat_template_kwargs").is_none());
}

#[tokio::test]
async fn reasoning_content_is_tolerated() {
    let fake = FakeLlm::start(
        vec![(
            200,
            tool_call_response(&[("job_stop", json!({"job":"j1"}))]),
        )],
        vec![],
    )
    .await;
    let done = LlmClient::new(cfg(&fake))
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap();
    assert_eq!(done.tool_calls.len(), 1);
    assert!(done.reasoning.is_some());
    assert_eq!(done.tool_calls[0].function.name, "job_stop");
    assert_eq!(
        done.tool_calls[0].function.args().unwrap(),
        json!({"job":"j1"})
    );
    assert_eq!(done.finish_reason.as_deref(), Some("tool_calls"));
}

#[tokio::test]
async fn loading_503_is_waited_out() {
    let loading: Value = json!({"error":{"code":503,"message":"Loading model"}});
    let fake = FakeLlm::start(
        vec![
            (503, loading.clone()),
            (503, loading),
            (200, tool_call_response(&[("source_list", json!({}))])),
        ],
        vec![],
    )
    .await;
    let done = LlmClient::new(cfg(&fake))
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap();
    assert_eq!(done.tool_calls.len(), 1);
    assert_eq!(fake.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn readiness_timeout_is_not_ready() {
    let loading: Value = json!({"error":{"code":503,"message":"Loading model"}});
    let fake = FakeLlm::start(vec![(503, loading); 1000], vec![]).await;
    let mut c = cfg(&fake);
    c.ready_timeout = Duration::from_millis(100);
    let err = LlmClient::new(c)
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap_err();
    match err {
        AgentError::NotReady { last, .. } => assert!(last.contains("Loading model"), "{last}"),
        other => panic!("expected NotReady, got {other:?}"),
    }
}

#[tokio::test]
async fn status_error_then_healthy_is_ready() {
    let fake = FakeLlm::start(
        vec![],
        vec![
            (500, json!({"error":"timeout"})),
            (200, json!({"phase":"warming","healthy":false})),
            (200, json!({"phase":"idle","healthy":true})),
        ],
    )
    .await;
    let mut c = cfg(&fake);
    c.status_url = Some(format!("{}/status", fake.url));
    LlmClient::new(c).unwrap().wait_ready().await.unwrap();
    assert_eq!(*fake.status_polls.lock().unwrap(), 3);
}

#[tokio::test]
async fn length_without_tool_call_is_token_budget() {
    let fake = FakeLlm::start(vec![(200, text_response("", "length"))], vec![]).await;
    let err = LlmClient::new(cfg(&fake))
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap_err();
    assert!(
        matches!(err, AgentError::TokenBudget { max_tokens: 1024 }),
        "{err:?}"
    );
}

#[tokio::test]
async fn http_400_is_llm_error_not_retried() {
    let fake = FakeLlm::start(vec![(400, json!({"error":"bad"}))], vec![]).await;
    let err = LlmClient::new(cfg(&fake))
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap_err();
    assert!(
        matches!(err, AgentError::Llm { status: 400, .. }),
        "{err:?}"
    );
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
}

/// A server that accepts, reads the request and then hangs (`hang`) or closes
/// without answering; returns its URL and how many connections it accepted.
async fn silent_server(hang: bool) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut sock, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            if hang {
                held.push(sock);
            }
        }
    });
    (url, accepted)
}

fn silent_cfg(url: &str, request_timeout: Duration, ready_timeout: Duration) -> LlmConfig {
    let mut c = LlmConfig::new(url, "floor-model");
    c.backoff_initial = Duration::from_millis(10);
    c.backoff_max = Duration::from_millis(10);
    c.request_timeout = request_timeout;
    c.ready_timeout = ready_timeout;
    c
}

#[tokio::test]
async fn request_timeout_is_unreachable_not_retried() {
    let (url, accepted) = silent_server(true).await;
    let c = silent_cfg(&url, Duration::from_millis(200), Duration::from_secs(5));
    let err = LlmClient::new(c)
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::Unreachable { .. }), "{err:?}");
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn closed_without_answer_is_unreachable_not_retried() {
    let (url, accepted) = silent_server(false).await;
    let c = silent_cfg(&url, Duration::from_secs(5), Duration::from_secs(5));
    let err = LlmClient::new(c)
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::Unreachable { .. }), "{err:?}");
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn attempt_is_bounded_by_ready_timeout() {
    let (url, accepted) = silent_server(true).await;
    let c = silent_cfg(&url, Duration::from_secs(300), Duration::from_millis(200));
    let started = std::time::Instant::now();
    let err = LlmClient::new(c)
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::NotReady { .. }), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn connection_refused_is_waited_out() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let c = silent_cfg(&url, Duration::from_secs(5), Duration::from_millis(100));
    let err = LlmClient::new(c)
        .unwrap()
        .complete(&msgs(), &tools())
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::NotReady { .. }), "{err:?}");
}

#[tokio::test]
async fn hung_status_poll_is_bounded_by_ready_timeout() {
    let (url, _) = silent_server(true).await;
    let mut c = silent_cfg(&url, Duration::from_secs(300), Duration::from_millis(200));
    c.status_url = Some(format!("{url}/status"));
    let started = std::time::Instant::now();
    let err = LlmClient::new(c).unwrap().wait_ready().await.unwrap_err();
    assert!(matches!(err, AgentError::NotReady { .. }), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}
