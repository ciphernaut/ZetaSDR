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
#[serde(tag = "cause", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum GraphError {
    #[error("unknown stage kind {kind:?} on node {node:?}; known: {known:?}")]
    UnknownStage {
        node: NodeId,
        kind: String,
        known: Vec<String>,
    },
    #[error("edge {edge} references a missing node")]
    DanglingEdge { edge: usize },
    #[error("edge {edge} carries {got:?} but {expected:?} was expected")]
    TypeMismatch {
        edge: usize,
        expected: StreamKind,
        got: StreamKind,
    },
    #[error("bad params on node {node:?}: {reason}")]
    BadParams { node: NodeId, reason: String },
    #[error("node id {node:?} is used more than once")]
    DuplicateNode { node: NodeId },
    #[error("root node {node:?} must accept iq input")]
    RootNeedsIq { node: NodeId },
    #[error("graph has no nodes")]
    Empty,
    #[error("edges form a cycle")]
    Cycle,
}

/// Check kinds, params, endpoints, edge types, unique node ids, that every root accepts IQ
/// (or takes no input at all, as `stream_in`, which reads the bus itself), that there is
/// at least one node, and that the edges form no cycle, in that order.
pub fn validate(graph: &Graph, registry: &StageRegistry) -> Result<(), GraphError> {
    for n in &graph.nodes {
        let spec = registry
            .spec(&n.kind)
            .ok_or_else(|| GraphError::UnknownStage {
                node: n.id.clone(),
                kind: n.kind.clone(),
                known: registry.kinds(),
            })?;
        let bad = |reason: String| GraphError::BadParams {
            node: n.id.clone(),
            reason,
        };
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
        return Err(GraphError::TypeMismatch {
            edge: i,
            expected,
            got: e.kind,
        });
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(n) = graph.nodes.iter().find(|n| !seen.insert(&n.id)) {
        return Err(GraphError::DuplicateNode { node: n.id.clone() });
    }
    for n in &graph.nodes {
        if graph.edges.iter().any(|e| e.to == n.id) {
            continue;
        }
        let can_root = registry
            .spec(&n.kind)
            .is_some_and(|s| s.inputs.is_empty() || s.inputs.contains(&StreamKind::Iq));
        if !can_root {
            return Err(GraphError::RootNeedsIq { node: n.id.clone() });
        }
    }
    if graph.nodes.is_empty() {
        return Err(GraphError::Empty);
    }
    if has_cycle(graph) {
        return Err(GraphError::Cycle);
    }
    Ok(())
}

/// Whether `a` and `b` are the same pipeline whatever their graph and node ids: the same
/// node kinds and params in the same order, and the same edges (by node position and
/// stream kind, in any order).
pub fn same_pipeline(a: &Graph, b: &Graph) -> bool {
    if a.nodes.len() != b.nodes.len()
        || a.edges.len() != b.edges.len()
        || a.nodes
            .iter()
            .zip(&b.nodes)
            .any(|(x, y)| x.kind != y.kind || x.params != y.params)
    {
        return false;
    }
    let edges = |g: &Graph| {
        let at = |id: &NodeId| g.nodes.iter().position(|n| &n.id == id);
        let mut v: Vec<_> = g
            .edges
            .iter()
            .map(|e| (at(&e.from), at(&e.to), format!("{:?}", e.kind)))
            .collect();
        v.sort();
        v
    };
    edges(a) == edges(b)
}

/// Kahn's algorithm: the edges form a cycle if some node never reaches
/// in-degree zero. Node ids are unique and every edge endpoint exists.
fn has_cycle(graph: &Graph) -> bool {
    use std::collections::HashMap;
    let mut indegree: HashMap<&NodeId, usize> = graph.nodes.iter().map(|n| (&n.id, 0)).collect();
    for e in &graph.edges {
        *indegree.entry(&e.to).or_default() += 1;
    }
    let mut ready: Vec<&NodeId> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut done = 0;
    while let Some(n) = ready.pop() {
        done += 1;
        for e in graph.edges.iter().filter(|e| &e.from == n) {
            let d = indegree.get_mut(&e.to).expect("endpoint exists");
            *d -= 1;
            if *d == 0 {
                ready.push(&e.to);
            }
        }
    }
    done < indegree.len()
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
        Node {
            id: NodeId(id.into()),
            kind: kind.into(),
            params,
        }
    }

    #[test]
    fn same_pipeline_ignores_ids_only() {
        let g = |ids: [&str; 3], kind: &str, edge_kind: StreamKind| Graph {
            id: GraphId(ids[0].into()),
            nodes: vec![
                node(ids[1], "passthrough", serde_json::json!({"kind": kind})),
                node(ids[2], "passthrough", serde_json::json!({"kind": kind})),
            ],
            edges: vec![Edge {
                from: NodeId(ids[1].into()),
                to: NodeId(ids[2].into()),
                kind: edge_kind,
            }],
        };
        let a = g(["g1", "a", "b"], "iq", StreamKind::Iq);
        assert!(same_pipeline(
            &a,
            &g(["g2", "x", "y"], "iq", StreamKind::Iq)
        ));
        assert!(!same_pipeline(
            &a,
            &g(["g1", "a", "b"], "audio", StreamKind::Iq)
        ));
        assert!(!same_pipeline(
            &a,
            &g(["g1", "a", "b"], "iq", StreamKind::Audio)
        ));
        let mut reversed = a.clone();
        reversed.edges[0] = Edge {
            from: NodeId("b".into()),
            to: NodeId("a".into()),
            kind: StreamKind::Iq,
        };
        assert!(!same_pipeline(&a, &reversed));
        let mut more = a.clone();
        more.nodes
            .push(node("c", "passthrough", serde_json::json!({"kind": "iq"})));
        assert!(!same_pipeline(&a, &more));
    }

    #[test]
    fn graph_roundtrips_json() {
        let g = Graph {
            id: GraphId("g1".into()),
            nodes: vec![
                node("a", "passthrough", serde_json::json!({"kind": "iq"})),
                node("b", "passthrough", serde_json::json!({"kind": "iq"})),
            ],
            edges: vec![Edge {
                from: NodeId("a".into()),
                to: NodeId("b".into()),
                kind: StreamKind::Iq,
            }],
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
            Err(GraphError::UnknownStage { kind, known, .. }) => {
                assert_eq!(kind, "nope");
                assert_eq!(known, vec!["passthrough".to_string()]);
            }
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
            _: crate::stage::StageContext,
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
            edges: vec![Edge {
                from: NodeId("a".into()),
                to: NodeId("b".into()),
                kind: StreamKind::Iq,
            }],
        };
        assert_eq!(
            validate(&g, &r),
            Err(GraphError::TypeMismatch {
                edge: 0,
                expected: StreamKind::Audio,
                got: StreamKind::Iq
            })
        );
        // `from` lacking the kind reports its first output.
        let g2 = Graph {
            edges: vec![Edge {
                from: NodeId("a".into()),
                to: NodeId("b".into()),
                kind: StreamKind::Audio,
            }],
            ..g.clone()
        };
        assert_eq!(
            validate(&g2, &r),
            Err(GraphError::TypeMismatch {
                edge: 0,
                expected: StreamKind::Iq,
                got: StreamKind::Audio
            })
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
                node("p", "passthrough", serde_json::json!({"kind": "iq"})),
                n("s", "sink_only"),
            ],
            edges: vec![Edge {
                from: NodeId(from.into()),
                to: NodeId(to.into()),
                kind: StreamKind::Iq,
            }],
        };
        let want = Err(GraphError::TypeMismatch {
            edge: 0,
            expected: StreamKind::Iq,
            got: StreamKind::Iq,
        });
        assert_eq!(validate(&mk("s", "p"), &r), want);
        assert_eq!(validate(&mk("p", "s"), &r), want);
    }

    #[test]
    fn bad_params_and_dangling_edges_are_rejected() {
        let r = registry();
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![node(
                "a",
                "passthrough",
                serde_json::json!({"kind": "Nope"}),
            )],
            edges: vec![],
        };
        assert!(matches!(
            validate(&g, &r),
            Err(GraphError::BadParams { .. })
        ));
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![node("a", "passthrough", serde_json::json!({"kind": "iq"}))],
            edges: vec![Edge {
                from: NodeId("a".into()),
                to: NodeId("z".into()),
                kind: StreamKind::Iq,
            }],
        };
        assert_eq!(validate(&g, &r), Err(GraphError::DanglingEdge { edge: 0 }));
    }

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: NodeId(from.into()),
            to: NodeId(to.into()),
            kind: StreamKind::Iq,
        }
    }

    fn iq(id: &str) -> Node {
        node(id, "passthrough", serde_json::json!({"kind": "iq"}))
    }

    #[test]
    fn duplicate_node_ids_are_rejected() {
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![iq("a"), iq("b"), iq("a")],
            edges: vec![edge("a", "b")],
        };
        assert_eq!(
            validate(&g, &registry()),
            Err(GraphError::DuplicateNode {
                node: NodeId("a".into())
            })
        );
    }

    #[test]
    fn root_node_without_iq_input_is_rejected() {
        let mut r = registry();
        r.register(fixed("audio_only", StreamKind::Audio, StreamKind::Audio));
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![node("a", "audio_only", serde_json::json!({}))],
            edges: vec![],
        };
        assert_eq!(
            validate(&g, &r),
            Err(GraphError::RootNeedsIq {
                node: NodeId("a".into())
            })
        );
    }

    #[test]
    fn a_root_taking_no_input_is_accepted() {
        let mut r = registry();
        r.register(Arc::new(Fixed(StageSpec {
            kind: "bus_reader",
            inputs: vec![],
            outputs: vec![StreamKind::Audio],
            params_schema: schemars::json_schema!({"type": "object"}),
        })));
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![node("a", "bus_reader", serde_json::json!({}))],
            edges: vec![],
        };
        assert_eq!(validate(&g, &r), Ok(()));
    }

    #[test]
    fn empty_graph_is_rejected() {
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![],
            edges: vec![],
        };
        assert_eq!(validate(&g, &registry()), Err(GraphError::Empty));
    }

    #[test]
    fn cycles_are_rejected() {
        let r = registry();
        let mk = |edges| Graph {
            id: GraphId::new(),
            nodes: vec![iq("a"), iq("b"), iq("c")],
            edges,
        };
        let ring = mk(vec![edge("a", "b"), edge("b", "c"), edge("c", "b")]);
        assert_eq!(validate(&ring, &r), Err(GraphError::Cycle));
        assert_eq!(
            validate(&mk(vec![edge("a", "a")]), &r),
            Err(GraphError::Cycle)
        );
        let chain = mk(vec![edge("a", "b"), edge("b", "c"), edge("a", "c")]);
        assert_eq!(validate(&chain, &r), Ok(()));
    }
}
