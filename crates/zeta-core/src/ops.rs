//! The operation set: the one control vocabulary every caller (CLI, MCP, GUI,
//! voice, a small local model) drives the node through. Strict on input
//! (unknown fields rejected, every field required, one unit per field) and
//! corrective on error (errors name the field and range and propose a value).

use std::path::PathBuf;

use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::capture::{CaptureMeta, write_cf32};
use crate::classify::{Band, Classification, Label, Tried};
use crate::graph::{Edge, Graph, GraphError, GraphId, Node as GraphNode, NodeId};
use crate::job::JobRecord;
use crate::knowledge::{Allocation, Signature};
use crate::lease::LeaseError;
use crate::modes::{
    Band as ModeBand, DecoderKind, Family, LoadedMode, ModeId, ModeRegistry, ModeRow, ModeState,
    is_mode_id, template,
};
use crate::node::{AfterGate, Node, NodeError};
use crate::port::{GainInfo, Port, PortError, PortStats, Tuning};
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
    /// List the sources: id, capabilities, `current` band, `holders` (the
    /// rigid lease holders' job ids), `elastic` (the jobs following the
    /// capture, see `job_start`), `pin` (the band `source_pin` holds the source
    /// at, `null` for none), `fault`, the source's unrecoverable failure (`null` when
    /// healthy), `bank`, the channel bank shared by the capture's channels
    /// (`bin_hz`, `consumers`, `dropped`, the input blocks it dropped because it could
    /// not keep up; `null` for none), `gain`, the overall receive
    /// gain (`db`, `null` while automatic; `min_db`, `max_db`, `automatic`,
    /// `automatic_available`; `null` for a source without adjustable gain), and
    /// `stats`, the stream's counters since it opened (`measured_rate_hz`, the
    /// rate delivered over the last 10 s, `null` before any; `overflows`,
    /// `timeouts`, `restarts`, streams reopened because their samples ran ahead
    /// of the clock; `null` for a source that keeps none). Jobs on a faulted source fail with
    /// `source ID: FAULT` as their reason; a successful tune (`source_ensure`,
    /// `job_start`) clears it. A channelised job's `job_context` holds `channel.path`:
    /// `bank` (with `bin_hz`) or `direct` (with `reason`, why not the bank).
    SourceList {},
    /// Check a band against the source and tune it if no job holds it. Takes
    /// no lease: an unheld source is tuned directly (a later `job_start` may
    /// retune it); a held source is never retuned, and the request either
    /// fits the current band (`Tuning`) or is a `Conflict`. Runs under the
    /// lease manager's per-source gate, so it never interleaves with a job's
    /// lease-and-tune. Elastic jobs do not hold the source: a tune relaunches
    /// them on the new capture.
    SourceEnsure {
        source: SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
        /// Jobs holding the source that may be stopped to make room (`[]` for
        /// none); see `job_start`.
        replace: Vec<JobId>,
    },
    /// Pin the source at a band: a rigid lease held by no job (`source_list`
    /// shows it as `pin`, never among `holders`), granted like a rigid
    /// `job_start`: inside the capture other rigid jobs hold it is leased without
    /// a tune, elsewhere it tunes when only elastic jobs hold the source (they are
    /// relaunched on the new capture), else it is a `conflict`. Replaces any pin
    /// the source had (a conflict keeps the old one). Persisted in the state
    /// directory and restored before any job resumes. The result is the capture.
    SourcePin {
        source: SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
        /// Jobs holding the source that may be stopped to make room (`[]` for
        /// none); see `job_start`.
        replace: Vec<JobId>,
    },
    /// Release the source's pin (see `source_pin`).
    SourceUnpin {
        source: SourceId,
    },
    /// Set the source's overall receive gain live; the result is the gain after
    /// the change (`source_list` shows it as `gain`). Persisted in the state
    /// directory and re-applied when the node opens.
    SourceSetGain {
        source: SourceId,
        /// The gain in dB, within `source_list`'s `gain.min_db..=max_db`; `null`
        /// turns the driver's automatic gain on (where `gain.automatic_available`).
        #[serde(deserialize_with = "crate::port::nullable_f64")]
        #[schemars(required, schema_with = "nullable_gain_db")]
        gain_db: Option<f64>,
    },
    JobStart {
        kind: JobKind,
        source: SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
        /// FM peak deviation in Hz (100..=75000) for `fm_demod` and
        /// `wfm_demod`; `null` is the kind's default (5000 for `fm_demod`,
        /// 75000 for `wfm_demod`). Any other kind must pass `null`.
        #[serde(deserialize_with = "nullable_u64")]
        #[schemars(required, schema_with = "nullable_hz_u64")]
        deviation_hz: Option<u64>,
        /// Sideband for `ssb_demod`: `usb` or `lsb`; `null` is `usb`. Any
        /// other kind must pass `null`.
        #[serde(deserialize_with = "nullable_sideband")]
        #[schemars(required, schema_with = "nullable_sideband_schema")]
        sideband: Option<Sideband>,
        /// `spectrum` only (`false` for any other kind): the job follows the
        /// source's capture instead of holding it. It never conflicts, never
        /// retunes a held source (its `tuning` is the capture it joins), is
        /// relaunched under the same id on every retune, and is stopped by a scan
        /// on its source.
        elastic: bool,
        /// Jobs holding a rigid lease on `source` that may be stopped to make room
        /// (`[]` for none). Under the source's gate each is checked (`unknown`, or
        /// `other` "job <id> does not hold source <src>", nothing stopped) and
        /// stopped as by `job_stop` right before the request, so nothing can take
        /// the radio in between. A conflict with a holder not named leaves them
        /// stopped and names the rest.
        replace: Vec<JobId>,
    },
    /// Attach a job of an attachable `kind` (`transcribe`) to the running job `job`: it
    /// reads that job's audio output stream (the last one in its graph order when it
    /// has several; audio passed between two of its stages does not count) instead of
    /// the radio, holds no lease, copies the job's `source` and `tuning`, and records
    /// it as its `input`. It ends `stopped` when the input job's
    /// stream closes for any reason, a restart by `graph_put` included (attach again to
    /// follow the new graph). On a node restart it resumes after its input, or is
    /// `failed` with `input job <id> not running` when the input does not run.
    JobAttach {
        kind: JobKind,
        job: JobId,
        /// BCP-47 language code of the speech (`en`, `de`, `pt-BR`); `null` is `en`.
        #[serde(deserialize_with = "nullable_string_value")]
        #[schemars(required, schema_with = "nullable_language")]
        language: Option<String>,
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
    /// List the streams: id, kind and provenance. One is always the node's
    /// own `ops` event stream (provenance source `node`): one event per
    /// applied operation with its outcome and caller (`transport`: `local`,
    /// `http`, `ui` or `agent`), logged after the operation completes.
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
    /// The channel plans the node knows, each with its channels expanded
    /// and sorted by frequency.
    ChannelPlans {},
    /// The decoder modes the node knows, one row each (`modes/*.json` built in, then the
    /// operator's `--modes-dir`), sorted by id, narrowed to `family` and `state` when
    /// given (`null` for any).
    ModesList {
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::modes::nullable_enum::<Family>")]
        family: Option<Family>,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::modes::nullable_enum::<ModeState>")]
        state: Option<ModeState>,
    },
    /// One mode's row as loaded, with its `state` and `reason`.
    ModesShow {
        id: String,
    },
    /// Run every registered classifier over the selection's band and span of
    /// the source ring (no upstream features).
    Classify {
        selection: Selection,
    },
    Schema {},
}

/// Deserialize a required `Option<u64>`: without `#[serde(default)]` a missing
/// key is a "missing field" error, while an explicit `null` is `None`.
fn nullable_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    <Option<u64> as Deserialize>::deserialize(d)
}

/// Schema of a required `Option<u64>`: `["integer","null"]`. (`required` alone
/// would drop the `null`.)
fn nullable_hz_u64(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint64",
        "minimum": 0,
        "description": "FM peak deviation in Hz (100..=75000) for `fm_demod` and `wfm_demod`; `null` is the kind's default (5000 for `fm_demod`, 75000 for `wfm_demod`). Any other kind must pass `null`."
    })
}

/// Deserialize a required `Option<Sideband>` (see [`nullable_u64`]).
fn nullable_sideband<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Sideband>, D::Error> {
    <Option<Sideband> as Deserialize>::deserialize(d)
}

/// Schema of a required `Option<Sideband>`: `["string","null"]`, enum
/// `usb`, `lsb` or `null`.
fn nullable_sideband_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "enum": ["usb", "lsb", null],
        "description": "Sideband for `ssb_demod`: `usb` or `lsb`; `null` is `usb`. Any other kind must pass `null`."
    })
}

/// An SSB sideband: upper (`usb`) or lower (`lsb`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sideband {
    Usb,
    Lsb,
}

/// Deserialize a required `Option<String>` (see [`nullable_u64`]).
fn nullable_string_value<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    <Option<String> as Deserialize>::deserialize(d)
}

/// Schema of `source_set_gain`'s `gain_db`: `["number","null"]`.
fn nullable_gain_db(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["number", "null"],
        "description": "The gain in dB, within `source_list`'s `gain.min_db..=max_db`; `null` turns the driver's automatic gain on (where `gain.automatic_available`)."
    })
}

/// Schema of `job_attach`'s `language`: `["string","null"]`.
fn nullable_language(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "description": "BCP-47 language code of the speech (`en`, `de`, `pt-BR`); `null` is `en`."
    })
}

/// The language a `job_attach` passing `null` transcribes.
pub const ATTACH_LANGUAGE_DEFAULT: &str = "en";

/// Whether `code` is a BCP-47 tag as `job_attach` takes it:
/// `^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$`.
pub fn is_bcp47(code: &str) -> bool {
    let mut parts = code.split('-');
    let primary = parts.next().unwrap_or("");
    (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `ssb_demod` audio bandwidth, Hz, used by the `ssb_demod` job kind.
pub const SSB_AUDIO_BANDWIDTH_HZ: u64 = 2_800;

/// Schema of a required `Option<String>`: `["string","null"]`.
fn nullable_string(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "description": "The port's unrecoverable failure (`Port::fault`); `null` when healthy. Jobs on a faulted source fail with it; a successful tune clears it."
    })
}

/// `fm_demod` peak deviation when a job start passes `null`.
pub const FM_DEVIATION_DEFAULT_HZ: u64 = 5_000;
/// `wfm_demod` peak deviation when a job start passes `null` (broadcast FM).
pub const WFM_DEVIATION_DEFAULT_HZ: u64 = 75_000;
/// `wfm_demod` de-emphasis time constant, µs (Australia and Europe).
pub const WFM_DEEMPHASIS_US: u64 = 50;
const FM_DEVIATION_MIN_HZ: u64 = 100;
const FM_DEVIATION_MAX_HZ: u64 = 75_000;

/// Built-in job kinds; each expands to a graph. `fm_demod`, `wfm_demod` and
/// `aprs_decode`, `pocsag_decode`, `rds_decode`, `acars_decode` and `adsb_decode` channelise the job band at the narrowest exact divisor of the source rate
/// that is at least 48 kHz and at least the band; `spectrum` covers the whole
/// capture band. `measure` channelises the job band the same way and
/// measures only that band, reporting its absolute `center_hz`. `scan`
/// time-shares the radio across a band wider than one capture: it expands to
/// no graph; the node steps the source across the band (see [`scan_steps`]),
/// dwelling [`SCAN_DWELL_MS`] on each step, and emits spectrum rows tagged
/// with each step's `center_hz` while holding the whole band, so every other
/// request on the source is a conflict until the scan is stopped.
/// `transcribe` is attachable, not started on the radio: `job_attach` runs it on another
/// job's audio (see [`JobKind::expand_attached`]); `job_start` refuses it.
///
/// The decoder kinds (`aprs_decode`, `pocsag_decode`, `rds_decode`, `acars_decode`,
/// `adsb_decode`) and [`JobKind::Mode`] expand from their mode rows (see
/// [`crate::modes`]); `adsb_decode`'s row refuses a band under 2 MHz (dump1090 needs most
/// of its 2.4 MHz). On the wire a kind is its name: a built-in one (see [`BUILTIN_KINDS`])
/// or any other id matching `^[a-z][a-z0-9_]*$`, which is a `Mode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobKind {
    Passthrough,
    Spectrum,
    FmDemod,
    WfmDemod,
    AmDemod,
    SsbDemod,
    AprsDecode,
    PocsagDecode,
    RdsDecode,
    AcarsDecode,
    AdsbDecode,
    Measure,
    Scan,
    Transcribe,
    /// A mode row's id that is not a built-in kind's name (an operator's row).
    Mode(ModeId),
}

/// The built-in kinds, in the order `schema` lists them: every kind but [`JobKind::Mode`].
pub static BUILTIN_KINDS: [JobKind; 14] = [
    JobKind::Passthrough,
    JobKind::Spectrum,
    JobKind::FmDemod,
    JobKind::WfmDemod,
    JobKind::AmDemod,
    JobKind::SsbDemod,
    JobKind::AprsDecode,
    JobKind::PocsagDecode,
    JobKind::RdsDecode,
    JobKind::AcarsDecode,
    JobKind::AdsbDecode,
    JobKind::Measure,
    JobKind::Scan,
    JobKind::Transcribe,
];

impl Serialize for JobKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for JobKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = std::borrow::Cow::<str>::deserialize(d)?;
        JobKind::from_name(&name).ok_or_else(|| {
            let builtins: Vec<&str> = BUILTIN_KINDS.iter().map(JobKind::name).collect();
            serde::de::Error::custom(format!(
                "unknown job kind {name:?}: expected one of {} or a mode id matching ^[a-z][a-z0-9_]*$",
                builtins.join(", ")
            ))
        })
    }
}

/// The built-in names only; [`schema`] adds the embedded mode rows' ids.
impl JsonSchema for JobKind {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JobKind".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let names: Vec<&str> = BUILTIN_KINDS.iter().map(JobKind::name).collect();
        schemars::json_schema!({ "type": "string", "enum": names })
    }
}

impl JobKind {
    /// The kind's wire name, also recorded as the job's `kind`.
    pub fn name(&self) -> &str {
        match self {
            JobKind::Passthrough => "passthrough",
            JobKind::Spectrum => "spectrum",
            JobKind::FmDemod => "fm_demod",
            JobKind::WfmDemod => "wfm_demod",
            JobKind::AmDemod => "am_demod",
            JobKind::SsbDemod => "ssb_demod",
            JobKind::AprsDecode => "aprs_decode",
            JobKind::PocsagDecode => "pocsag_decode",
            JobKind::RdsDecode => "rds_decode",
            JobKind::AcarsDecode => "acars_decode",
            JobKind::AdsbDecode => "adsb_decode",
            JobKind::Measure => "measure",
            JobKind::Scan => "scan",
            JobKind::Transcribe => "transcribe",
            JobKind::Mode(id) => &id.0,
        }
    }

    /// The id of the mode row the kind expands from: a decoder kind's own name, a
    /// `Mode`'s id; `None` for the other built-in kinds.
    fn mode_id(&self) -> Option<&str> {
        match self {
            JobKind::AprsDecode
            | JobKind::PocsagDecode
            | JobKind::RdsDecode
            | JobKind::AcarsDecode
            | JobKind::AdsbDecode => Some(self.name()),
            JobKind::Mode(id) => Some(&id.0),
            _ => None,
        }
    }

    /// The ready row of `modes` the kind expands from (`None` for a kind without one).
    /// A missing row is `other("unknown mode <id> (known: ...)")`, a row that is not
    /// ready `other("mode <id> is <state>: <reason>")`.
    pub fn mode_row<'a>(&self, modes: &'a ModeRegistry) -> Result<Option<&'a ModeRow>, OpError> {
        let Some(id) = self.mode_id() else {
            return Ok(None);
        };
        let m = modes.get(id).ok_or_else(|| {
            other(format!(
                "unknown mode {id} (known: {})",
                modes.ids().join(", ")
            ))
        })?;
        let state = match m.state {
            ModeState::Ready => return Ok(Some(&m.row)),
            ModeState::Broken => "broken",
            ModeState::Unavailable => "unavailable",
        };
        Err(other(format!(
            "mode {id} is {state}: {}",
            m.reason.as_deref().unwrap_or_default()
        )))
    }

    /// Whether `job_start` may start the kind `elastic` (following the capture).
    pub fn elastic_capable(&self) -> bool {
        matches!(self, JobKind::Spectrum)
    }

    /// Whether the kind runs on another job's output (`job_attach`) rather than on the
    /// radio (`job_start`).
    pub fn attachable(&self) -> bool {
        matches!(self, JobKind::Transcribe)
    }

    /// Why `job_start` refuses an attachable kind.
    fn start_refusal(&self) -> OpError {
        OpError::Other {
            message: format!("{} attaches to a job: use job_attach", self.name()),
        }
    }

    /// The graph of an attached job of this kind reading `input_stream` (an audio stream)
    /// of `input_job`: `stream_in` then the kind's chain, joined by audio edges. For
    /// `transcribe`: `stream_in -> denoise -> vad -> transcribe {"language"}`. A kind that
    /// is not [`attachable`](Self::attachable) is `other("<kind> is not attachable")`.
    pub fn expand_attached(
        &self,
        input_job: &JobId,
        input_stream: &StreamId,
        language: &str,
    ) -> Result<Graph, OpError> {
        let chain: Vec<(&str, serde_json::Value)> = match self {
            JobKind::Transcribe => vec![
                ("denoise", serde_json::json!({})),
                ("vad", serde_json::json!({})),
                ("transcribe", serde_json::json!({"language": language})),
            ],
            _ => {
                return Err(OpError::Other {
                    message: format!("{} is not attachable", self.name()),
                });
            }
        };
        let mut nodes = vec![GraphNode {
            id: NodeId::new(),
            kind: crate::stream_in::STREAM_IN_KIND.into(),
            params: serde_json::json!({
                "job": input_job.0,
                "kind": "audio",
                "stream": input_stream.0,
            }),
        }];
        nodes.extend(chain.into_iter().map(|(kind, params)| GraphNode {
            id: NodeId::new(),
            kind: kind.into(),
            params,
        }));
        let edges = nodes
            .windows(2)
            .map(|w| Edge {
                from: w[0].id.clone(),
                to: w[1].id.clone(),
                kind: StreamKind::Audio,
            })
            .collect();
        Ok(Graph {
            id: GraphId::new(),
            nodes,
            edges,
        })
    }

    /// The kind recorded as `name` (see [`JobKind::name`]): a built-in kind, else a
    /// `Mode` when `name` is a mode id (`^[a-z][a-z0-9_]*$`), else `None`.
    pub fn from_name(name: &str) -> Option<JobKind> {
        BUILTIN_KINDS
            .iter()
            .find(|k| k.name() == name)
            .cloned()
            .or_else(|| is_mode_id(name).then(|| JobKind::Mode(ModeId(name.into()))))
    }

    /// [`JobKind::expand`] again for a resumed job whose `stored` graph was expanded for
    /// another source rate: its `deviation_hz` and `sideband` are read back from the
    /// stored graph's node params (absent: the kind's default), and a demodulator's
    /// `squelch_db` and `squelch`, its adjacent-channel guard's `guard_hz`, `guard_db` and
    /// `channel_hz`, and a channel's `wide_hz` (as `graph_put` may have set them) are
    /// carried to the node of the same stage kind when `expand` writes the key (absent
    /// from `stored`: the defaults `expand` writes).
    pub fn reexpand(
        &self,
        source: &SourceId,
        tuning: Tuning,
        source_rate_hz: Hz,
        bank_bin_rate_hz: Option<u64>,
        stored: &Graph,
        modes: &ModeRegistry,
    ) -> Result<Graph, OpError> {
        let param = |key: &str| stored.nodes.iter().find_map(|n| n.params.get(key).cloned());
        let deviation_hz = param("deviation_hz").and_then(|v| v.as_u64()).map(Hz);
        let sideband = param("sideband").and_then(|v| serde_json::from_value(v).ok());
        let mut g = self.expand(
            source,
            tuning,
            source_rate_hz,
            bank_bin_rate_hz,
            deviation_hz,
            sideband,
            modes,
        )?;
        for n in &mut g.nodes {
            let Some(old) = stored.nodes.iter().find(|o| o.kind == n.kind) else {
                continue;
            };
            for key in CARRIED_PARAMS {
                if let (Some(v), Some(p)) = (old.params.get(key), n.params.as_object_mut())
                    && p.contains_key(key)
                {
                    p.insert(key.into(), v.clone());
                }
            }
        }
        Ok(g)
    }

    /// Whether the kind's graph has a `channel` node, so depends on the source rate.
    fn channelised(&self) -> bool {
        matches!(
            self,
            JobKind::FmDemod
                | JobKind::WfmDemod
                | JobKind::AmDemod
                | JobKind::SsbDemod
                | JobKind::AprsDecode
                | JobKind::PocsagDecode
                | JobKind::RdsDecode
                | JobKind::AcarsDecode
                | JobKind::AdsbDecode
                | JobKind::Measure
                | JobKind::Mode(_)
        )
    }

    /// The kind's graph for `tuning` on a source sampling at `source_rate_hz`
    /// (the port-returned `bandwidth_hz`). Kinds with a `channel` node refuse
    /// an unknown (0) rate and a band wider than [`channel_rate_hz`].
    ///
    /// `bank_bin_rate_hz` is the bin rate of the source's channel bank, if it
    /// has one (twice the bin width). A band no wider than a bin is served by
    /// the bank, so its channel rate is [`channel_rate_hz`] of the bin rate;
    /// otherwise of the source rate.
    ///
    /// A decoder kind or a `Mode` expands from its ready row of `modes` (see
    /// [`JobKind::mode_row`] and [`template::expand`]).
    #[allow(clippy::too_many_arguments)]
    pub fn expand(
        &self,
        _source: &SourceId,
        tuning: Tuning,
        source_rate_hz: Hz,
        bank_bin_rate_hz: Option<u64>,
        deviation_hz: Option<Hz>,
        sideband: Option<Sideband>,
        modes: &ModeRegistry,
    ) -> Result<Graph, OpError> {
        if self.attachable() {
            return Err(self.start_refusal());
        }
        let row = self.mode_row(modes)?;
        let bw = tuning.bandwidth_hz.0;
        let out = match bank_bin_rate_hz {
            Some(bin_rate) if bw <= bin_rate / 2 => channel_rate_hz(bin_rate, bw),
            _ => channel_rate_hz(source_rate_hz.0, bw),
        };
        if self.channelised() {
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
        if let Some(row) = row {
            template::check_band(row, tuning, source_rate_hz.0)?;
            return template::expand(row, tuning, out);
        }
        Ok(match self {
            JobKind::Transcribe => unreachable!("refused above: attachable"),
            JobKind::AprsDecode
            | JobKind::PocsagDecode
            | JobKind::RdsDecode
            | JobKind::AcarsDecode
            | JobKind::AdsbDecode
            | JobKind::Mode(_) => unreachable!("expanded above from the row"),
            JobKind::Scan => Graph {
                id: GraphId::new(),
                nodes: vec![],
                edges: vec![],
            },
            JobKind::Passthrough => Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId::new(),
                    kind: "passthrough".into(),
                    params: serde_json::json!({"kind": "iq"}),
                }],
                edges: vec![],
            },
            JobKind::Measure => {
                let (channel, measure) = (NodeId::new(), NodeId::new());
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
                            id: measure.clone(),
                            kind: "measure".into(),
                            params: serde_json::json!({"bandwidth_hz": tuning.bandwidth_hz.0}),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: measure,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::Spectrum => Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId::new(),
                    kind: "fft".into(),
                    params: serde_json::json!({
                        "size": spectrum_size(tuning.bandwidth_hz.0),
                        "window": "hann",
                        "rows_per_s": SPECTRUM_ROWS_PER_S,
                    }),
                }],
                edges: vec![],
            },
            JobKind::FmDemod => {
                let (channel, demod) = (NodeId::new(), NodeId::new());
                let mut channel_params = serde_json::json!({
                    "center_hz": tuning.center_hz.0,
                    "bandwidth_hz": bw,
                    "out_rate_hz": out,
                });
                let mut demod_params = serde_json::json!({
                    "deviation_hz": deviation_hz.map_or(FM_DEVIATION_DEFAULT_HZ, |d| d.0),
                    "audio_rate_hz": AUDIO_RATE_HZ,
                    "squelch_db": null,
                    "squelch": true,
                });
                // On a plan channel, with room for both neighbours at the channel rate: the
                // adjacent-channel guard (the channel cut three raster steps wide).
                if let Some((plan, _)) = crate::plans::channel_at(tuning.center_hz.0) {
                    let g = crate::plans::adjacent_hz(plan);
                    if bw + 2 * g <= out {
                        channel_params["wide_hz"] = (bw + 2 * g).into();
                        demod_params["guard_hz"] = g.into();
                        demod_params["guard_db"] = crate::plans::GUARD_DB_DEFAULT.into();
                        demod_params["channel_hz"] = bw.into();
                    }
                }
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: channel_params,
                        },
                        GraphNode {
                            id: demod.clone(),
                            kind: "nbfm_demod".into(),
                            params: demod_params,
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::WfmDemod => {
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
                            kind: "wfm_demod".into(),
                            params: serde_json::json!({
                                "deviation_hz": deviation_hz.map_or(WFM_DEVIATION_DEFAULT_HZ, |d| d.0),
                                "audio_rate_hz": AUDIO_RATE_HZ,
                                "deemphasis_us": WFM_DEEMPHASIS_US,
                            }),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::AmDemod => {
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
                            kind: "am_demod".into(),
                            params: serde_json::json!({
                                "audio_rate_hz": AUDIO_RATE_HZ,
                                "squelch_db": null,
                                "squelch": true,
                            }),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::SsbDemod => {
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
                            kind: "ssb_demod".into(),
                            params: serde_json::json!({
                                "sideband": sideband.unwrap_or(Sideband::Usb),
                                "audio_rate_hz": AUDIO_RATE_HZ,
                                "audio_bandwidth_hz": SSB_AUDIO_BANDWIDTH_HZ,
                            }),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
        })
    }
}

/// The audio rate every demodulating job kind outputs; the demod stage
/// resamples from the channel rate to this.
pub const AUDIO_RATE_HZ: u64 = 48_000;

/// Target spectrum resolution: a job's FFT has about one bin per this many Hz of band.
pub const SPECTRUM_BIN_TARGET_HZ: u64 = 6_000;
/// Smallest and largest FFT size a `spectrum` (or `scan`) job uses.
pub const SPECTRUM_SIZE_MIN: u32 = 1024;
pub const SPECTRUM_SIZE_MAX: u32 = 8192;
/// Spectrum rows per second of signal time.
pub const SPECTRUM_ROWS_PER_S: u32 = 20;

/// FFT size for a spectrum of `bandwidth_hz`: the smallest power of two at least
/// `bandwidth_hz / SPECTRUM_BIN_TARGET_HZ`, clamped to
/// `SPECTRUM_SIZE_MIN..=SPECTRUM_SIZE_MAX`.
pub fn spectrum_size(bandwidth_hz: u64) -> u32 {
    let want = bandwidth_hz.div_ceil(SPECTRUM_BIN_TARGET_HZ).max(1);
    want.checked_next_power_of_two()
        .unwrap_or(u64::MAX)
        .clamp(SPECTRUM_SIZE_MIN as u64, SPECTRUM_SIZE_MAX as u64) as u32
}

/// Widest capture one scan step asks of the source.
pub const SCAN_STEP_MAX_HZ: u64 = 16_000_000;
/// How long a scan waits on each step before reading the ring (the node's
/// default; `NodeConfig::scan_dwell_ms` overrides it).
pub const SCAN_DWELL_MS: u64 = 250;

/// The steps a scan of `span` takes on a source with `caps`: captures of
/// `w = min(max_bandwidth_hz, SCAN_STEP_MAX_HZ, span width)` centred at
/// `start + w/2 + i·w` for `i < ceil(bandwidth_hz / w)`, where `start` is the
/// span's lower edge, so a span narrower than one capture is one step exactly
/// as wide as the span. The last step may overrun the span; every centre is
/// clamped to `[tune_min + w/2, tune_max - w/2]`, so no capture reaches
/// outside the source's tune range (when that range is narrower than `w`,
/// to `tune_min + w/2`).
pub fn scan_steps(span: Tuning, caps: &Capabilities) -> Vec<Tuning> {
    let w = caps
        .max_bandwidth_hz
        .value()
        .0
        .min(SCAN_STEP_MAX_HZ)
        .min(span.bandwidth_hz.0)
        .max(1);
    let n = span.bandwidth_hz.0.div_ceil(w).max(1);
    let start = span.center_hz.0.saturating_sub(span.bandwidth_hz.0 / 2);
    let lo = caps.tune_min_hz.value().0.saturating_add(w / 2);
    let hi = caps.tune_max_hz.value().0.saturating_sub(w / 2).max(lo);
    (0..n)
        .map(|i| Tuning {
            center_hz: Hz((start + w / 2 + i * w).clamp(lo, hi)),
            bandwidth_hz: Hz(w),
        })
        .collect()
}

/// The lowest channel output rate a job is channelised at.
pub const CHANNEL_RATE_MIN_HZ: u64 = 48_000;

/// Stage kinds that carry the demodulator squelch (`squelch_db`, `squelch` params).
pub const SQUELCH_STAGES: [&str; 2] = ["nbfm_demod", "am_demod"];

/// Fill in the squelch params of every [`SQUELCH_STAGES`] node of `graph` that lacks
/// them (a graph stored before the squelch, or an operator's): `squelch_db: null`, and
/// `squelch: false` when the node's audio feeds another node of the graph (a decoder, as
/// in `aprs_decode`), else `true` (the job's own audio, as in `fm_demod`). Params
/// already there are kept. Whether anything was filled in.
pub fn fill_squelch_params(graph: &mut Graph) -> bool {
    let mut filled = false;
    for n in &mut graph.nodes {
        if !SQUELCH_STAGES.contains(&n.kind.as_str()) {
            continue;
        }
        let feeds = graph
            .edges
            .iter()
            .any(|e| e.from == n.id && e.kind == StreamKind::Audio);
        let Some(p) = n.params.as_object_mut() else {
            continue;
        };
        if !p.contains_key("squelch") {
            p.insert("squelch".into(), (!feeds).into());
            filled = true;
        }
        if !p.contains_key("squelch_db") {
            p.insert("squelch_db".into(), serde_json::Value::Null);
            filled = true;
        }
    }
    filled
}

/// The params [`JobKind::reexpand`] carries from a stored graph to the node of the same
/// stage kind: what an operator may set with `graph_put` and still have the node's graph.
const CARRIED_PARAMS: [&str; 6] = [
    "squelch_db",
    "squelch",
    "guard_hz",
    "guard_db",
    "channel_hz",
    "wide_hz",
];

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
    #[schemars(required, schema_with = "crate::types::nullable::<Tuning>")]
    pub current: Option<Tuning>,
    /// The jobs holding a rigid lease (never an elastic job).
    pub holders: Vec<JobId>,
    /// The elastic jobs following the source's capture.
    pub elastic: Vec<JobId>,
    /// The band `source_pin` holds the source at; `null` when unpinned.
    #[schemars(required, schema_with = "crate::types::nullable::<Tuning>")]
    pub pin: Option<Tuning>,
    /// The port's unrecoverable failure (`Port::fault`); `null` when healthy.
    /// Jobs on a faulted source fail with it; a successful tune clears it.
    #[schemars(required, schema_with = "nullable_string")]
    pub fault: Option<String>,
    /// The channel bank the node keeps for the source's capture; `null` when it
    /// keeps none (a narrow capture, or no bank factory).
    #[schemars(required, schema_with = "crate::types::nullable::<BankInfo>")]
    pub bank: Option<BankInfo>,
    /// The overall receive gain (`source_set_gain`); `null` for a source without
    /// adjustable gain (a recording, a KiwiSDR, another node, a VRT stream).
    #[schemars(required, schema_with = "crate::types::nullable::<GainInfo>")]
    pub gain: Option<GainInfo>,
    /// The stream's health counters (`Port::stats`): measured rate, overflows,
    /// read timeouts, stream restarts; `null` for a source that keeps none.
    #[schemars(required, schema_with = "crate::types::nullable::<PortStats>")]
    pub stats: Option<PortStats>,
}

/// A source's channel bank: its bin spacing and live subscribers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BankInfo {
    pub bin_hz: u64,
    pub consumers: usize,
    /// Input blocks the bank dropped at its full queue, cumulative.
    pub dropped: u64,
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
    #[schemars(required, schema_with = "crate::types::nullable::<JobId>")]
    pub job: Option<JobId>,
    #[schemars(required, schema_with = "crate::types::nullable::<ChannelId>")]
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
                channels,
                samples,
            } => serde_json::json!({
                "sample_rate_hz": sample_rate_hz.0,
                "channels": channels,
                "samples": samples.len() / usize::from((*channels).max(1)),
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

/// One channel plan as `channel_plans` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PlanInfo {
    pub id: String,
    pub name: String,
    pub region: String,
    /// The width of one channel.
    pub bandwidth_hz: Hz,
    /// The smallest spacing between two channel centres.
    pub adjacent_hz: Hz,
    /// Sorted by frequency.
    pub channels: Vec<PlanChannel>,
}

/// One channel of a plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PlanChannel {
    pub number: u32,
    pub center_hz: Hz,
    /// What the channel is reserved for; `null` for an ordinary channel.
    #[schemars(required, schema_with = "nullable_plan_note")]
    #[serde(deserialize_with = "nullable_string_value")]
    pub note: Option<String>,
}

/// Schema of `PlanChannel.note`: `["string","null"]`.
fn nullable_plan_note(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "description": "What the channel is reserved for (`emergency`, `calling`, `data, no voice`, `road (convention)`); `null` for an ordinary channel."
    })
}

impl From<&crate::plans::ChannelPlan> for PlanInfo {
    fn from(p: &crate::plans::ChannelPlan) -> Self {
        let note = |n| {
            p.notes
                .iter()
                .find(|(c, _)| *c == n)
                .map(|(_, t)| (*t).to_string())
        };
        PlanInfo {
            id: p.id.into(),
            name: p.name.into(),
            region: p.region.into(),
            bandwidth_hz: Hz(p.bandwidth_hz),
            adjacent_hz: Hz(crate::plans::adjacent_hz(p)),
            channels: crate::plans::channels(p)
                .into_iter()
                .map(|(number, hz)| PlanChannel {
                    number,
                    center_hz: Hz(hz),
                    note: note(number),
                })
                .collect(),
        }
    }
}

/// One mode as `modes_list` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ModeInfo {
    pub id: ModeId,
    pub name: String,
    pub family: Family,
    pub band: ModeBand,
    pub modulation: Vec<String>,
    pub state: ModeState,
    /// Why the row is not ready (a broken row names its file's field); `null` when ready.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    #[serde(deserialize_with = "crate::modes::nullable")]
    pub reason: Option<String>,
    pub decoder_kind: DecoderKind,
    /// The file the row came from: `modes/<id>.json` for a built-in row, the path for an
    /// operator row (the file to fix when the row is broken).
    pub file: String,
    /// The program a program row runs; `null` for a graph row or one not yet probed.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    #[serde(deserialize_with = "crate::modes::nullable")]
    pub program: Option<String>,
    /// When `zeta modes check` last passed this program row on this host (UNIX seconds); `null` if never.
    #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
    #[serde(deserialize_with = "crate::modes::nullable")]
    pub checked: Option<u64>,
}

impl From<&LoadedMode> for ModeInfo {
    fn from(m: &LoadedMode) -> Self {
        ModeInfo {
            id: m.row.id.clone(),
            name: m.row.name.clone(),
            family: m.row.family,
            band: m.row.band.clone(),
            modulation: m.row.modulation.clone(),
            state: m.state,
            reason: m.reason.clone(),
            decoder_kind: m.row.decoder.kind(),
            file: m.file.clone(),
            program: m.program.as_ref().map(|p| p.display().to_string()),
            checked: m.checked,
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
    ChannelPlans {
        plans: Vec<PlanInfo>,
    },
    /// `modes_list`: one entry per row, sorted by id.
    Modes {
        modes: Vec<ModeInfo>,
    },
    /// `modes_show`: the row as loaded.
    Mode {
        mode: Box<LoadedMode>,
    },
    /// `classify`: every classifier's labels, and the modes decode-to-identify
    /// considered with their score and outcome.
    Classification {
        labels: Vec<Label>,
        tried: Vec<Tried>,
    },
    /// A source's gain after `source_set_gain`.
    Gain(GainInfo),
    Ok,
    Schema(serde_json::Value),
}

impl Op {
    /// The snake_case tag this variant serialises under `op`.
    pub fn name(&self) -> &'static str {
        match self {
            Op::SourceList {} => "source_list",
            Op::SourceEnsure { .. } => "source_ensure",
            Op::SourcePin { .. } => "source_pin",
            Op::SourceUnpin { .. } => "source_unpin",
            Op::SourceSetGain { .. } => "source_set_gain",
            Op::JobStart { .. } => "job_start",
            Op::JobAttach { .. } => "job_attach",
            Op::JobList {} => "job_list",
            Op::JobStop { .. } => "job_stop",
            Op::JobContext { .. } => "job_context",
            Op::IqRead { .. } => "iq_read",
            Op::StreamList {} => "stream_list",
            Op::StreamTail { .. } => "stream_tail",
            Op::GraphGet { .. } => "graph_get",
            Op::GraphPut { .. } => "graph_put",
            Op::KnowledgeLookup { .. } => "knowledge_lookup",
            Op::ChannelPlans {} => "channel_plans",
            Op::ModesList { .. } => "modes_list",
            Op::ModesShow { .. } => "modes_show",
            Op::Classify { .. } => "classify",
            Op::Schema {} => "schema",
        }
    }

    /// Whether the op only reads the node's state: its result is left out of
    /// `ops.log` when it succeeds (a page polls these several times a second).
    pub fn read_only(&self) -> bool {
        matches!(
            self,
            Op::SourceList {}
                | Op::JobList {}
                | Op::StreamList {}
                | Op::StreamTail { .. }
                | Op::Schema {}
                | Op::JobContext { .. }
                | Op::GraphGet { .. }
                | Op::KnowledgeLookup { .. }
                | Op::ChannelPlans {}
                | Op::ModesList { .. }
                | Op::ModesShow { .. }
        )
    }

    /// The op as JSON for the `ops` stream: the full value when it fits in
    /// [`OPS_RESULT_LIMIT_BYTES`], otherwise
    /// `{"op": <name>, "truncated": true, "bytes": n}`. A value that fails to
    /// serialise is summarised with `bytes: 0`.
    pub fn summary_json(&self) -> serde_json::Value {
        bounded_json(self, "op", self.name())
    }
}

/// `value` as JSON when its serialisation fits in [`OPS_RESULT_LIMIT_BYTES`],
/// else `{tag: name, "truncated": true, "bytes": n}`; a serde failure gives
/// `bytes: 0`.
fn bounded_json<T: Serialize>(value: &T, tag: &str, name: &str) -> serde_json::Value {
    let summary = |bytes: usize| {
        let mut m = serde_json::Map::new();
        m.insert(tag.into(), name.into());
        m.insert("truncated".into(), true.into());
        m.insert("bytes".into(), bytes.into());
        serde_json::Value::Object(m)
    };
    match serde_json::to_vec(value) {
        Ok(buf) if buf.len() > OPS_RESULT_LIMIT_BYTES => summary(buf.len()),
        Ok(buf) => serde_json::from_slice(&buf).unwrap_or_else(|_| summary(0)),
        Err(_) => summary(0),
    }
}

impl OpResult {
    /// The snake_case tag this variant serialises under `result`.
    pub fn variant_name(&self) -> &'static str {
        match self {
            OpResult::Sources(_) => "sources",
            OpResult::Tuning(_) => "tuning",
            OpResult::Job(_) => "job",
            OpResult::Jobs(_) => "jobs",
            OpResult::Context(_) => "context",
            OpResult::Iq { .. } => "iq",
            OpResult::Streams(_) => "streams",
            OpResult::Frames { .. } => "frames",
            OpResult::Graph(_) => "graph",
            OpResult::Knowledge { .. } => "knowledge",
            OpResult::ChannelPlans { .. } => "channel_plans",
            OpResult::Modes { .. } => "modes",
            OpResult::Mode { .. } => "mode",
            OpResult::Classification { .. } => "classification",
            OpResult::Gain(_) => "gain",
            OpResult::Ok => "ok",
            OpResult::Schema(_) => "schema",
        }
    }

    /// The result as JSON for the `ops` stream: the full value when it fits
    /// in [`OPS_RESULT_LIMIT_BYTES`], otherwise
    /// `{"result": <variant>, "truncated": true, "bytes": n}`. A value that
    /// fails to serialise is summarised with `bytes: 0`.
    pub fn summary_json(&self) -> serde_json::Value {
        bounded_json(self, "result", self.variant_name())
    }
}

/// Largest result JSON, in bytes, carried whole on the `ops` stream.
pub const OPS_RESULT_LIMIT_BYTES: usize = 8192;

/// The `ops` stream's event payload for one applied operation:
/// `{"op", "outcome": "ok"|"error", "result"|"error", "elapsed_ms",
/// "transport"}` where `transport` names the caller (see
/// [`crate::node::Node::apply_from`]); see [`Op::summary_json`] and
/// [`OpResult::summary_json`] and [`OpError::summary_json`] for large ops,
/// results and errors.
pub fn ops_payload(
    op: &Op,
    outcome: &Result<OpResult, OpError>,
    elapsed_ms: u64,
    transport: &str,
) -> serde_json::Value {
    let op = op.summary_json();
    let mut v = serde_json::json!({
        "op": op,
        "elapsed_ms": elapsed_ms,
        "transport": transport,
    });
    match outcome {
        Ok(r) => {
            v["outcome"] = "ok".into();
            v["result"] = r.summary_json();
        }
        Err(e) => {
            v["outcome"] = "error".into();
            v["error"] = e.summary_json();
        }
    }
    v
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
        #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
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

impl OpError {
    /// The snake_case tag this variant serialises under `error`.
    pub fn name(&self) -> &'static str {
        match self {
            OpError::OutOfRange { .. } => "out_of_range",
            OpError::Conflict { .. } => "conflict",
            OpError::Unknown { .. } => "unknown",
            OpError::Ring { .. } => "ring",
            OpError::Graph { .. } => "graph",
            OpError::Other { .. } => "other",
        }
    }

    /// The error as JSON for the `ops` stream: the full value when it fits in
    /// [`OPS_RESULT_LIMIT_BYTES`], otherwise
    /// `{"error": <variant>, "truncated": true, "bytes": n}`. A value that
    /// fails to serialise is summarised with `bytes: 0`.
    pub fn summary_json(&self) -> serde_json::Value {
        bounded_json(self, "error", self.name())
    }
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
                    Some(s) if s > max => f.write_str(&crate::port::over_cap(Some(*s), *max)),
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
///
/// `$defs.JobKind` lists the built-in kinds and the embedded mode rows' ids (read from
/// the row files alone, without a stage registry); an operator row's id is accepted too.
pub fn schema() -> serde_json::Value {
    let mut v = serde_json::json!({
        "op": schemars::schema_for!(Op),
        "result": schemars::schema_for!(OpResult),
        "error": schemars::schema_for!(OpError),
    });
    let mut kinds: Vec<String> = BUILTIN_KINDS.iter().map(|k| k.name().into()).collect();
    for (_, text) in ModeRegistry::embedded() {
        let id = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|row| row["id"].as_str().map(String::from));
        if let Some(id) = id.filter(|id| !kinds.contains(id)) {
            kinds.push(id);
        }
    }
    let job_kind = serde_json::json!({
        "type": "string",
        "enum": kinds,
        "description": "A built-in job kind or an embedded mode's id, or the id of an operator mode (modes_list)",
    });
    for part in ["op", "result", "error"] {
        if let Some(defs) = v[part]["$defs"].as_object_mut()
            && defs.contains_key("JobKind")
        {
            defs.insert("JobKind".into(), job_kind.clone());
        }
    }
    v
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
        NodeError::InputNotRunning(j) => OpError::Other {
            message: format!("job {} is not running", j.0),
        },
        NodeError::NoAudioStream(j) => OpError::Other {
            message: format!("job {} has no audio stream", j.0),
        },
        NodeError::Expand(e) => e,
        NodeError::Other(message) => OpError::Other { message },
        e @ (NodeError::Io(_) | NodeError::DuplicateSource(_) | NodeError::Stopped(_)) => other(e),
    }
}

/// The source's port, once `want` is checked against its declared ranges, so
/// the corrective error never depends on the port's own `tune`.
fn checked_port(
    node: &Node,
    source: &SourceId,
    want: &Tuning,
) -> Result<std::sync::Arc<dyn Port>, OpError> {
    checked_port_up_to(node, source, want, |caps| caps.max_bandwidth_hz.value().0)
}

/// [`checked_port`] with the widest band given by `max_band` of the
/// capabilities: a scan's band may be as wide as the tune range plus one capture.
fn checked_port_up_to(
    node: &Node,
    source: &SourceId,
    want: &Tuning,
    max_band: impl Fn(&Capabilities) -> u64,
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
    range("bandwidth_hz", want.bandwidth_hz.0, 1, max_band(caps))?;
    Ok(port)
}

/// The capture band `id` leased on `port`: the port's current band, else the
/// first holder's (as the lease manager takes it), or `None` once `id` holds
/// no lease (the job already ended).
fn leased_band(node: &Node, port: &dyn Port, id: &JobId) -> Option<Tuning> {
    let holders = node.lease_holders(port.id());
    if !holders.iter().any(|(j, _)| j == id) {
        return None;
    }
    port.current().or_else(|| holders.first().map(|(_, t)| *t))
}

/// `job_start` expands against the pre-tune rate, but another job can lease
/// the source between that tune and this job's lease, so the lease may share a
/// capture at another rate. Re-expand against the leased rate and relaunch the
/// job under the same id (`job_graph_put`), once; if the rate moves again the
/// job is stopped and the start is a `Conflict`. Kinds without a `channel`
/// node do not depend on the rate and are left alone.
async fn reconcile_rate(
    node: &Node,
    port: &dyn Port,
    id: &JobId,
    kind: JobKind,
    tuning: Tuning,
    (deviation_hz, sideband): (Option<Hz>, Option<Sideband>),
    mut expected: Hz,
) -> Result<(), OpError> {
    if !kind.channelised() {
        return Ok(());
    }
    for attempt in 0..2 {
        let Some(leased) = leased_band(node, port, id) else {
            return Ok(());
        };
        if leased.bandwidth_hz == expected {
            return Ok(());
        }
        if attempt == 1 {
            // Best effort: the conflict is the answer whether or not the stop lands.
            let _ = node.job_stop(id).await;
            let holders = node
                .lease_holders(port.id())
                .into_iter()
                .map(|(j, _)| j)
                .filter(|j| j != id)
                .collect();
            return Err(OpError::Conflict {
                source: port.id().clone(),
                holders,
                current: leased,
                options: vec!["retry job_start".into(), "stop one of the holders".into()],
            });
        }
        let graph = match kind.expand(
            port.id(),
            tuning,
            leased.bandwidth_hz,
            node.bank_bin_rate(port.id(), leased),
            deviation_hz,
            sideband,
            &node.modes(),
        ) {
            Ok(g) => g,
            Err(e) => {
                let _ = node.job_stop(id).await;
                return Err(e);
            }
        };
        node.job_graph_set(id, graph, Some(leased.bandwidth_hz))
            .await
            .map_err(|e| node_error(node, e))?;
        expected = leased.bandwidth_hz;
    }
    Ok(())
}

/// Stop the `replace` holders under `guard` (see [`Node::replace_holders`]);
/// nothing to do without a guard, which [`Node::gate_for_replace`] gives only
/// when `replace` names jobs.
async fn replace_under(
    node: &Node,
    source: &SourceId,
    replace: &[JobId],
    guard: Option<&tokio::sync::OwnedMutexGuard<()>>,
) -> Result<(), NodeError> {
    match guard {
        Some(g) => node.replace_holders(source, replace, g).await,
        None => Ok(()),
    }
}

/// After a request with `replace` failed with `e`, outside the source's gate:
/// the named jobs were stopped without idling the port (their stop ran under
/// the gate), so idle it now if nobody holds it. Nothing was stopped when `e`
/// is the refusal of an id, and then nothing is idled.
async fn idle_after_replace(node: &Node, port: &dyn Port, replace: &[JobId], e: &NodeError) {
    if replace.is_empty() || matches!(e, NodeError::UnknownJob(_) | NodeError::Other(_)) {
        return;
    }
    node.idle_if_unheld(port).await;
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
                    elastic: node.lease_elastic(&id),
                    pin: node.pinned(&id),
                    fault: node.port(&id).and_then(|p| p.fault()),
                    bank: node.bank(&id).map(|b| BankInfo {
                        bin_hz: b.bin_hz(),
                        consumers: b.consumers(),
                        dropped: b.dropped(),
                    }),
                    gain: node.port(&id).and_then(|p| p.gain()),
                    stats: node.port(&id).and_then(|p| p.stats()),
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
            replace,
        } => {
            let want = Tuning {
                center_hz,
                bandwidth_hz,
            };
            let port = checked_port(node, &source, &want)?;
            let guard = node.gate_for_replace(&source, &replace).await;
            let tuned = match replace_under(node, &source, &replace, guard.as_ref()).await {
                Ok(()) => node
                    .tune_if_unheld(port.as_ref(), want, guard.as_ref())
                    .await
                    .map_err(NodeError::Lease),
                Err(e) => Err(e),
            };
            drop(guard);
            let (got, tuned) = match tuned {
                Ok(t) => t,
                Err(e) => {
                    idle_after_replace(node, port.as_ref(), &replace, &e).await;
                    return Err(node_error(node, e));
                }
            };
            // Outside the gate: the relaunched jobs' leases take it.
            if tuned {
                node.relaunch_elastic(&source).await;
            }
            Ok(OpResult::Tuning(got))
        }
        Op::SourcePin {
            source,
            center_hz,
            bandwidth_hz,
            replace,
        } => {
            let want = Tuning {
                center_hz,
                bandwidth_hz,
            };
            checked_port(node, &source, &want)?;
            node.source_pin(&source, want, &replace)
                .await
                .map(OpResult::Tuning)
                .map_err(|e| node_error(node, e))
        }
        Op::SourceUnpin { source } => {
            if node.port(&source).is_none() {
                return Err(unknown_source(node, &source));
            }
            node.source_unpin(&source)
                .await
                .map(|()| OpResult::Ok)
                .map_err(|e| node_error(node, e))
        }
        Op::SourceSetGain { source, gain_db } => {
            if node.port(&source).is_none() {
                return Err(unknown_source(node, &source));
            }
            node.source_set_gain(&source, gain_db)
                .await
                .map(OpResult::Gain)
                .map_err(|e| node_error(node, e))
        }
        Op::JobStart {
            kind,
            source,
            center_hz,
            bandwidth_hz,
            deviation_hz,
            sideband,
            elastic,
            replace,
        } => {
            if kind.attachable() {
                return Err(kind.start_refusal());
            }
            if elastic && !kind.elastic_capable() {
                return Err(OpError::Other {
                    message: format!("{} cannot be elastic", kind.name()),
                });
            }
            if sideband.is_some() && kind != JobKind::SsbDemod {
                return Err(OpError::Other {
                    message: format!(
                        "sideband applies only to kind ssb_demod; pass null (kind is {})",
                        kind.name()
                    ),
                });
            }
            let deviation = match deviation_hz {
                None => None,
                Some(_) if !matches!(kind, JobKind::FmDemod | JobKind::WfmDemod) => {
                    return Err(OpError::Other {
                        message: format!(
                            "deviation_hz applies only to kinds fm_demod and wfm_demod; pass null (kind is {})",
                            kind.name()
                        ),
                    });
                }
                Some(d) => {
                    range("deviation_hz", d, FM_DEVIATION_MIN_HZ, FM_DEVIATION_MAX_HZ)?;
                    Some(Hz(d))
                }
            };
            // An unknown or unready mode is refused before anything is tuned.
            kind.mode_row(&node.modes())?;
            let tuning = Tuning {
                center_hz,
                bandwidth_hz,
            };
            if kind == JobKind::Scan {
                // No tune here: the scan leases its whole band and steps the
                // source itself; a held source is a conflict at the lease.
                let port = checked_port_up_to(node, &source, &tuning, |caps| {
                    let tune =
                        (caps.tune_max_hz.value().0).saturating_sub(caps.tune_min_hz.value().0);
                    tune.saturating_add(caps.max_bandwidth_hz.value().0)
                })?;
                let graph = kind.expand(&source, tuning, Hz(0), None, None, None, &node.modes())?;
                // With `replace`, the named holders stop and the scan leases under
                // one hold of the gate; the elastic jobs it displaces stop after.
                let guard = node.gate_for_replace(&source, &replace).await;
                let g = guard.as_ref();
                let (started, after) = match replace_under(node, &source, &replace, g).await {
                    Ok(()) => {
                        node.job_start_gated(kind.name(), source, tuning, graph, None, false, g)
                            .await
                    }
                    Err(e) => (Err(e), AfterGate::Nothing),
                };
                drop(guard);
                node.after_gate(after).await;
                return match started {
                    Ok(id) => Ok(OpResult::Job(id)),
                    Err(e) => {
                        idle_after_replace(node, port.as_ref(), &replace, &e).await;
                        Err(node_error(node, e))
                    }
                };
            }
            let port = checked_port(node, &source, &tuning)?;
            // With `replace`, one hold of the source's gate covers stopping the named
            // holders, the tune and the new job's lease: nothing takes the radio in
            // between. Without it each step takes the gate itself.
            let guard = node.gate_for_replace(&source, &replace).await;
            if let Err(e) = replace_under(node, &source, &replace, guard.as_ref()).await {
                drop(guard);
                idle_after_replace(node, port.as_ref(), &replace, &e).await;
                return Err(node_error(node, e));
            }
            // The granted capture band: its width is the source rate (Port contract).
            // An elastic job never tunes here: it joins the current capture (or tunes
            // an unheld source to `tuning`) at its lease, which also re-expands its
            // graph for the capture. A rigid tune here relaunches the elastic jobs once
            // the gate is let go (the job's lease, inside the new capture, tunes nothing).
            let (granted, tuned) = match elastic {
                true => (port.current().unwrap_or(tuning), false),
                false => match node
                    .tune_if_unheld(port.as_ref(), tuning, guard.as_ref())
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        drop(guard);
                        let e = NodeError::Lease(e);
                        idle_after_replace(node, port.as_ref(), &replace, &e).await;
                        return Err(node_error(node, e));
                    }
                },
            };
            let bin_rate = node.bank_bin_rate(&source, granted);
            let (started, after) = match kind.expand(
                &source,
                tuning,
                granted.bandwidth_hz,
                bin_rate,
                deviation,
                sideband,
                &node.modes(),
            ) {
                Ok(graph) => {
                    let (r, after) = node
                        .job_start_gated(
                            kind.name(),
                            source.clone(),
                            tuning,
                            graph,
                            Some(granted.bandwidth_hz),
                            elastic,
                            guard.as_ref(),
                        )
                        .await;
                    // The tune above moved the capture; the lease, inside it,
                    // tuned nothing more, so the elastic jobs follow from here.
                    let after = match after {
                        AfterGate::Nothing if tuned && r.is_ok() => AfterGate::Relaunch {
                            source: source.clone(),
                            except: None,
                        },
                        after => after,
                    };
                    (r.map_err(|e| node_error(node, e)), after)
                }
                Err(e) => (Err(e), AfterGate::Nothing),
            };
            drop(guard);
            node.after_gate(after).await;
            let id = match started {
                Ok(id) => id,
                Err(e) => {
                    if !replace.is_empty() {
                        node.idle_if_unheld(port.as_ref()).await;
                    }
                    // The tune above moved the capture and no lease followed it.
                    if tuned {
                        node.relaunch_elastic(&source).await;
                    }
                    return Err(e);
                }
            };
            reconcile_rate(
                node,
                port.as_ref(),
                &id,
                kind,
                tuning,
                (deviation, sideband),
                granted.bandwidth_hz,
            )
            .await?;
            Ok(OpResult::Job(id))
        }
        Op::JobAttach {
            kind,
            job,
            language,
        } => job_attach(node, kind, job, language).await,
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
        Op::ChannelPlans {} => Ok(OpResult::ChannelPlans {
            plans: crate::plans::PLANS.iter().map(PlanInfo::from).collect(),
        }),
        Op::ModesList { family, state } => Ok(OpResult::Modes {
            modes: node
                .modes()
                .all()
                .iter()
                .filter(|m| family.is_none_or(|f| m.row.family == f))
                .filter(|m| state.is_none_or(|s| m.state == s))
                .map(ModeInfo::from)
                .collect(),
        }),
        Op::ModesShow { id } => {
            let modes = node.modes();
            match modes.get(&id) {
                Some(m) => Ok(OpResult::Mode {
                    mode: Box::new(m.clone()),
                }),
                None => Err(other(format!(
                    "unknown mode {id} (known: {})",
                    modes.ids().join(", ")
                ))),
            }
        }
        Op::Classify { selection } => classify(node, selection).await,
        Op::Schema {} => Ok(OpResult::Schema(node.schema())),
    }
}

/// `job_attach`: check the kind and language, then attach to `job`'s audio stream.
async fn job_attach(
    node: &Node,
    kind: JobKind,
    job: JobId,
    language: Option<String>,
) -> Result<OpResult, OpError> {
    if !kind.attachable() {
        return Err(other(format!("{} is not attachable", kind.name())));
    }
    let language = language.unwrap_or_else(|| ATTACH_LANGUAGE_DEFAULT.into());
    if !is_bcp47(&language) {
        return Err(other(format!(
            "language {language:?} is not a BCP-47 code such as en, de or pt-BR; pass null for en"
        )));
    }
    if !node.job_list().iter().any(|r| r.id == job) {
        return Err(unknown_job(node, &job));
    }
    let stream = node
        .job_audio_stream(&job)
        .map_err(|e| node_error(node, e))?;
    let graph = kind.expand_attached(&job, &stream, &language)?;
    node.job_attach(kind.name(), &job, graph)
        .await
        .map(OpResult::Job)
        .map_err(|e| node_error(node, e))
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

/// Run the classifier set over the selection's band and span of the source ring.
async fn classify(node: &Node, sel: Selection) -> Result<OpResult, OpError> {
    let (rate, center, samples) = ring_samples(node, &sel)?;
    // `check_band` has proved the band lies inside the ring's.
    let band = Band {
        offset_hz: sel.center_hz.0 as i64 - center.0 as i64,
        bandwidth_hz: sel.bandwidth_hz,
    };
    let Classification { labels, tried } = node
        .classifiers()
        .run_all(&samples, rate, center, band, None)
        .await;
    Ok(OpResult::Classification { labels, tried })
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

    fn caps_16m() -> Capabilities {
        use crate::types::Claim;
        Capabilities {
            tune_min_hz: Claim::Asserted(Hz(1_000_000)),
            tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
            max_bandwidth_hz: Claim::Asserted(Hz(16_000_000)),
            formats: vec![],
            channels: 1,
            rx: true,
            tx: false,
            remote: false,
            push: false,
        }
    }

    fn t(center_hz: u64, bandwidth_hz: u64) -> Tuning {
        Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
        }
    }

    #[test]
    fn a_sub_band_scan_is_one_step_as_wide_as_the_span() {
        assert_eq!(
            scan_steps(t(100_000_000, 1_000_000), &caps_16m()),
            [t(100_000_000, 1_000_000)]
        );
    }

    #[test]
    fn scan_steps_stay_inside_the_tune_range() {
        // 0..40 MHz: steps of 16 MHz at 8, 24 and 40 MHz (the last overruns),
        // the first moved up to keep its capture above the 1 MHz floor.
        assert_eq!(
            scan_steps(t(20_000_000, 40_000_000), &caps_16m()),
            [
                t(9_000_000, 16_000_000),
                t(24_000_000, 16_000_000),
                t(40_000_000, 16_000_000)
            ]
        );
        // A span reaching past the tune ceiling: the last capture ends at it.
        let steps = scan_steps(t(5_990_000_000, 30_000_000), &caps_16m());
        assert_eq!(steps.last(), Some(&t(5_992_000_000, 16_000_000)));
        // A span at the floor: the first capture starts at it.
        assert_eq!(
            scan_steps(t(2_000_000, 4_000_000), &caps_16m()),
            [t(3_000_000, 4_000_000)]
        );
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
