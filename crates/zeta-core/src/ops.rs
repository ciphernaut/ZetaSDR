//! The operation set: the one control vocabulary every caller (CLI, MCP, GUI,
//! voice, a small local model) drives the node through. Strict on input
//! (unknown fields rejected, every field required, one unit per field) and
//! corrective on error (errors name the field and range and propose a value).

use std::path::PathBuf;

use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::capture::{CaptureMeta, write_cf32};
use crate::classify::Label;
use crate::graph::{Edge, Graph, GraphError, GraphId, Node as GraphNode, NodeId};
use crate::job::JobRecord;
use crate::knowledge::{Allocation, Signature};
use crate::lease::LeaseError;
use crate::node::{Node, NodeError};
use crate::port::{Port, PortError, Tuning};
use crate::ring::RingError;
use crate::stream::{Frame, Payload, StreamInfo};
use crate::types::{
    Capabilities, ChannelId, Hz, JobId, Selection, SourceId, StreamId, StreamKind, Timestamp,
};

/// One request to the node.
///
/// Variants without fields are written `Variant {}` so serde enforces
/// `deny_unknown_fields` on them too (it ignores extra keys on internally
/// tagged unit variants); the wire form is just `{"op": "..."}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum Op {
    SourceList {},
    /// Check a band against the source and tune it if no job holds it. Takes
    /// no lease: an unheld source is tuned directly (a later `job_start` may
    /// retune it); a held source is never retuned, and the request either
    /// fits the current band (`Tuning`) or is a `Conflict`. Runs under the
    /// lease manager's per-source gate, so it never interleaves with a job's
    /// lease-and-tune.
    SourceEnsure {
        source: SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
    },
    JobStart {
        kind: JobKind,
        source: SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
    },
    JobList {},
    JobStop {
        job: JobId,
    },
    JobContext {
        job: JobId,
    },
    /// Save a selection under `state_dir/captures`: writes the whole ring
    /// bandwidth over the span as a SigMF pair; the selection band is recorded
    /// as an annotation. The capture is labelled with the ring's centre, and
    /// the selection band must lie inside the ring's band.
    IqRead {
        selection: Selection,
    },
    StreamList {},
    /// Collect frames from a stream until `max_frames` (1..=1000) arrive or
    /// `timeout_ms` (1..=10000) passes, whichever is first; frames published
    /// before the call are not seen. Frames skipped because the caller fell
    /// behind are dropped and counted in the result's `missed`.
    StreamTail {
        stream: StreamId,
        max_frames: u32,
        timeout_ms: u32,
    },
    GraphGet {
        job: JobId,
    },
    /// Replace a job's graph; restarts a running job under the same id with
    /// the new graph.
    GraphPut {
        job: JobId,
        graph: Graph,
    },
    /// Band-plan allocations in `region` and known signatures overlapping the
    /// band, from every registered knowledge provider.
    KnowledgeLookup {
        region: String,
        center_hz: Hz,
        bandwidth_hz: Hz,
    },
    /// Run every registered classifier over the selection's span of the
    /// source ring (no upstream features). The analysis covers the whole
    /// source band; the selection band is validated but not used to narrow
    /// the analysis in this release.
    Classify {
        selection: Selection,
    },
    Schema {},
}

/// Built-in job kinds; each expands to a graph. `fm_demod` and `aprs_decode`
/// channelise the job band at the narrowest exact divisor of the source rate
/// that is at least 48 kHz and at least the band; `spectrum` covers the whole
/// capture band. `measure` covers the whole source band; the job band is
/// validated but not used to narrow the analysis in this release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Passthrough,
    Spectrum,
    FmDemod,
    AprsDecode,
    Measure,
}

impl JobKind {
    /// The kind's wire name, also recorded as the job's `kind`.
    pub fn name(&self) -> &'static str {
        match self {
            JobKind::Passthrough => "passthrough",
            JobKind::Spectrum => "spectrum",
            JobKind::FmDemod => "fm_demod",
            JobKind::AprsDecode => "aprs_decode",
            JobKind::Measure => "measure",
        }
    }

    /// The kind's graph for `tuning` on a source sampling at `source_rate_hz`
    /// (the port-returned `bandwidth_hz`). Kinds with a `channel` node refuse
    /// an unknown (0) rate and a band wider than [`channel_rate_hz`].
    pub fn expand(
        &self,
        _source: &SourceId,
        tuning: Tuning,
        source_rate_hz: Hz,
    ) -> Result<Graph, OpError> {
        let out = channel_rate_hz(source_rate_hz.0, tuning.bandwidth_hz.0);
        if matches!(self, JobKind::FmDemod | JobKind::AprsDecode) {
            if source_rate_hz.0 == 0 {
                return Err(OpError::Other {
                    message: "source rate unknown: tune returned bandwidth_hz 0".into(),
                });
            }
            if tuning.bandwidth_hz.0 > out {
                return Err(OpError::OutOfRange {
                    field: "bandwidth_hz".into(),
                    value: tuning.bandwidth_hz.0,
                    min: 1,
                    max: out,
                    suggested: None,
                });
            }
        }
        Ok(match self {
            JobKind::Passthrough => Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId::new(),
                    kind: "passthrough".into(),
                    params: serde_json::json!({"kind": "iq"}),
                }],
                edges: vec![],
            },
            JobKind::Measure => Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId::new(),
                    kind: "measure".into(),
                    params: serde_json::json!({}),
                }],
                edges: vec![],
            },
            JobKind::Spectrum => Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId::new(),
                    kind: "fft".into(),
                    params: serde_json::json!({"size": 1024, "window": "hann"}),
                }],
                edges: vec![],
            },
            JobKind::FmDemod => {
                let (channel, demod) = (NodeId::new(), NodeId::new());
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: serde_json::json!({
                                "center_hz": tuning.center_hz.0,
                                "bandwidth_hz": tuning.bandwidth_hz.0,
                                "out_rate_hz": out,
                            }),
                        },
                        GraphNode {
                            id: demod.clone(),
                            kind: "nbfm_demod".into(),
                            params: serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": out}),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::AprsDecode => {
                let (channel, demod, decode) = (NodeId::new(), NodeId::new(), NodeId::new());
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: serde_json::json!({
                                "center_hz": tuning.center_hz.0,
                                "bandwidth_hz": tuning.bandwidth_hz.0,
                                "out_rate_hz": out,
                            }),
                        },
                        GraphNode {
                            id: demod.clone(),
                            kind: "nbfm_demod".into(),
                            params: serde_json::json!({"deviation_hz": 3_000, "audio_rate_hz": out}),
                        },
                        GraphNode {
                            id: decode.clone(),
                            kind: "aprs_decode".into(),
                            params: serde_json::json!({}),
                        },
                    ],
                    edges: vec![
                        Edge {
                            from: channel,
                            to: demod.clone(),
                            kind: StreamKind::Iq,
                        },
                        Edge {
                            from: demod,
                            to: decode,
                            kind: StreamKind::Audio,
                        },
                    ],
                }
            }
        })
    }
}

/// The lowest channel output rate a job is channelised at.
pub const CHANNEL_RATE_MIN_HZ: u64 = 48_000;

/// The narrowest exact divisor of `source_rate_hz` that is at least
/// [`CHANNEL_RATE_MIN_HZ`] and at least `bandwidth_hz`; the source rate itself
/// when it is no wider than that. Integer only: the channel stage decimates by
/// an exact integer factor.
pub fn channel_rate_hz(source_rate_hz: u64, bandwidth_hz: u64) -> u64 {
    let target = bandwidth_hz.max(CHANNEL_RATE_MIN_HZ);
    if target >= source_rate_hz {
        return source_rate_hz;
    }
    (1..=source_rate_hz / target)
        .rev()
        .find(|d| source_rate_hz.is_multiple_of(*d))
        .map_or(source_rate_hz, |d| source_rate_hz / d)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SourceInfo {
    pub id: SourceId,
    pub capabilities: Capabilities,
    #[schemars(required)]
    pub current: Option<Tuning>,
    pub holders: Vec<JobId>,
}

/// One frame of a `stream_tail`, as JSON. `source`, `job`, `channel` and
/// `start_ns`/`end_ns` are the frame's provenance (`job`/`channel` null when
/// absent). `payload` by kind: event, the event value; text, the string;
/// audio, `{"sample_rate_hz", "samples", "peak"}` (count and largest absolute
/// sample); iq and sink, `{"sample_rate_hz", "center_hz", "samples"}`;
/// video, `{}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct FrameSummary {
    pub stream: StreamId,
    pub kind: StreamKind,
    pub source: SourceId,
    #[schemars(required)]
    pub job: Option<JobId>,
    #[schemars(required)]
    pub channel: Option<ChannelId>,
    pub start_ns: Timestamp,
    pub end_ns: Timestamp,
    pub payload: serde_json::Value,
}

impl FrameSummary {
    pub fn from_frame(f: &Frame) -> Self {
        let payload = match &f.payload {
            Payload::Event(v) => v.clone(),
            Payload::Text(t) => serde_json::Value::String(t.clone()),
            Payload::Audio {
                sample_rate_hz,
                samples,
            } => serde_json::json!({
                "sample_rate_hz": sample_rate_hz.0,
                "samples": samples.len(),
                "peak": samples.iter().fold(0.0f32, |m, s| m.max(s.abs())),
            }),
            Payload::Iq(b) | Payload::Sink(b) => serde_json::json!({
                "sample_rate_hz": b.sample_rate_hz.0,
                "center_hz": b.center_hz.0,
                "samples": b.samples.len(),
            }),
            Payload::Video => serde_json::json!({}),
        };
        Self {
            stream: f.stream.clone(),
            kind: f.kind,
            source: f.provenance.source.clone(),
            job: f.provenance.job.clone(),
            channel: f.provenance.channel.clone(),
            start_ns: f.provenance.span.start_ns,
            end_ns: f.provenance.span.end_ns,
            payload,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[schemars(deny_unknown_fields)]
pub enum OpResult {
    Sources(Vec<SourceInfo>),
    Tuning(Tuning),
    Job(JobId),
    Jobs(Vec<JobRecord>),
    Context(serde_json::Value),
    /// `path` is the `.sigmf-meta` file; the data file sits beside it.
    /// `center_hz` and `bandwidth_hz` are the band actually written (the
    /// ring's), not the selection's.
    Iq {
        sample_rate_hz: Hz,
        center_hz: Hz,
        bandwidth_hz: Hz,
        samples: usize,
        path: PathBuf,
    },
    Streams(Vec<StreamInfo>),
    /// `missed`: frames skipped during the tail because it fell behind.
    Frames {
        frames: Vec<FrameSummary>,
        missed: u64,
    },
    Graph(Graph),
    Knowledge {
        allocations: Vec<Allocation>,
        signatures: Vec<Signature>,
    },
    Labels(Vec<Label>),
    Ok,
    Schema(serde_json::Value),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum OpError {
    OutOfRange {
        field: String,
        value: u64,
        min: u64,
        max: u64,
        #[schemars(required)]
        suggested: Option<u64>,
    },
    /// The source is held by `holders` at `current`, which the request does
    /// not fit inside.
    Conflict {
        source: SourceId,
        holders: Vec<JobId>,
        current: Tuning,
        options: Vec<String>,
    },
    Unknown {
        field: String,
        value: String,
        known: Vec<String>,
    },
    /// Struct variants, not `Ring(RingError)`: flattening a tagged enum into
    /// this one makes a schema that cannot forbid extra fields.
    Ring {
        detail: RingError,
    },
    Graph {
        detail: GraphError,
    },
    /// A struct variant, not `Other(String)`: an internally tagged enum cannot
    /// carry a bare string.
    Other {
        message: String,
    },
}

// Hand-written: thiserror treats a field named `source` as the error source.
impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::OutOfRange {
                field,
                value,
                min,
                max,
                suggested,
            } => {
                write!(f, "{field} {value} out of range {min}..={max}")?;
                match suggested {
                    Some(s) => write!(f, "; did you mean {s}?"),
                    None => Ok(()),
                }
            }
            OpError::Conflict {
                source,
                holders,
                current,
                options,
            } => write!(
                f,
                "source {} is held by {:?} at centre {} Hz, bandwidth {} Hz; options: {}",
                source.0,
                holders.iter().map(|j| j.0.as_str()).collect::<Vec<_>>(),
                current.center_hz.0,
                current.bandwidth_hz.0,
                options.join("; ")
            ),
            OpError::Unknown {
                field,
                value,
                known,
            } => {
                write!(f, "unknown {field} {value:?}; known: {}", known.join(", "))
            }
            OpError::Ring { detail } => write!(f, "{detail}"),
            OpError::Graph { detail } => write!(f, "{detail}"),
            OpError::Other { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for OpError {}

/// Propose the value a caller probably meant when `value` is out of
/// `[min, max]` by whole powers of ten (dropped or extra zeros): multiply when
/// below `min`, divide (only when exact) when above `max`. The SI-prefix
/// scales 10^3 and 10^6 are tried first, then 10^1, 10^2, 10^4, 10^5; the
/// first that lands in range wins. Prefix-first matters for wide ranges: for
/// 101500 in 1 MHz..6 GHz plain smallest-k would give 1015000, not 101500000.
pub fn suggest(value: u64, min: u64, max: u64) -> Option<u64> {
    let in_range = |v: u64| (min..=max).contains(&v);
    [3u32, 6, 1, 2, 4, 5].into_iter().find_map(|k| {
        let p = 10u64.pow(k);
        let v = if value < min {
            value.checked_mul(p)?
        } else if value > max && value.is_multiple_of(p) {
            value / p
        } else {
            return None;
        };
        in_range(v).then_some(v)
    })
}

/// JSON schemas of `Op`, `OpResult` and `OpError` under `op`, `result`,
/// `error`. The `schema` operation answers with [`Node::schema`], which adds
/// the node's stage kinds under `stages`.
pub fn schema() -> serde_json::Value {
    serde_json::json!({
        "op": schemars::schema_for!(Op),
        "result": schemars::schema_for!(OpResult),
        "error": schemars::schema_for!(OpError),
    })
}

fn other(e: impl std::fmt::Display) -> OpError {
    OpError::Other {
        message: e.to_string(),
    }
}

fn range(field: &str, value: u64, min: u64, max: u64) -> Result<(), OpError> {
    if (min..=max).contains(&value) {
        return Ok(());
    }
    Err(OpError::OutOfRange {
        field: field.into(),
        value,
        min,
        max,
        suggested: suggest(value, min, max),
    })
}

fn unknown_source(node: &Node, source: &SourceId) -> OpError {
    let mut known: Vec<String> = node.ports().into_iter().map(|(id, _)| id.0).collect();
    known.sort();
    OpError::Unknown {
        field: "source".into(),
        value: source.0.clone(),
        known,
    }
}

fn unknown_job(node: &Node, job: &JobId) -> OpError {
    let known = node.job_list().into_iter().map(|r| r.id.0).collect();
    OpError::Unknown {
        field: "job".into(),
        value: job.0.clone(),
        known,
    }
}

fn port_error(e: PortError) -> OpError {
    match e {
        PortError::OutOfRange {
            field,
            value,
            min,
            max,
            suggested,
        } => OpError::OutOfRange {
            field: field.into(),
            value,
            min,
            max,
            suggested: suggested.or_else(|| suggest(value, min, max)),
        },
        e => other(e),
    }
}

fn lease_error(e: LeaseError) -> OpError {
    match e {
        LeaseError::Conflict {
            source,
            holders,
            current,
            options,
        } => OpError::Conflict {
            source,
            holders,
            current,
            options,
        },
        LeaseError::Port(p) => port_error(p),
    }
}

fn node_error(node: &Node, e: NodeError) -> OpError {
    match e {
        NodeError::UnknownSource(s) => unknown_source(node, &s),
        NodeError::UnknownJob(j) => unknown_job(node, &j),
        NodeError::Graph(detail) => OpError::Graph { detail },
        NodeError::Lease(l) => lease_error(l),
        e @ (NodeError::Io(_) | NodeError::DuplicateSource(_)) => other(e),
    }
}

/// The source's port, once `want` is checked against its declared ranges, so
/// the corrective error never depends on the port's own `tune`.
fn checked_port(
    node: &Node,
    source: &SourceId,
    want: &Tuning,
) -> Result<std::sync::Arc<dyn Port>, OpError> {
    let port = node
        .port(source)
        .ok_or_else(|| unknown_source(node, source))?;
    let caps = port.capabilities();
    range(
        "center_hz",
        want.center_hz.0,
        caps.tune_min_hz.value().0,
        caps.tune_max_hz.value().0,
    )?;
    range(
        "bandwidth_hz",
        want.bandwidth_hz.0,
        1,
        caps.max_bandwidth_hz.value().0,
    )?;
    Ok(port)
}

pub(crate) async fn apply(node: &Node, op: Op) -> Result<OpResult, OpError> {
    match op {
        Op::SourceList {} => {
            let mut v: Vec<SourceInfo> = node
                .ports()
                .into_iter()
                .map(|(id, capabilities)| SourceInfo {
                    current: node.port(&id).and_then(|p| p.current()),
                    holders: node
                        .lease_holders(&id)
                        .into_iter()
                        .map(|(j, _)| j)
                        .collect(),
                    id,
                    capabilities,
                })
                .collect();
            v.sort_by(|a, b| a.id.0.cmp(&b.id.0));
            Ok(OpResult::Sources(v))
        }
        Op::SourceEnsure {
            source,
            center_hz,
            bandwidth_hz,
        } => {
            let want = Tuning {
                center_hz,
                bandwidth_hz,
            };
            let port = checked_port(node, &source, &want)?;
            node.tune_if_unheld(port.as_ref(), want)
                .await
                .map(OpResult::Tuning)
                .map_err(lease_error)
        }
        Op::JobStart {
            kind,
            source,
            center_hz,
            bandwidth_hz,
        } => {
            let tuning = Tuning {
                center_hz,
                bandwidth_hz,
            };
            let port = checked_port(node, &source, &tuning)?;
            // The granted capture band: its width is the source rate (Port contract).
            let granted = node
                .tune_if_unheld(port.as_ref(), tuning)
                .await
                .map_err(lease_error)?;
            let graph = kind.expand(&source, tuning, granted.bandwidth_hz)?;
            node.job_start(kind.name(), source, tuning, graph)
                .await
                .map(OpResult::Job)
                .map_err(|e| node_error(node, e))
        }
        Op::JobList {} => Ok(OpResult::Jobs(node.job_list())),
        Op::JobStop { job } => node
            .job_stop(&job)
            .await
            .map(|()| OpResult::Ok)
            .map_err(|e| node_error(node, e)),
        Op::JobContext { job } => node
            .job_context(&job)
            .map(OpResult::Context)
            .ok_or_else(|| unknown_job(node, &job)),
        Op::IqRead { selection } => iq_read(node, selection),
        Op::StreamList {} => {
            let mut v = node.bus().list();
            v.sort_by(|a, b| a.id.0.cmp(&b.id.0));
            Ok(OpResult::Streams(v))
        }
        Op::StreamTail {
            stream,
            max_frames,
            timeout_ms,
        } => stream_tail(node, stream, max_frames, timeout_ms).await,
        Op::GraphGet { job } => node
            .job_list()
            .into_iter()
            .find(|r| r.id == job)
            .map(|r| OpResult::Graph(r.graph))
            .ok_or_else(|| unknown_job(node, &job)),
        Op::GraphPut { job, graph } => node
            .job_graph_put(&job, graph)
            .await
            .map(|()| OpResult::Ok)
            .map_err(|e| node_error(node, e)),
        Op::KnowledgeLookup {
            region,
            center_hz,
            bandwidth_hz,
        } => {
            let (mut allocations, mut signatures) = (Vec::new(), Vec::new());
            for p in node.knowledge() {
                allocations.extend(p.allocations(&region, center_hz, bandwidth_hz).await);
                signatures.extend(p.signatures(center_hz, bandwidth_hz).await);
            }
            Ok(OpResult::Knowledge {
                allocations,
                signatures,
            })
        }
        Op::Classify { selection } => classify(node, selection).await,
        Op::Schema {} => Ok(OpResult::Schema(node.schema())),
    }
}

/// The ring's sample rate and centre and the selection's samples, once the
/// selection's band is checked to lie within the ring's band.
fn ring_samples(node: &Node, sel: &Selection) -> Result<(Hz, Hz, Vec<Complex<f32>>), OpError> {
    if node.port(&sel.source).is_none() {
        return Err(unknown_source(node, &sel.source));
    }
    let (start_ns, end_ns) = (sel.span.start_ns, sel.span.end_ns);
    if end_ns.0 <= start_ns.0 {
        // Reported even before the ring has data, as the ring itself would.
        return Err(OpError::Ring {
            detail: RingError::BadSpan { start_ns, end_ns },
        });
    }
    let empty = || OpError::Ring {
        detail: RingError::Empty,
    };
    let ring = node.ring(&sel.source).ok_or_else(empty)?;
    let r = ring.read().unwrap();
    let (rate, center) = (r.sample_rate_hz(), r.center_hz().ok_or_else(empty)?);
    check_band(sel, center, rate)?;
    let samples = r
        .read(sel.span.clone())
        .map_err(|detail| OpError::Ring { detail })?;
    Ok((rate, center, samples))
}

/// `[center - bw/2, center + bw/2]` of the selection must lie within the
/// ring's `[center - rate/2, center + rate/2]`, with a bandwidth of at least 1 Hz.
fn check_band(sel: &Selection, center: Hz, rate: Hz) -> Result<(), OpError> {
    range("selection.bandwidth_hz", sel.bandwidth_hz.0, 1, rate.0)?;
    let (half, ring_half) = (sel.bandwidth_hz.0 / 2, rate.0 / 2);
    range(
        "selection.center_hz",
        sel.center_hz.0,
        center.0.saturating_sub(ring_half).saturating_add(half),
        center.0.saturating_add(ring_half).saturating_sub(half),
    )
}

/// Run the classifier set over the selection's span of the source ring.
async fn classify(node: &Node, sel: Selection) -> Result<OpResult, OpError> {
    let (rate, _, samples) = ring_samples(node, &sel)?;
    Ok(OpResult::Labels(
        node.classifiers().run_all(&samples, rate, None).await,
    ))
}

/// Most frames and longest wait one `stream_tail` takes.
const TAIL_MAX_FRAMES: u64 = 1000;
const TAIL_MAX_TIMEOUT_MS: u64 = 10_000;

/// `field` = `value` must be in `1..=max`; no suggestion is offered.
fn limit(field: &str, value: u32, max: u64) -> Result<(), OpError> {
    if (1..=max).contains(&(value as u64)) {
        return Ok(());
    }
    Err(OpError::OutOfRange {
        field: field.into(),
        value: value as u64,
        min: 1,
        max,
        suggested: None,
    })
}

/// Subscribe to `stream` and collect up to `max` frames within `timeout_ms`.
/// A lagged receiver keeps going and counts the skipped frames in `missed`;
/// a closed stream returns what was collected.
async fn stream_tail(
    node: &Node,
    stream: StreamId,
    max: u32,
    timeout_ms: u32,
) -> Result<OpResult, OpError> {
    limit("max_frames", max, TAIL_MAX_FRAMES)?;
    limit("timeout_ms", timeout_ms, TAIL_MAX_TIMEOUT_MS)?;
    let bus = node.bus();
    let Some(mut rx) = bus.subscribe(&stream) else {
        let mut known: Vec<String> = bus.list().into_iter().map(|s| s.id.0).collect();
        known.sort();
        return Err(OpError::Unknown {
            field: "stream".into(),
            value: stream.0,
            known,
        });
    };
    let mut out = Vec::new();
    let mut missed = 0u64;
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
    while out.len() < max as usize {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => break,
            Ok(Ok(f)) => out.push(FrameSummary::from_frame(&f)),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => missed += n,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
        }
    }
    Ok(OpResult::Frames {
        frames: out,
        missed,
    })
}

/// Write the selection's span from the source ring as a cf32_le SigMF pair.
fn iq_read(node: &Node, sel: Selection) -> Result<OpResult, OpError> {
    let (rate, center, samples) = ring_samples(node, &sel)?;
    let dir = node.state_dir().join("captures");
    std::fs::create_dir_all(&dir).map_err(other)?;
    let name = ulid::Ulid::generate().to_string();
    let half = sel.bandwidth_hz.0 / 2;
    let meta = CaptureMeta {
        sample_rate_hz: rate,
        center_hz: center,
        start: sel.span.start_ns,
        annotations: vec![serde_json::json!({
            "core:sample_start": 0,
            "core:sample_count": samples.len(),
            "core:freq_lower_edge": sel.center_hz.0.saturating_sub(half),
            "core:freq_upper_edge": sel.center_hz.0.saturating_add(half),
            "core:label": "selection",
        })],
    };
    let path = write_cf32(&dir, &name, &meta, &samples).map_err(other)?;
    // Complex baseband: the written band is as wide as the sample rate.
    Ok(OpResult::Iq {
        sample_rate_hz: rate,
        center_hz: center,
        bandwidth_hz: rate,
        samples: samples.len(),
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_scales_by_powers_of_ten() {
        assert_eq!(
            suggest(101_500, 1_000_000, 6_000_000_000),
            Some(101_500_000)
        );
        assert_eq!(
            suggest(100_000, 100_000_000, 100_000_000),
            Some(100_000_000)
        );
        assert_eq!(
            suggest(2_400_000, 24_000_000, 1_766_000_000),
            Some(24_000_000)
        );
        assert_eq!(
            suggest(101_500_000_000, 1_000_000, 6_000_000_000),
            Some(101_500_000)
        );
        assert_eq!(
            suggest(101_500_001_000, 1_000_000, 6_000_000_000),
            Some(101_500_001)
        );
        assert_eq!(suggest(7, 1_000_000_000, 6_000_000_000), None);
        assert_eq!(suggest(7_000_000_001, 1_000_000, 6_000_000_000), None);
    }

    #[test]
    fn wire_shapes_are_tagged() {
        let op = Op::JobStop {
            job: JobId("j".into()),
        };
        assert_eq!(
            serde_json::to_value(&op).unwrap(),
            serde_json::json!({"op": "job_stop", "job": "j"})
        );
        assert_eq!(
            serde_json::to_value(Op::JobList {}).unwrap(),
            serde_json::json!({"op": "job_list"})
        );
        assert_eq!(
            serde_json::to_value(OpResult::Ok).unwrap(),
            serde_json::json!({"result": "ok"})
        );
        let e = OpError::Ring {
            detail: RingError::Empty,
        };
        let want = serde_json::json!({"error": "ring", "detail": {"cause": "empty"}});
        assert_eq!(serde_json::to_value(&e).unwrap(), want);
        let e = OpError::Other {
            message: "m".into(),
        };
        assert_eq!(
            serde_json::from_value::<OpError>(serde_json::to_value(&e).unwrap()).unwrap(),
            e
        );
    }
}
