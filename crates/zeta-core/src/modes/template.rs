//! A mode row's decoder expanded to a job graph.
//!
//! A `graph` row's template has its placeholders (`"$center_hz"`, `"$bandwidth_hz"`,
//! `"$out_rate_hz"`) filled with the job's integers, and `"$audio_policy"` with the job's
//! [`AudioPolicy`] by name (a row naming it is one whose stage has several voices); a
//! `program` row becomes a `channel` at the same rate into [`EXTERNAL_DECODE_KIND`],
//! which resamples to the program's `input.rate_hz` (an audio row's channel runs through
//! its `demod`, an `nbfm_demod` with the squelch off, first). A `frame` row becomes `channel → resample → <demod> →
//! clock_recover → slicer → frame_decode` (the resampler left out when the channel is
//! already at the chain rate). Every node and the graph get fresh ids, as the built-in
//! kinds' graphs do: a re-expansion carries params by stage kind, never by node id.

use std::collections::HashMap;

use super::handler_file::own_handler_id;
use super::{Decoder, Demod, FrameBlock, InputStream, ModeHandler, ModeRow, ProgramBlock};
use crate::graph::{Edge, Graph, GraphId, Node, NodeId};
use crate::ops::{AudioPolicy, OpError};
use crate::port::Tuning;
use crate::stage::StageRegistry;
use crate::types::StreamKind;

/// The stage kind that runs a program row's decoder, or a handler file's program; its
/// params are `{"mode": <id>}`, with `"handler": <id>` for a handler not the row's own.
pub const EXTERNAL_DECODE_KIND: &str = "external_decode";

/// The placeholder a graph row's template names a stage's audio policy param with,
/// filled with the job's [`AudioPolicy::name`] (a string, where the others are integers).
pub const AUDIO_POLICY_PLACEHOLDER: &str = "$audio_policy";

/// `row`'s graph for `tuning`, run by `handler`, with its channel output at
/// `out_rate_hz`, the narrowest exact divisor of the source rate that `expand` picks, for
/// every kind of row (the `channel` stage decimates by an integer only; a program's
/// adaptor resamples from it to the program's `input.rate_hz`). A graph row's
/// `"$audio_policy"` is filled with `audio_policy`'s name.
///
/// An in-process handler (a graph or frame row's own) expands the row's decoder. A
/// program handler expands as a program row does, from `block` (the handler's program
/// block, [`super::ModeRegistry::program_block`]; a program row's own block when `None`
/// and the handler is the row's own): its `external_decode` node's params are
/// `{"mode": <id>}` for the row's own, `{"mode": <id>, "handler": <id>}` for any other.
pub fn expand(
    row: &ModeRow,
    handler: &ModeHandler,
    block: Option<&ProgramBlock>,
    tuning: Tuning,
    out_rate_hz: u64,
    audio_policy: AudioPolicy,
) -> Result<Graph, OpError> {
    let values = [
        ("$center_hz", tuning.center_hz.0.into()),
        ("$bandwidth_hz", tuning.bandwidth_hz.0.into()),
        ("$out_rate_hz", out_rate_hz.into()),
        (AUDIO_POLICY_PLACEHOLDER, audio_policy.name().into()),
    ];
    let own = handler.id == own_handler_id(row);
    if !handler.in_process {
        let block = match (block, &row.decoder) {
            (Some(b), _) => b,
            (None, Some(Decoder::Program(b))) if own => b,
            (None, _) => {
                return Err(OpError::Other {
                    message: format!("mode {}: handler {} has no program", row.id.0, handler.id),
                });
            }
        };
        let named = (!own).then_some(handler.id.as_str());
        return program_graph(row, block, named, tuning, out_rate_hz);
    }
    let Some(decoder) = &row.decoder else {
        return Err(no_decoder(row));
    };
    match decoder {
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
        Decoder::Program(block) => program_graph(row, block, None, tuning, out_rate_hz),
        Decoder::Frame(block) => Ok(frame_graph(block, tuning, out_rate_hz)),
    }
}

/// `mode <id> has no decoder of its own`: what expanding or checking a band of a row
/// with none, without a handler's block, is.
fn no_decoder(row: &ModeRow) -> OpError {
    OpError::Other {
        message: format!("mode {} has no decoder of its own", row.id.0),
    }
}

/// A program's graph: a `channel` at `out_rate_hz` into [`EXTERNAL_DECODE_KIND`] (an
/// audio program hearing the channel through its `demod` first), the decode node's
/// params `{"mode": <row id>, "bandwidth_hz": <the job's>}`, with `"handler": <id>` for a handler not the row's own.
fn program_graph(
    row: &ModeRow,
    block: &ProgramBlock,
    handler: Option<&str>,
    tuning: Tuning,
    out_rate_hz: u64,
) -> Result<Graph, OpError> {
    let channel = Node {
        id: NodeId::new(),
        kind: "channel".into(),
        params: serde_json::json!({
            "center_hz": tuning.center_hz.0,
            "bandwidth_hz": tuning.bandwidth_hz.0,
            "out_rate_hz": out_rate_hz,
        }),
    };
    let mut params = serde_json::json!({ "mode": row.id.0, "bandwidth_hz": tuning.bandwidth_hz.0 });
    if let Some(h) = handler {
        params["handler"] = h.into();
    }
    let decode = Node {
        id: NodeId::new(),
        kind: EXTERNAL_DECODE_KIND.into(),
        params,
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

/// The stream kinds a job of `row` emits. A graph row's are those of its terminal nodes
/// (no edge leaves them), from each stage's `spec().outputs`, in node order, each kind
/// once, and `packet` when any node's stage lists it and no `packet` edge leaves that
/// node (the stage's packets then reach the job's output). A program row emits events,
/// and packets too with a `packets` block; a frame row emits events and packets (its
/// audio stays inside its chain). A row with no decoder of its own emits nothing itself.
pub fn outputs(row: &ModeRow, stages: &StageRegistry) -> Vec<StreamKind> {
    let (nodes, edges) = match &row.decoder {
        None => return Vec::new(),
        Some(Decoder::Graph { nodes, edges, .. }) => (nodes, edges),
        Some(Decoder::Frame(_)) => return vec![StreamKind::Event, StreamKind::Packet],
        Some(Decoder::Program(p)) if p.packets.is_some() => {
            return vec![StreamKind::Event, StreamKind::Packet];
        }
        Some(Decoder::Program(_)) => return vec![StreamKind::Event],
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
    let packets = nodes.iter().any(|n| {
        stages
            .spec(&n.kind)
            .is_some_and(|s| s.outputs.contains(&StreamKind::Packet))
            && !edges
                .iter()
                .any(|e| e.from == n.id && e.kind == StreamKind::Packet)
    });
    if packets && !out.contains(&StreamKind::Packet) {
        out.push(StreamKind::Packet);
    }
    out
}

/// Whether `row`'s template names `"$audio_policy"` anywhere in its nodes' params: the
/// rows `job_start` takes an `audio_policy` for (any other has one voice).
pub fn names_audio_policy(row: &ModeRow) -> bool {
    fn names(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::String(s) => s == AUDIO_POLICY_PLACEHOLDER,
            serde_json::Value::Object(m) => m.values().any(names),
            serde_json::Value::Array(a) => a.iter().any(names),
            _ => false,
        }
    }
    match &row.decoder {
        Some(Decoder::Graph { nodes, .. }) => nodes.iter().any(|n| names(&n.params)),
        Some(Decoder::Program(_) | Decoder::Frame(_)) | None => false,
    }
}

/// The voices a job of `row` has beside its main audio stream: the [`Stage::voice_roles`]
/// of a graph row's terminal nodes (no edge leaves them) with their template params, in
/// node order, each once. Empty for a program or frame row, a row with no decoder of its
/// own, and a row with one voice.
///
/// [`Stage::voice_roles`]: crate::stage::Stage::voice_roles
pub fn voices(row: &ModeRow, stages: &StageRegistry) -> Vec<String> {
    let Some(Decoder::Graph { nodes, edges, .. }) = &row.decoder else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for n in nodes
        .iter()
        .filter(|n| !edges.iter().any(|e| e.from == n.id))
    {
        for role in stages.voice_roles(&n.kind, &n.params) {
            if !out.iter().any(|r| r == role) {
                out.push(role.to_string());
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

/// Refuse a band narrower than an IQ program or a frame row needs: `band.min_hz` (else
/// `band.bandwidth_hz`) is the least, `band.bandwidth_hz` the suggestion, the source rate
/// `source_rate_hz` the most. `block` is the program that runs (a handler's,
/// [`super::ModeRegistry::program_block`]); `None` reads the row's own decoder (an error
/// for a row with none). A `graph` row run in process takes any band its channel can cut.
pub fn check_band(
    row: &ModeRow,
    block: Option<&ProgramBlock>,
    tuning: Tuning,
    source_rate_hz: u64,
) -> Result<(), OpError> {
    let checked = match (block, &row.decoder) {
        (Some(b), _) | (None, Some(Decoder::Program(b))) => b.input.stream == InputStream::Iq,
        (None, Some(Decoder::Frame(_))) => true,
        (None, Some(Decoder::Graph { .. })) => false,
        (None, None) => return Err(no_decoder(row)),
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
        why: None,
    })
}

/// Replace every string in `v` that is one of `values`' placeholders by its value.
fn fill(v: &mut serde_json::Value, values: &[(&str, serde_json::Value)]) {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            if let Some((_, n)) = values.iter().find(|(p, _)| p == s) {
                *v = n.clone();
            }
        }
        Value::Object(m) => m.values_mut().for_each(|x| fill(x, values)),
        Value::Array(a) => a.iter_mut().for_each(|x| fill(x, values)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADSB: &str = include_str!("../../../../modes/adsb_decode.json");

    #[test]
    fn a_program_row_with_packets_outputs_events_and_packets() {
        let stages = StageRegistry::new();
        // The ADS-B row reads dump1090's Beast output as its packets.
        let row = super::super::parse(ADSB).unwrap();
        assert_eq!(
            outputs(&row, &stages),
            [StreamKind::Event, StreamKind::Packet]
        );
        let mut v: serde_json::Value = serde_json::from_str(ADSB).unwrap();
        v["decoder"]["packets"] = serde_json::Value::Null;
        let row = super::super::parse(&v.to_string()).unwrap();
        assert_eq!(outputs(&row, &stages), [StreamKind::Event]);
    }

    /// A row with no decoder of its own gives nothing itself, and expands or checks a
    /// band only by a handler's block: without one it is an error naming the row.
    #[test]
    fn a_row_with_no_decoder_expands_only_by_a_handlers_block() {
        use crate::types::Hz;
        let stages = StageRegistry::new();
        let mut v: serde_json::Value = serde_json::from_str(ADSB).unwrap();
        let block: ProgramBlock = serde_json::from_value({
            let mut d = v["decoder"].clone();
            d.as_object_mut().unwrap().remove("kind");
            d
        })
        .unwrap();
        v["id"] = "nodec_decode".into();
        v["decoder"] = serde_json::Value::Null;
        let row = super::super::parse(&v.to_string()).unwrap();
        assert_eq!(row.decoder, None);
        assert!(outputs(&row, &stages).is_empty());
        assert!(voices(&row, &stages).is_empty());
        assert!(!names_audio_policy(&row));
        assert!(row.handler_blocks().is_empty());
        let tuning = Tuning {
            center_hz: Hz(1_090_000_000),
            bandwidth_hz: Hz(2_400_000),
        };
        let none = ModeHandler::of_row(&row);
        assert_eq!(none.id, super::super::handler_file::NO_DECODER);
        let policy = AudioPolicy::default();
        let e = expand(&row, &none, None, tuning, 2_400_000, policy).unwrap_err();
        assert_eq!(
            e.to_string(),
            "mode nodec_decode: handler none has no program"
        );
        let in_process = ModeHandler {
            in_process: true,
            ..none.clone()
        };
        let e = expand(&row, &in_process, None, tuning, 2_400_000, policy).unwrap_err();
        assert_eq!(e.to_string(), "mode nodec_decode has no decoder of its own");
        let e = check_band(&row, None, tuning, 2_400_000).unwrap_err();
        assert_eq!(e.to_string(), "mode nodec_decode has no decoder of its own");
        // A handler's block runs it.
        let alt = ModeHandler {
            id: "alt".into(),
            ..none
        };
        let g = expand(&row, &alt, Some(&block), tuning, 2_400_000, policy).unwrap();
        assert!(g.nodes.iter().any(|n| n.kind == EXTERNAL_DECODE_KIND));
        assert_eq!(check_band(&row, Some(&block), tuning, 2_400_000), Ok(()));
    }

    /// A program's decode node carries the job's band width, for `$bandwidth_hz`.
    #[test]
    fn a_programs_decode_node_carries_the_jobs_bandwidth() {
        let row = super::super::parse(include_str!("../../../../modes/adsb_decode.json")).unwrap();
        let Some(Decoder::Program(block)) = row.decoder.clone() else {
            panic!("a program row")
        };
        for bw in [1_500_000, 2_000_000] {
            let tuning = Tuning {
                center_hz: crate::types::Hz(1_090_000_000),
                bandwidth_hz: crate::types::Hz(bw),
            };
            let g = program_graph(&row, &block, None, tuning, 2_400_000).unwrap();
            let n = g.nodes.iter().find(|n| n.kind == EXTERNAL_DECODE_KIND);
            assert_eq!(n.unwrap().params["bandwidth_hz"], bw);
        }
    }
}
