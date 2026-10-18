//! The agent loop: model turns, tool calls applied through an executor, errors fed back.

use crate::exec::{Executor, to_json};
use crate::llm::{AgentError, LlmClient};
use crate::wire::{ChatMessage, ToolCall};
use serde::Serialize;
use serde_json::Value;
use zeta_client::{ToolDef, op_from_call};
use zeta_core::ops::OpError;

pub struct AgentConfig {
    pub system_prompt: String,
    pub tools: Vec<ToolDef>,
    pub max_turns: u32,
}

impl AgentConfig {
    /// `max_turns` defaults to 8.
    pub fn new(system_prompt: impl Into<String>, tools: Vec<ToolDef>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            tools,
            max_turns: 8,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CallRecord {
    pub tool: String,
    pub arguments: Value,
    pub ok: bool,
    pub result: Value,
}

#[derive(Debug, Serialize)]
pub struct Transcript {
    pub request: String,
    pub answer: Option<String>,
    pub calls: Vec<CallRecord>,
    /// How many completions were requested.
    pub turns: u32,
}

/// Why a run stopped, with everything done up to that point.
#[derive(Debug, Serialize, thiserror::Error)]
#[error("{error}")]
pub struct AgentFailure {
    #[serde(flatten)]
    pub error: AgentError,
    pub transcript: Transcript,
}

const QUESTION_WORDS: &[&str] = &[
    "what", "which", "where", "when", "who", "whom", "whose", "why", "how", "is", "are", "was",
    "were", "do", "does", "did", "can", "could", "will", "would", "should", "has", "have", "any",
];

/// Whether `request` asks something rather than commands something: it ends
/// with `?`, or its first word is a question word.
pub fn is_question(request: &str) -> bool {
    let text = request.trim();
    if text.ends_with('?') {
        return true;
    }
    let first = text.split_whitespace().next().unwrap_or("");
    let word = first
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .to_lowercase();
    QUESTION_WORDS.contains(&word.as_str())
}

pub struct Agent<E: Executor> {
    llm: LlmClient,
    exec: E,
    cfg: AgentConfig,
}

impl<E: Executor> Agent<E> {
    pub fn new(llm: LlmClient, exec: E, cfg: AgentConfig) -> Self {
        Self { llm, exec, cfg }
    }

    /// Run `request` to an answer or a failure.
    pub async fn run(&self, request: &str) -> Result<Transcript, AgentFailure> {
        let mut transcript = Transcript {
            request: request.to_string(),
            answer: None,
            calls: Vec::new(),
            turns: 0,
        };
        match self.turns(&mut transcript).await {
            Ok(()) => Ok(transcript),
            Err(error) => Err(AgentFailure { error, transcript }),
        }
    }

    async fn turns(&self, t: &mut Transcript) -> Result<(), AgentError> {
        self.llm.wait_ready().await?;
        let mut messages = vec![
            ChatMessage::system(self.cfg.system_prompt.clone()),
            ChatMessage::user(t.request.clone()),
        ];
        while t.turns < self.cfg.max_turns {
            t.turns += 1;
            let c = self.llm.complete(&messages, &self.cfg.tools).await?;
            if c.tool_calls.is_empty() {
                // Design 10: prose is an answer only to a question or after
                // tool calls; it is never parsed for operations.
                let content = c.content.unwrap_or_default();
                let text = content.trim();
                if !text.is_empty() && (!t.calls.is_empty() || is_question(&t.request)) {
                    t.answer = Some(text.to_string());
                    return Ok(());
                }
                return Err(AgentError::NoToolCall { content });
            }
            // `reasoning_content` is never sent back.
            messages.push(ChatMessage {
                role: "assistant".into(),
                content: c.content,
                tool_calls: c.tool_calls.clone(),
                tool_call_id: None,
            });
            for call in &c.tool_calls {
                let record = self.call(call).await?;
                // Compact JSON, as `serde_json::to_string` writes it.
                let content = record.result.to_string();
                messages.push(ChatMessage::tool(call.id.clone(), content));
                t.calls.push(record);
            }
        }
        Err(AgentError::TurnLimit {
            turns: self.cfg.max_turns,
        })
    }

    /// Apply one tool call. Bad arguments and unknown tools are the model's
    /// fault: they become the call's result and never reach the node.
    async fn call(&self, call: &ToolCall) -> Result<CallRecord, AgentError> {
        let f = &call.function;
        let (arguments, outcome) = match f.args() {
            Err(message) => (f.arguments.clone(), Err(OpError::Other { message })),
            Ok(args) => {
                let op = op_from_call(&f.name, args.clone(), &self.cfg.tools);
                (args, op)
            }
        };
        let result = match outcome {
            Ok(op) => self.exec.apply(op).await?,
            Err(e) => Err(to_json(&e)),
        };
        let ok = result.is_ok();
        Ok(CallRecord {
            tool: f.name.clone(),
            arguments,
            ok,
            result: result.unwrap_or_else(|e| e),
        })
    }
}
