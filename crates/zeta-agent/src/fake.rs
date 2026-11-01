//! Fake OpenAI-compatible server for tests (feature `test-fake`).

use crate::exec::Executor;
use crate::llm::AgentError;
use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

type Canned = Arc<Mutex<VecDeque<(u16, Value)>>>;

/// Request-keyed scripts: (a substring of the last user message, its replies in order).
pub type Scripts = Vec<(String, Vec<(u16, Value)>)>;

#[derive(Clone)]
struct Fake {
    chat: Canned,
    status: Canned,
    requests: Arc<Mutex<Vec<Value>>>,
    status_polls: Arc<Mutex<usize>>,
    /// Refills `chat` when it runs out (`serve_loop`); empty for `FakeLlm`.
    repeat: Arc<Vec<(u16, Value)>>,
    /// When set, chat answers come from these instead of `chat` (`start_by_request`).
    scripts: Option<Arc<Scripts>>,
}

/// A loopback chat server answering canned responses in order.
pub struct FakeLlm {
    /// `http://127.0.0.1:PORT`; the status route is `{url}/status`.
    pub url: String,
    /// Every chat request body, in order.
    pub requests: Arc<Mutex<Vec<Value>>>,
    /// How many times `GET /status` was answered.
    pub status_polls: Arc<Mutex<usize>>,
}

impl FakeLlm {
    /// Chat answers are consumed in order, then `500 {"error":"fake exhausted"}`;
    /// status answers are consumed in order and the last one repeats.
    pub async fn start(chat: Vec<(u16, Value)>, status: Vec<(u16, Value)>) -> FakeLlm {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let fake = fake(chat, status);
        let out = FakeLlm {
            url: format!("http://{addr}"),
            requests: fake.requests.clone(),
            status_polls: fake.status_polls.clone(),
        };
        tokio::spawn(async move { axum::serve(listener, router(fake)).await });
        out
    }

    /// Each chat request is answered from the first script whose key is a substring of the
    /// last `user` message: its reply `k` when the request holds `k` assistant messages after
    /// that user message, so every conversation (one agent run) starts at the script's first
    /// reply. `500 {"error":"fake: no script for <text>"}` when no key matches, `500
    /// {"error":"fake: script exhausted for <text>"}` past its last reply. Status answers as
    /// in [`FakeLlm::start`].
    pub async fn start_by_request(scripts: Scripts, status: Vec<(u16, Value)>) -> FakeLlm {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut fake = fake(Vec::new(), status);
        fake.scripts = Some(Arc::new(scripts));
        let out = FakeLlm {
            url: format!("http://{addr}"),
            requests: fake.requests.clone(),
            status_polls: fake.status_polls.clone(),
        };
        tokio::spawn(async move { axum::serve(listener, router(fake)).await });
        out
    }
}

/// Serve `scripts` on `listener` forever, answering as [`FakeLlm::start_by_request`] does.
/// Takes a bound listener so a caller binding port 0 can report the port first.
pub async fn serve_by_request(
    listener: tokio::net::TcpListener,
    scripts: Scripts,
) -> std::io::Result<()> {
    let mut fake = fake(Vec::new(), Vec::new());
    fake.scripts = Some(Arc::new(scripts));
    axum::serve(listener, router(fake)).await
}

/// Serve `chat` on `addr` forever, starting over each time it runs out.
pub async fn serve_loop(addr: SocketAddr, chat: Vec<(u16, Value)>) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let mut fake = fake(Vec::new(), Vec::new());
    fake.repeat = Arc::new(chat);
    axum::serve(listener, router(fake)).await
}

fn fake(chat: Vec<(u16, Value)>, status: Vec<(u16, Value)>) -> Fake {
    Fake {
        chat: Arc::new(Mutex::new(chat.into())),
        status: Arc::new(Mutex::new(status.into())),
        requests: Arc::default(),
        status_polls: Arc::default(),
        repeat: Arc::default(),
        scripts: None,
    }
}

fn router(fake: Fake) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/status", get(status))
        .with_state(fake)
}

fn reply((code, body): (u16, Value)) -> (StatusCode, Json<Value>) {
    (
        StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    )
}

/// The reply `scripts` give the chat request `body` (see [`FakeLlm::start_by_request`]).
fn scripted(scripts: &Scripts, body: &Value) -> (u16, Value) {
    let messages = body["messages"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let last_user = messages.iter().rposition(|m| m["role"] == "user");
    let text = last_user
        .and_then(|i| messages[i]["content"].as_str())
        .unwrap_or_default();
    let Some((_, replies)) = scripts.iter().find(|(key, _)| text.contains(key.as_str())) else {
        return (500, json!({"error": format!("fake: no script for {text}")}));
    };
    let turn = last_user.map_or(0, |i| {
        messages[i + 1..]
            .iter()
            .filter(|m| m["role"] == "assistant")
            .count()
    });
    replies.get(turn).cloned().unwrap_or_else(|| {
        let error = format!("fake: script exhausted for {text}");
        (500, json!({ "error": error }))
    })
}

async fn chat(State(f): State<Fake>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    if let Some(scripts) = &f.scripts {
        let next = scripted(scripts, &body);
        f.requests.lock().unwrap().push(body);
        return reply(next);
    }
    f.requests.lock().unwrap().push(body);
    let next = {
        let mut q = f.chat.lock().unwrap();
        if q.is_empty() {
            q.extend(f.repeat.iter().cloned());
        }
        q.pop_front()
    };
    reply(next.unwrap_or((500, json!({"error": "fake exhausted"}))))
}

async fn status(State(f): State<Fake>) -> (StatusCode, Json<Value>) {
    *f.status_polls.lock().unwrap() += 1;
    let mut q = f.status.lock().unwrap();
    let next = if q.len() > 1 {
        q.pop_front()
    } else {
        q.front().cloned()
    };
    reply(next.unwrap_or((404, json!({"error": "no status"}))))
}

/// An OpenAI tool-call response as llama-server emits it for a thinking
/// model: `arguments` as a JSON string, plus `reasoning_content`.
pub fn tool_call_response(calls: &[(&str, Value)]) -> Value {
    tool_call_response_with(calls, "", Some("The user wants this; call the tool."))
}

/// As `tool_call_response`, with the message's `content` and `reasoning_content` given.
pub fn tool_call_response_with(
    calls: &[(&str, Value)],
    content: &str,
    reasoning: Option<&str>,
) -> Value {
    let tool_calls: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            json!({"id": format!("call_{i}"), "type": "function", "function": {
                "name": name,
                "arguments": args.to_string(),
            }})
        })
        .collect();
    let mut message = json!({
        "role": "assistant",
        "content": content,
        "tool_calls": tool_calls,
    });
    if let Some(r) = reasoning {
        message["reasoning_content"] = json!(r);
    }
    json!({
        "choices": [{
            "index": 0,
            "finish_reason": "tool_calls",
            "message": message,
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    })
}

/// A plain text response with the given finish reason.
pub fn text_response(content: &str, finish_reason: &str) -> Value {
    json!({
        "choices": [{
            "index": 0,
            "finish_reason": finish_reason,
            "message": {"role": "assistant", "content": content},
        }],
    })
}

/// An executor that records every operation and answers from a table keyed
/// by op name; an unlisted op answers `{"result":"ok"}`.
pub struct RecordingExecutor {
    /// Every operation it was asked to apply, in order.
    pub calls: Mutex<Vec<Value>>,
    answers: HashMap<String, Result<Value, Value>>,
}

impl RecordingExecutor {
    pub fn new(answers: Vec<(&str, Result<Value, Value>)>) -> Self {
        Self {
            calls: Mutex::default(),
            answers: answers
                .into_iter()
                .map(|(op, answer)| (op.to_string(), answer))
                .collect(),
        }
    }
}

#[async_trait]
impl Executor for RecordingExecutor {
    async fn apply(&self, op: Value) -> Result<Result<Value, Value>, AgentError> {
        let answer = op["op"]
            .as_str()
            .and_then(|name| self.answers.get(name).cloned())
            .unwrap_or_else(|| Ok(json!({"result": "ok"})));
        self.calls.lock().unwrap().push(op);
        Ok(answer)
    }
}
