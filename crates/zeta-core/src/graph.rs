//! Graph document: the serialised pipeline an LLM reads and writes and the GUI renders.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::stage::StageRegistry;
use crate::types::StreamKind;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        pub struct $name(pub String);

        impl $name {
            /// Generate a fresh ULID-backed id.
            pub fn new() -> Self {
                Self(ulid::Ulid::generate().to_string())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

string_id!(GraphId);
string_id!(NodeId);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Node {
    pub id: NodeId,
    pub kind: String,
    pub params: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: StreamKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Graph {
    pub id: GraphId,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum GraphError {
    #[error("unknown stage kind {kind:?} on node {node:?}")]
    UnknownStage { node: NodeId, kind: String },
    #[error("edge {edge} references a missing node")]
    DanglingEdge { edge: usize },
    #[error("edge {edge} carries {got:?} but {expected:?} was expected")]
    TypeMismatch { edge: usize, expected: StreamKind, got: StreamKind },
    #[error("bad params on node {node:?}: {reason}")]
    BadParams { node: NodeId, reason: String },
}

/// Check kinds, params, endpoints and edge types, in that order.
pub fn validate(graph: &Graph, registry: &StageRegistry) -> Result<(), GraphError> {
    for n in &graph.nodes {
        let spec = registry
            .spec(&n.kind)
            .ok_or_else(|| GraphError::UnknownStage { node: n.id.clone(), kind: n.kind.clone() })?;
        let bad = |reason: String| GraphError::BadParams { node: n.id.clone(), reason };
        let schema = serde_json::to_value(&spec.params_schema).map_err(|e| bad(e.to_string()))?;
        let validator = jsonschema::validator_for(&schema).map_err(|e| bad(e.to_string()))?;
        if let Some(e) = validator.iter_errors(&n.params).next() {
            return Err(bad(e.to_string()));
        }
    }
    let find = |id: &NodeId| graph.nodes.iter().find(|n| &n.id == id);
    let mut ends = Vec::with_capacity(graph.edges.len());
    for (i, e) in graph.edges.iter().enumerate() {
        match (find(&e.from), find(&e.to)) {
            (Some(f), Some(t)) => ends.push((f, t)),
            _ => return Err(GraphError::DanglingEdge { edge: i }),
        }
    }
    for (i, (e, (f, t))) in graph.edges.iter().zip(ends).enumerate() {
        // Kinds were all checked above, so both specs exist.
        let (fs, ts) = match (registry.spec(&f.kind), registry.spec(&t.kind)) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        let expected = if !fs.outputs.contains(&e.kind) {
            fs.outputs.first()
        } else if !ts.inputs.contains(&e.kind) {
            ts.inputs.first()
        } else {
            continue;
        };
        // A stage with no ports on the offending side still rejects the edge.
        let expected = expected.copied().unwrap_or(e.kind);
        return Err(GraphError::TypeMismatch { edge: i, expected, got: e.kind });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::{Backend, PassthroughStage, Stage, StageRegistry, StageSpec};
    use crate::types::StreamKind;
    use std::sync::Arc;

    fn registry() -> StageRegistry {
        let mut r = StageRegistry::new();
        r.register(Arc::new(PassthroughStage::new()));
        r
    }

    fn node(id: &str, kind: &str, params: serde_json::Value) -> Node {
        Node { id: NodeId(id.into()), kind: kind.into(), params }
    }

    #[test]
    fn graph_roundtrips_json() {
        let g = Graph {
            id: GraphId("g1".into()),
            nodes: vec![
                node("a", "passthrough", serde_json::json!({"kind": "Iq"})),
                node("b", "passthrough", serde_json::json!({"kind": "Iq"})),
            ],
            edges: vec![Edge { from: NodeId("a".into()), to: NodeId("b".into()), kind: StreamKind::Iq }],
        };
        let j = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<Graph>(&j).unwrap(), g);
        assert!(validate(&g, &registry()).is_ok());
        let bad = j.replace("\"nodes\"", "\"extra\":1,\"nodes\"");
        assert!(serde_json::from_str::<Graph>(&bad).is_err());
    }

    #[test]
    fn unknown_stage_kind_is_rejected_by_name() {
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![node("a", "nope", serde_json::json!({}))],
            edges: vec![],
        };
        match validate(&g, &registry()) {
            Err(GraphError::UnknownStage { kind, .. }) => assert_eq!(kind, "nope"),
            other => panic!("{other:?}"),
        }
    }

    struct Fixed(StageSpec);
    #[async_trait::async_trait]
    impl Stage for Fixed {
        fn spec(&self) -> &StageSpec {
            &self.0
        }
        fn backend(&self) -> Backend {
            Backend::Cpu
        }
        async fn run(
            &self,
            _: serde_json::Value,
            _: Vec<tokio::sync::broadcast::Receiver<crate::stream::Frame>>,
            _: Vec<crate::types::StreamId>,
            _: Arc<crate::stream::StreamBus>,
            _: tokio_util::sync::CancellationToken,
        ) -> Result<(), crate::stage::StageError> {
            Ok(())
        }
    }

    fn fixed(kind: &'static str, i: StreamKind, o: StreamKind) -> Arc<Fixed> {
        Arc::new(Fixed(StageSpec {
            kind,
            inputs: vec![i],
            outputs: vec![o],
            params_schema: schemars::json_schema!({"type": "object"}),
        }))
    }

    #[test]
    fn edge_type_mismatch_is_rejected() {
        let mut r = registry();
        r.register(fixed("iq_out", StreamKind::Text, StreamKind::Iq));
        r.register(fixed("audio_in", StreamKind::Audio, StreamKind::Text));
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![
                node("a", "iq_out", serde_json::json!({})),
                node("b", "audio_in", serde_json::json!({})),
            ],
            edges: vec![Edge { from: NodeId("a".into()), to: NodeId("b".into()), kind: StreamKind::Iq }],
        };
        assert_eq!(
            validate(&g, &r),
            Err(GraphError::TypeMismatch { edge: 0, expected: StreamKind::Audio, got: StreamKind::Iq })
        );
        // `from` lacking the kind reports its first output.
        let g2 = Graph {
            edges: vec![Edge { from: NodeId("a".into()), to: NodeId("b".into()), kind: StreamKind::Audio }],
            ..g.clone()
        };
        assert_eq!(
            validate(&g2, &r),
            Err(GraphError::TypeMismatch { edge: 0, expected: StreamKind::Iq, got: StreamKind::Audio })
        );
    }

    #[test]
    fn edge_to_stage_with_no_ports_is_mismatch() {
        let mut r = registry();
        r.register(Arc::new(Fixed(StageSpec {
            kind: "sink_only",
            inputs: vec![],
            outputs: vec![],
            params_schema: schemars::json_schema!({"type": "object"}),
        })));
        let n = |id, k| node(id, k, serde_json::json!({}));
        let mk = |from: &str, to: &str| Graph {
            id: GraphId::new(),
            nodes: vec![
                node("p", "passthrough", serde_json::json!({"kind": "Iq"})),
                n("s", "sink_only"),
            ],
            edges: vec![Edge { from: NodeId(from.into()), to: NodeId(to.into()), kind: StreamKind::Iq }],
        };
        let want = Err(GraphError::TypeMismatch { edge: 0, expected: StreamKind::Iq, got: StreamKind::Iq });
        assert_eq!(validate(&mk("s", "p"), &r), want);
        assert_eq!(validate(&mk("p", "s"), &r), want);
    }

    #[test]
    fn bad_params_and_dangling_edges_are_rejected() {
        let r = registry();
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![node("a", "passthrough", serde_json::json!({"kind": "Nope"}))],
            edges: vec![],
        };
        assert!(matches!(validate(&g, &r), Err(GraphError::BadParams { .. })));
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![node("a", "passthrough", serde_json::json!({"kind": "Iq"}))],
            edges: vec![Edge { from: NodeId("a".into()), to: NodeId("z".into()), kind: StreamKind::Iq }],
        };
        assert_eq!(validate(&g, &r), Err(GraphError::DanglingEdge { edge: 0 }));
    }
}
