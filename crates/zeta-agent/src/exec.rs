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

/// A node in this process. Its `ops` records carry `transport` (see
/// `Node::apply_from`): `"local"` from [`LocalExecutor::new`].
pub struct LocalExecutor {
    node: Arc<Node>,
    transport: &'static str,
}

impl LocalExecutor {
    /// Calls logged as `"local"`.
    pub fn new(node: Arc<Node>) -> Self {
        Self::tagged(node, "local")
    }

    /// Calls logged with `transport`, e.g. `"agent"` for the server's `/ask`.
    pub fn tagged(node: Arc<Node>, transport: &'static str) -> Self {
        Self { node, transport }
    }
}

#[async_trait]
impl Executor for LocalExecutor {
    async fn apply(&self, op: Value) -> Result<Result<Value, Value>, AgentError> {
        let op = match parse_op(op) {
            Ok(op) => op,
            Err(e) => return Ok(Err(to_json(&e))),
        };
        Ok(match self.node.apply_from(op, self.transport).await {
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

/// How many capture records a trimmed `capture_list` keeps for the model (the newest).
pub const CAPTURE_LIST_KEEP: usize = 20;

/// Trim a result before the model sees it: a `job_list` loses each record's
/// `graph` (a graph is several hundred bytes the model never needs; `graph_get`
/// fetches one); a `capture_list` keeps the newest [`CAPTURE_LIST_KEEP`] records (the
/// node lists newest first), each without `allocations`, `meta_path` and `data_path`
/// (`capture_show` has them), and gains `count`, how many the store holds. Everything
/// else passes unchanged.
pub fn trim_result(op: &str, result: &mut Value) {
    let drop: &[&str] = match op {
        "job_list" => &["graph"],
        "capture_list" => &["allocations", "meta_path", "data_path"],
        _ => return,
    };
    let Some(records) = result.get_mut("value").and_then(Value::as_array_mut) else {
        return;
    };
    let count = records.len();
    if op == "capture_list" {
        records.truncate(CAPTURE_LIST_KEEP);
    }
    for r in records.iter_mut() {
        if let Some(o) = r.as_object_mut() {
            for k in drop {
                o.remove(*k);
            }
        }
    }
    if op == "capture_list"
        && let Some(o) = result.as_object_mut()
    {
        o.insert("count".into(), count.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_list_is_trimmed_to_the_newest_twenty_with_a_count() {
        let records: Vec<Value> = (0..30)
            .map(|i| {
                json!({"id": format!("c{i:02}"), "allocations": [{"x": 1}],
                       "meta_path": "/a.sigmf-meta", "data_path": "/a.sigmf-data",
                       "center_hz": 1})
            })
            .collect();
        let mut r = json!({"kind": "captures", "value": records});
        trim_result("capture_list", &mut r);
        let v = r["value"].as_array().unwrap();
        assert_eq!(v.len(), CAPTURE_LIST_KEEP);
        assert_eq!(v[0]["id"], "c00");
        assert_eq!(r["count"], 30);
        for rec in v {
            let o = rec.as_object().unwrap();
            assert!(!o.contains_key("allocations") && !o.contains_key("meta_path"));
            assert!(!o.contains_key("data_path"));
            assert_eq!(rec["center_hz"], 1);
        }
    }

    #[test]
    fn other_results_pass_unchanged() {
        let mut r = json!({"kind": "captures", "value": [{"allocations": []}]});
        let before = r.clone();
        trim_result("capture_show", &mut r);
        assert_eq!(r, before);
    }
}

/// An operation error as JSON; one that cannot be serialised becomes `other`.
pub(crate) fn to_json(e: &(impl Serialize + std::fmt::Display)) -> Value {
    serde_json::to_value(e).unwrap_or_else(|_| json!({"error": "other", "message": e.to_string()}))
}
