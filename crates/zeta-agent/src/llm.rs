//! OpenAI-compatible chat client with readiness and retry.

use crate::wire::{ChatMessage, ChatResponse, ToolCall};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use zeta_client::ToolDef;

/// Where and how to reach the model (design 10 sets the defaults).
#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub url: String,
    pub model: String,
    pub key: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub thinking: bool,
    pub status_url: Option<String>,
    pub ready_timeout: Duration,
    pub request_timeout: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
}

impl LlmConfig {
    pub fn new(url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            model: model.into(),
            key: None,
            max_tokens: 1024,
            temperature: 0.0,
            thinking: false,
            status_url: None,
            // Model loads from SD took four to six minutes (design 10).
            ready_timeout: Duration::from_secs(600),
            // Probe latencies reached 142 s.
            request_timeout: Duration::from_secs(300),
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(15),
        }
    }

    /// The chat completions URL: taken as is if it already ends in
    /// `/chat/completions`, else `/v1/chat/completions` is appended (`/v1` once).
    pub fn endpoint(&self) -> String {
        if self.url.ends_with("/chat/completions") {
            return self.url.clone();
        }
        let base = self.url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        }
    }
}

/// Bound on one `status_url` poll, so a hung manager cannot stall readiness.
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);

/// One model turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub reasoning: Option<String>,
}

#[derive(Debug, Serialize, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum AgentError {
    #[error(
        "the model answered in prose instead of calling a tool; use a model with tool calling: {content}"
    )]
    NoToolCall { content: String },
    #[error(
        "the model spent its {max_tokens}-token budget without calling a tool; raise --llm-max-tokens or turn thinking off"
    )]
    TokenBudget { max_tokens: u32 },
    #[error("the agent reached its limit of {turns} turns without finishing")]
    TurnLimit { turns: u32 },
    #[error(
        "the model server was not ready after {waited_s} s (last: {last}); wait for the model to load or raise the ready timeout"
    )]
    NotReady { waited_s: u64, last: String },
    #[error("the model server answered HTTP {status}: {body}")]
    Llm { status: u16, body: String },
    #[error("no usable answer from the model server at {url}: {detail}")]
    Unreachable { url: String, detail: String },
    #[error("the model server sent an unreadable response: {detail}")]
    BadResponse { detail: String },
    #[error("node error: {detail}")]
    Node { detail: String },
}

pub struct LlmClient {
    cfg: LlmConfig,
    http: reqwest::Client,
}

impl LlmClient {
    pub fn new(cfg: LlmConfig) -> Result<Self, AgentError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| AgentError::Unreachable {
                url: cfg.endpoint(),
                detail: error_chain(&e),
            })?;
        Ok(Self { cfg, http })
    }

    pub fn request_body(&self, messages: &[ChatMessage], tools: &[ToolDef]) -> Value {
        let tools: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }})
            })
            .collect();
        let mut body = json!({
            "model": self.cfg.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "max_tokens": self.cfg.max_tokens,
            "temperature": self.cfg.temperature,
        });
        if !self.cfg.thinking {
            body["chat_template_kwargs"] = json!({"enable_thinking": false});
        }
        body
    }

    /// Poll `status_url` until it reports `"healthy": true`; anything else,
    /// a hung poll included, is "not yet" (design 10: readiness is a
    /// sequence, not a flag).
    pub async fn wait_ready(&self) -> Result<(), AgentError> {
        let Some(url) = &self.cfg.status_url else {
            return Ok(());
        };
        let mut wait = Backoff::new(&self.cfg);
        loop {
            let poll = self
                .http
                .get(url)
                .timeout(STATUS_TIMEOUT.min(wait.remaining()));
            let last = match poll.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    match resp.text().await {
                        Ok(text) => {
                            let healthy = serde_json::from_str::<Value>(&text)
                                .is_ok_and(|v| v["healthy"] == json!(true));
                            if status.is_success() && healthy {
                                return Ok(());
                            }
                            format!("HTTP {}: {text}", status.as_u16())
                        }
                        Err(e) => error_chain(&e),
                    }
                }
                Err(e) => error_chain(&e),
            };
            wait.next(last).await?;
        }
    }

    /// One chat completion, waiting out 503 and connection errors. Each
    /// attempt is bounded by `request_timeout` and by what is left of
    /// `ready_timeout`; any other transport failure is not retried.
    pub async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<Completion, AgentError> {
        let body = self.request_body(messages, tools);
        let url = self.cfg.endpoint();
        let mut wait = Backoff::new(&self.cfg);
        // The previous attempt's failure, kept when the deadline cuts the
        // last attempt short, so `NotReady` names what the server said.
        let mut last = String::new();
        loop {
            let limit = self.cfg.request_timeout.min(wait.remaining());
            let clipped = limit < self.cfg.request_timeout;
            let mut req = self.http.post(&url).json(&body).timeout(limit);
            if let Some(key) = &self.cfg.key {
                req = req.bearer_auth(key);
            }
            let fail = |e: reqwest::Error| {
                if e.is_timeout() && clipped {
                    let text = error_chain(&e);
                    wait.not_ready(if last.is_empty() { text } else { last.clone() })
                } else if e.is_timeout() {
                    AgentError::Unreachable {
                        url: url.clone(),
                        detail: format!("no answer within {} s", limit.as_secs()),
                    }
                } else {
                    AgentError::Unreachable {
                        url: url.clone(),
                        detail: error_chain(&e),
                    }
                }
            };
            last = match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    match resp.text().await {
                        Ok(text) if status == 503 => text,
                        Err(e) if status == 503 => error_chain(&e),
                        Ok(text) if !(200..300).contains(&status) => {
                            return Err(AgentError::Llm { status, body: text });
                        }
                        Ok(text) => return decode(&text, self.cfg.max_tokens),
                        Err(e) => return Err(fail(e)),
                    }
                }
                Err(e) if e.is_connect() => error_chain(&e),
                Err(e) => return Err(fail(e)),
            };
            wait.next(last.clone()).await?;
        }
    }
}

/// Doubling backoff measured against `ready_timeout` from the first attempt.
struct Backoff {
    start: Instant,
    delay: Duration,
    max: Duration,
    timeout: Duration,
}

impl Backoff {
    fn new(cfg: &LlmConfig) -> Self {
        Self {
            start: Instant::now(),
            delay: cfg.backoff_initial,
            max: cfg.backoff_max,
            timeout: cfg.ready_timeout,
        }
    }

    /// What is left of the readiness window.
    fn remaining(&self) -> Duration {
        self.timeout.saturating_sub(self.start.elapsed())
    }

    fn not_ready(&self, last: String) -> AgentError {
        AgentError::NotReady {
            waited_s: self.start.elapsed().as_secs(),
            last,
        }
    }

    async fn next(&mut self, last: String) -> Result<(), AgentError> {
        let elapsed = self.start.elapsed();
        if elapsed >= self.timeout {
            return Err(self.not_ready(last));
        }
        tokio::time::sleep(self.delay.min(self.timeout - elapsed)).await;
        self.delay = (self.delay * 2).min(self.max);
        Ok(())
    }
}

fn decode(text: &str, max_tokens: u32) -> Result<Completion, AgentError> {
    let resp: ChatResponse = serde_json::from_str(text).map_err(|e| AgentError::BadResponse {
        detail: format!("{e}: {text}"),
    })?;
    let Some(choice) = resp.choices.into_iter().next() else {
        return Err(AgentError::BadResponse {
            detail: format!("no choices: {text}"),
        });
    };
    let tool_calls = choice.message.tool_calls.unwrap_or_default();
    if choice.finish_reason.as_deref() == Some("length") && tool_calls.is_empty() {
        return Err(AgentError::TokenBudget { max_tokens });
    }
    Ok(Completion {
        content: choice.message.content,
        tool_calls,
        finish_reason: choice.finish_reason,
        reasoning: choice.message.reasoning_content,
    })
}

/// An error and its sources, so a transport failure names its cause.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_forms() {
        let ep = |u: &str| LlmConfig::new(u, "m").endpoint();
        assert_eq!(ep("http://h:8080"), "http://h:8080/v1/chat/completions");
        assert_eq!(ep("http://h:8080/v1"), "http://h:8080/v1/chat/completions");
        assert_eq!(
            ep("http://h:8080/v1/chat/completions"),
            "http://h:8080/v1/chat/completions"
        );
        assert_eq!(ep("http://h:4000/"), "http://h:4000/v1/chat/completions");
    }
}
