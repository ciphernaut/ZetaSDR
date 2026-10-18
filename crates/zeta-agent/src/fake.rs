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

#[derive(Clone)]
struct Fake {
    chat: Canned,
    status: Canned,
    requests: Arc<Mutex<Vec<Value>>>,
    status_polls: Arc<Mutex<usize>>,
    /// Refills `chat` when it runs out (`serve_loop`); empty for `FakeLlm`.
    repeat: Arc<Vec<(u16, Value)>>,
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

async fn chat(State(f): State<Fake>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
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
    json!({
        "choices": [{
            "index": 0,
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": "",
                "reasoning_content": "The user wants this; call the tool.",
                "tool_calls": tool_calls,
            },
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
