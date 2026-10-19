//! LLM agent loop for ZetaSDR.

pub mod agent;
pub mod exec;
#[cfg(feature = "test-fake")]
pub mod fake;
pub mod llm;
pub mod wire;

pub use agent::{
    Agent, AgentConfig, AgentFailure, CallRecord, Transcript, build_agent, is_question,
};
pub use exec::{Executor, LocalExecutor, RemoteExecutor};
pub use llm::{AgentError, Completion, LlmClient, LlmConfig};
pub use wire::{ChatMessage, FunctionCall, ToolCall};
