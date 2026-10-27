//! A mode row's decoder expanded to a job graph.
//!
//! A `graph` row's template has its placeholders (`"$center_hz"`, `"$bandwidth_hz"`,
//! `"$out_rate_hz"`) filled with the job's integers; a `program` row becomes a `channel`
//! at the same rate into [`EXTERNAL_DECODE_KIND`], which resamples to the program's
//! `input.rate_hz`. Every node and the graph get fresh ids, as the built-in kinds'
//! graphs do: a re-expansion carries params by stage kind, never by node id.

use std::collections::HashMap;

use super::{Decoder, InputStream, ModeRow};
use crate::graph::{Edge, Graph, GraphId, Node, NodeId};
use crate::ops::OpError;
use crate::port::Tuning;
use crate::types::StreamKind;

/// The stage kind that runs a program row's decoder; its params are `{"mode": <id>}`.
pub const EXTERNAL_DECODE_KIND: &str = "external_decode";

/// `row`'s graph for `tuning` with its channel output at `out_rate_hz`, the narrowest
/// exact divisor of the source rate that `expand` picks, for both kinds of row (the
/// `channel` stage decimates by an integer only; a program row's adaptor resamples from
/// it to the program's `input.rate_hz`).
pub fn expand(row: &ModeRow, tuning: Tuning, out_rate_hz: u64) -> Result<Graph, OpError> {
    let values = [
        ("$center_hz", tuning.center_hz.0),
        ("$bandwidth_hz", tuning.bandwidth_hz.0),
        ("$out_rate_hz", out_rate_hz),
    ];
    match &row.decoder {
        Decoder::Graph { nodes, edges } => {
            let ids: HashMap<&NodeId, NodeId> =
                nodes.iter().map(|n| (&n.id, NodeId::new())).collect();
            let id = |old: &NodeId| {
                ids.get(old).cloned().ok_or_else(|| OpError::Other {
                    message: format!(
                        "mode {}: an edge names node {}, which the row lacks",
                        row.id.0, old.0
                    ),
                })
            };
            let nodes = nodes
                .iter()
                .map(|n| {
                    let mut params = n.params.clone();
                    fill(&mut params, &values);
                    Ok(Node {
                        id: id(&n.id)?,
                        kind: n.kind.clone(),
                        params,
                    })
                })
                .collect::<Result<_, OpError>>()?;
            let edges = edges
                .iter()
                .map(|e| {
                    Ok(Edge {
                        from: id(&e.from)?,
                        to: id(&e.to)?,
                        kind: e.kind,
                    })
                })
                .collect::<Result<_, OpError>>()?;
            Ok(Graph {
                id: GraphId::new(),
                nodes,
                edges,
            })
        }
        Decoder::Program(_) => {
            let (channel, decode) = (NodeId::new(), NodeId::new());
            Ok(Graph {
                id: GraphId::new(),
                nodes: vec![
                    Node {
                        id: channel.clone(),
                        kind: "channel".into(),
                        params: serde_json::json!({
                            "center_hz": tuning.center_hz.0,
                            "bandwidth_hz": tuning.bandwidth_hz.0,
                            "out_rate_hz": out_rate_hz,
                        }),
                    },
                    Node {
                        id: decode.clone(),
                        kind: EXTERNAL_DECODE_KIND.into(),
                        params: serde_json::json!({ "mode": row.id.0 }),
                    },
                ],
                edges: vec![Edge {
                    from: channel,
                    to: decode,
                    kind: StreamKind::Iq,
                }],
            })
        }
    }
}

/// Refuse a band narrower than an IQ program row needs: `band.min_hz` (else
/// `band.bandwidth_hz`) is the least, `band.bandwidth_hz` the suggestion, the source
/// rate `source_rate_hz` the most. A `graph` row takes any band its channel can cut.
pub fn check_band(row: &ModeRow, tuning: Tuning, source_rate_hz: u64) -> Result<(), OpError> {
    let Decoder::Program(block) = &row.decoder else {
        return Ok(());
    };
    if block.input.stream != InputStream::Iq {
        return Ok(());
    }
    let min = row.band.min_hz.unwrap_or(row.band.bandwidth_hz);
    let bw = tuning.bandwidth_hz.0;
    if bw >= min {
        return Ok(());
    }
    Err(OpError::OutOfRange {
        field: "bandwidth_hz".into(),
        value: bw,
        min,
        max: source_rate_hz,
        suggested: Some(row.band.bandwidth_hz),
    })
}

/// Replace every string in `v` that is one of `values`' placeholders by its integer.
fn fill(v: &mut serde_json::Value, values: &[(&str, u64)]) {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            if let Some((_, n)) = values.iter().find(|(p, _)| p == s) {
                *v = (*n).into();
            }
        }
        Value::Object(m) => m.values_mut().for_each(|x| fill(x, values)),
        Value::Array(a) => a.iter_mut().for_each(|x| fill(x, values)),
        _ => {}
    }
}
