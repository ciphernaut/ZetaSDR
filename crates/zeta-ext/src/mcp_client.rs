//! A minimal MCP client over streamable HTTP: one session per client, opened on
//! first use (`initialize`, then `notifications/initialized`), `tools/call` with
//! the `mcp-session-id` header, answers read from the SSE `data:` lines.

use std::fmt;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Mutex;

/// The protocol revision sent in `initialize`.
pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
/// The `clientInfo.name` sent in `initialize`.
pub const CLIENT_NAME: &str = "zetasdr";

const SESSION_HEADER: &str = "mcp-session-id";
const NOT_INITIALIZED: &str = "not initialized";

/// Why a call failed.
#[derive(Debug, Clone, PartialEq)]
pub enum McpError {
    /// The connection or HTTP exchange failed.
    Transport(String),
    /// The server answered a JSON-RPC error.
    Rpc { code: i64, message: String },
    /// The server did not answer in time.
    Timeout,
    /// The answer was not the expected JSON-RPC shape.
    BadBody(String),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            McpError::Transport(e) => write!(f, "mcp transport: {e}"),
            McpError::Rpc { code, message } => write!(f, "mcp error {code}: {message}"),
            McpError::Timeout => write!(f, "mcp: timed out"),
            McpError::BadBody(e) => write!(f, "mcp: bad body: {e}"),
        }
    }
}

impl std::error::Error for McpError {}

fn transport(e: reqwest::Error) -> McpError {
    if e.is_timeout() {
        McpError::Timeout
    } else {
        McpError::Transport(e.to_string())
    }
}

/// An MCP client for one server (`base_url/mcp`).
pub struct McpClient {
    http: reqwest::Client,
    base: String,
    session: Mutex<Option<String>>,
}

impl McpClient {
    /// `base_url` is `http://HOST:PORT`; `timeout` bounds every request.
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, McpError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("zetasdr/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| McpError::Transport(format!("http client: {e}")))?;
        Ok(Self {
            http,
            base: base_url.trim_end_matches('/').to_string(),
            session: Mutex::new(None),
        })
    }

    /// `GET /health` as JSON.
    pub async fn health(&self) -> Result<Value, McpError> {
        let r = self
            .http
            .get(format!("{}/health", self.base))
            .send()
            .await
            .map_err(transport)?;
        let body = r.text().await.map_err(transport)?;
        serde_json::from_str(&body).map_err(|e| McpError::BadBody(format!("health: {e}")))
    }

    /// Call `tool` with `args`: the first text content, as JSON when it parses,
    /// else as a string. A lost session is reopened once.
    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, McpError> {
        let mut session = self.session.lock().await;
        let body = json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": tool, "arguments": args},
        });
        for attempt in 0..2 {
            let sid = match session.as_ref() {
                Some(s) => s.clone(),
                None => {
                    let s = self.open().await?;
                    *session = Some(s.clone());
                    s
                }
            };
            match self.post(&body, Some(&sid)).await {
                Err(McpError::Rpc { message, .. })
                    if attempt == 0 && message.contains(NOT_INITIALIZED) =>
                {
                    *session = None;
                }
                Err(e) => return Err(e),
                Ok((_, v)) => return content_of(&v),
            }
        }
        unreachable!("the second attempt always returns")
    }

    /// `initialize` + `notifications/initialized`; the session id.
    async fn open(&self) -> Result<String, McpError> {
        let init = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": CLIENT_NAME, "version": "0"},
            },
        });
        let (sid, _) = self.post(&init, None).await?;
        let sid = sid.ok_or_else(|| McpError::BadBody("no mcp-session-id header".into()))?;
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        self.send(&note, Some(&sid)).await?;
        Ok(sid)
    }

    async fn send(&self, body: &Value, sid: Option<&str>) -> Result<reqwest::Response, McpError> {
        let mut req = self
            .http
            .post(format!("{}/mcp", self.base))
            .header("accept", "application/json, text/event-stream")
            .json(body);
        if let Some(s) = sid {
            req = req.header(SESSION_HEADER, s);
        }
        req.send().await.map_err(transport)
    }

    /// Send a request; the session id the server returned and the JSON-RPC
    /// `result`, or the `error` as [`McpError::Rpc`].
    async fn post(
        &self,
        body: &Value,
        sid: Option<&str>,
    ) -> Result<(Option<String>, Value), McpError> {
        let r = self.send(body, sid).await?;
        let new_sid = r
            .headers()
            .get(SESSION_HEADER)
            .and_then(|h| h.to_str().ok())
            .map(str::to_string);
        let text = r.text().await.map_err(transport)?;
        let msg = parse_body(&text)?;
        if let Some(e) = msg.get("error") {
            return Err(McpError::Rpc {
                code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
        let result = msg
            .get("result")
            .cloned()
            .ok_or_else(|| McpError::BadBody("no result".into()))?;
        Ok((new_sid, result))
    }
}

/// The JSON-RPC message in `text`: the last SSE `data:` line, else the whole
/// body as JSON.
fn parse_body(text: &str) -> Result<Value, McpError> {
    let data = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .next_back()
        .map(str::trim)
        .unwrap_or(text);
    serde_json::from_str(data).map_err(|e| {
        let head: String = text.chars().take(80).collect();
        McpError::BadBody(format!("{e}: {head:?}"))
    })
}

/// The first text content of a `tools/call` result.
fn content_of(result: &Value) -> Result<Value, McpError> {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|c| c.iter().find_map(|i| i.get("text").and_then(Value::as_str)))
        .ok_or_else(|| McpError::BadBody("no text content".into()))?;
    Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
}
