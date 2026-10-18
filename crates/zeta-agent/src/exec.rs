//! Executors: where the agent's operations are applied (a local node or a remote one).

use crate::llm::AgentError;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use zeta_client::{NodeClient, parse_op};
use zeta_core::node::Node;

/// Applies one operation JSON. `Ok(Ok(result))` and `Ok(Err(op_error))` are
/// both fed back to the model; `Err(AgentError::Node)` means the node itself
/// cannot be reached, which aborts the loop.
#[async_trait]
pub trait Executor: Send + Sync {
    async fn apply(&self, op: Value) -> Result<Result<Value, Value>, AgentError>;
}

#[async_trait]
impl<E: Executor + ?Sized> Executor for &E {
    async fn apply(&self, op: Value) -> Result<Result<Value, Value>, AgentError> {
        (**self).apply(op).await
    }
}

/// A node in this process.
pub struct LocalExecutor(pub Arc<Node>);

#[async_trait]
impl Executor for LocalExecutor {
    async fn apply(&self, op: Value) -> Result<Result<Value, Value>, AgentError> {
        let op = match parse_op(op) {
            Ok(op) => op,
            Err(e) => return Ok(Err(to_json(&e))),
        };
        Ok(match self.0.apply(op).await {
            Ok(r) => match serde_json::to_value(&r) {
                Ok(v) => Ok(v),
                Err(e) => Err(
                    json!({"error": "other", "message": format!("serialising the result: {e}")}),
                ),
            },
            Err(e) => Err(to_json(&e)),
        })
    }
}

/// A node served over HTTP.
pub struct RemoteExecutor(pub NodeClient);

#[async_trait]
impl Executor for RemoteExecutor {
    async fn apply(&self, op: Value) -> Result<Result<Value, Value>, AgentError> {
        self.0.apply_value(op).await.map_err(|e| AgentError::Node {
            detail: e.to_string(),
        })
    }
}

/// An operation error as JSON; one that cannot be serialised becomes `other`.
pub(crate) fn to_json(e: &(impl Serialize + std::fmt::Display)) -> Value {
    serde_json::to_value(e).unwrap_or_else(|_| json!({"error": "other", "message": e.to_string()}))
}
