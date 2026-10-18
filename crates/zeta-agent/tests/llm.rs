//! The chat client against the fake OpenAI-compatible server over loopback.
use serde_json::{Value, json};
use std::time::Duration;
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
