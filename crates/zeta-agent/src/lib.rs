//! LLM agent loop for ZetaSDR.

#[cfg(feature = "test-fake")]
pub mod fake;
pub mod llm;
pub mod wire;

pub use llm::{AgentError, Completion, LlmClient, LlmConfig};
pub use wire::{ChatMessage, FunctionCall, ToolCall};
