//! A mode row's decoder expanded to a job graph.
//!
//! A `graph` row's template has its placeholders (`"$center_hz"`, `"$bandwidth_hz"`,
//! `"$out_rate_hz"`) filled with the job's integers; a `program` row becomes a `channel`
//! at the same rate into [`EXTERNAL_DECODE_KIND`], which resamples to the program's
//! `input.rate_hz` (an audio row's channel runs through its `demod`, an `nbfm_demod`
//! with the squelch off, first). A `frame` row becomes `channel → resample → <demod> →
//! clock_recover → slicer → frame_decode` (the resampler left out when the channel is
//! already at the chain rate). Every node and the graph get fresh ids, as the built-in
//! kinds' graphs do: a re-expansion carries params by stage kind, never by node id.

use std::collections::HashMap;

use super::{Decoder, Demod, FrameBlock, InputStream, ModeRow};
use crate::graph::{Edge, Graph, GraphId, Node, NodeId};
use crate::ops::OpError;
use crate::port::Tuning;
use crate::stage::StageRegistry;
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
        Decoder::Graph { nodes, edges, .. } => {
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
        Decoder::Program(block) => {
            let channel = Node {
                id: NodeId::new(),
                kind: "channel".into(),
                params: serde_json::json!({
                    "center_hz": tuning.center_hz.0,
                    "bandwidth_hz": tuning.bandwidth_hz.0,
                    "out_rate_hz": out_rate_hz,
                }),
            };
            let decode = Node {
                id: NodeId::new(),
                kind: EXTERNAL_DECODE_KIND.into(),
                params: serde_json::json!({ "mode": row.id.0 }),
            };
            // An audio program hears the channel through its demodulator.
            let demod = match (block.input.stream, &block.demod) {
                (InputStream::Iq, _) => None,
                (InputStream::Audio, Some(d)) => Some(demod_node(d)),
                (InputStream::Audio, None) => {
                    return Err(OpError::Other {
                        message: format!("mode {}: audio input needs a demod", row.id.0),
                    });
                }
            };
            let edge = |from: &Node, to: &Node, kind| Edge {
                from: from.id.clone(),
                to: to.id.clone(),
                kind,
            };
            let (nodes, edges) = match demod {
                None => {
                    let edges = vec![edge(&channel, &decode, StreamKind::Iq)];
                    (vec![channel, decode], edges)
                }
                Some(demod) => {
                    let edges = vec![
                        edge(&channel, &demod, StreamKind::Iq),
                        edge(&demod, &decode, StreamKind::Audio),
                    ];
                    (vec![channel, demod, decode], edges)
                }
            };
            Ok(Graph {
                id: GraphId::new(),
                nodes,
                edges,
            })
        }
        Decoder::Frame(block) => Ok(frame_graph(block, tuning, out_rate_hz)),
    }
}

/// The stream kinds a job of `row` emits. A graph row's are those of its terminal nodes
/// (no edge leaves them), from each stage's `spec().outputs`, in node order, each kind
/// once. A program row and a frame row emit events only (a frame row's audio stays
/// inside its chain).
pub fn outputs(row: &ModeRow, stages: &StageRegistry) -> Vec<StreamKind> {
    let Decoder::Graph { nodes, edges, .. } = &row.decoder else {
        return vec![StreamKind::Event];
    };
    let mut out: Vec<StreamKind> = Vec::new();
    for n in nodes
        .iter()
        .filter(|n| !edges.iter().any(|e| e.from == n.id))
    {
        let Some(spec) = stages.spec(&n.kind) else {
            continue;
        };
        for k in &spec.outputs {
            if !out.contains(k) {
                out.push(*k);
            }
        }
    }
    out
}

/// A frame row's chain: `channel` at `out_rate_hz`, `resample` to the chain rate
/// ([`crate::bits::chain_rate_hz`]; left out when the channel is at it), the three
/// [`crate::bits::decision_nodes`], `frame_decode` with the row's framing, and
/// `frame_parse` when the parser is not `opaque`.
fn frame_graph(block: &FrameBlock, tuning: Tuning, out_rate_hz: u64) -> Graph {
    use serde_json::json;
    let d = &block.demod;
    let chain = crate::bits::chain_rate_hz(out_rate_hz, d.symbol_rate_baud, d.modulation, d.order);
    let mut nodes: Vec<(&str, serde_json::Value, StreamKind)> = vec![(
        "channel",
        json!({
            "center_hz": tuning.center_hz.0,
            "bandwidth_hz": tuning.bandwidth_hz.0,
            "out_rate_hz": out_rate_hz,
        }),
        StreamKind::Iq,
    )];
    if chain != out_rate_hz {
        nodes.push(("resample", json!({"out_rate_hz": chain}), StreamKind::Iq));
    }
    let decision =
        crate::bits::decision_nodes(d.modulation, d.symbol_rate_baud, d.order, d.tone_spacing_hz);
    let leaving = [StreamKind::Audio, StreamKind::Audio, StreamKind::Event];
    nodes.extend(
        decision
            .into_iter()
            .zip(leaving)
            .map(|((_, kind, params), k)| (kind, params, k)),
    );
    nodes.push(("frame_decode", block.framing.clone(), StreamKind::Event));
    if block.parser != "opaque" {
        nodes.push((
            "frame_parse",
            json!({"parser": block.parser}),
            StreamKind::Event,
        ));
    }
    let nodes: Vec<(Node, StreamKind)> = nodes
        .into_iter()
        .map(|(kind, params, leaving)| {
            let node = Node {
                id: NodeId::new(),
                kind: kind.into(),
                params,
            };
            (node, leaving)
        })
        .collect();
    let edges = nodes
        .windows(2)
        .map(|w| Edge {
            from: w[0].0.id.clone(),
            to: w[1].0.id.clone(),
            kind: w[0].1,
        })
        .collect();
    Graph {
        id: GraphId::new(),
        nodes: nodes.into_iter().map(|(n, _)| n).collect(),
        edges,
    }
}

/// The node a program row's `demod` runs as, its squelch off: a decoder hears the
/// channel's noise too.
fn demod_node(demod: &Demod) -> Node {
    match demod {
        Demod::Nbfm {
            deviation_hz,
            audio_rate_hz,
        } => Node {
            id: NodeId::new(),
            kind: "nbfm_demod".into(),
            params: serde_json::json!({
                "deviation_hz": deviation_hz,
                "audio_rate_hz": audio_rate_hz,
                "squelch": false,
            }),
        },
    }
}

/// Refuse a band narrower than an IQ program row or a frame row needs: `band.min_hz`
/// (else `band.bandwidth_hz`) is the least, `band.bandwidth_hz` the suggestion, the
/// source rate `source_rate_hz` the most. A `graph` row takes any band its channel can
/// cut.
pub fn check_band(row: &ModeRow, tuning: Tuning, source_rate_hz: u64) -> Result<(), OpError> {
    let checked = match &row.decoder {
        Decoder::Program(block) => block.input.stream == InputStream::Iq,
        Decoder::Frame(_) => true,
        Decoder::Graph { .. } => false,
    };
    if !checked {
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
