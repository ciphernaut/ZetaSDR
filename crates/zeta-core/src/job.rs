//! Job records: what a node persists so jobs, ids and context survive restart.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::graph::Graph;
use crate::ops::AudioPolicy;
use crate::port::Tuning;
use crate::types::{JobId, SourceId, Timestamp};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum JobState {
    Created,
    Running,
    Paused,
    Stopped,
    Failed { reason: String },
}

/// Current on-disk format of [`JobRecord`].
pub const JOB_RECORD_FORMAT: u32 = 1;

/// A persisted job.
///
/// Format policy: records are `deny_unknown_fields`; every field added after
/// format 1 must carry `#[serde(default)]`; a loader bumps `JOB_RECORD_FORMAT`
/// only when a field is removed or its meaning changes, and `load_records`
/// reports any record whose `format > JOB_RECORD_FORMAT` in `load_errors` as
/// `"<file>: format N newer than supported 1"`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_format)]
pub struct JobRecord {
    /// Record format version; absent in files written before versioning (format 1).
    #[serde(default = "JobRecord::current_format")]
    pub format: u32,
    pub id: JobId,
    pub kind: String,
    pub graph: Graph,
    pub source: SourceId,
    pub tuning: Tuning,
    pub state: JobState,
    pub context: serde_json::Value,
    pub created_ns: Timestamp,
    /// The job an attached job reads its stream from (`job_attach`); `null` for a job on
    /// the radio. An attached job holds no lease and copies `source` and `tuning` from it.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<JobId>")]
    pub input: Option<JobId>,
    /// The capture rate (the source's `bandwidth_hz`) the node expanded `graph` for, so a
    /// resume at another rate re-expands a built-in kind; `null` for a graph the node did
    /// not expand for a rate (`graph_put`, `scan`, attached jobs), which launches as stored.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<u64>")]
    pub capture_rate_hz: Option<u64>,
    /// Started with `elastic` (a `spectrum` job only): it follows the source's capture
    /// instead of holding it, so `tuning` is the capture it last joined.
    #[serde(default)]
    pub elastic: bool,
    /// The graph was set by an operator (`graph_put`), not expanded by the node: it is
    /// launched as stored and never re-expanded. Lets a record without
    /// `capture_rate_hz` written before that field existed (whose graph the node did
    /// expand) be told apart from an operator's graph.
    #[serde(default)]
    pub operator_graph: bool,
    /// The controller job that started this one and owns it (a `watch` owns the
    /// `identify` runs it starts and their children); `null` for an operator's job. An
    /// owned record is dropped from the history as soon as it stops or fails, so it
    /// never takes one of the kept slots.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<JobId>")]
    pub owner: Option<JobId>,
    /// The `audio_policy` the job was started with (a mode whose stage has several
    /// voices): what its main audio stream carries, filled into its graph at every
    /// expansion, so a restart or a retune resumes it. `null`: the row's default
    /// (`first_active`), and for every other job.
    #[serde(default)]
    #[schemars(schema_with = "crate::modes::nullable_enum::<AudioPolicy>")]
    pub audio_policy: Option<AudioPolicy>,
    /// The voice of its input an attached job reads (`job_attach`'s `stream`: a role
    /// among the input row's `voices`); `null` for the input's main audio stream, and for
    /// a job on the radio. A relaunch reads the same voice.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub stream: Option<String>,
    /// The sinks the node appends to `graph` at every expansion of it (the feed a `watch`
    /// gives the decoders it keeps): nodes of stages with an `event` input and no
    /// outputs, each fed by the graph's decoder events ([`crate::graph::append_sinks`]).
    /// `graph` already carries them; a re-expansion at another capture rate appends them
    /// again. Empty for every other job (and in a record written before the field).
    #[serde(default)]
    pub sinks: Vec<crate::graph::Node>,
    /// The handler that runs the job's mode row (`job_start`'s `handler`, or the
    /// policy's choice): one of the mode's `handlers` (`modes_list`). `null` for a kind
    /// of no mode row, an attached job, and a record written before handlers (which runs
    /// the row's own).
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub handler: Option<String>,
    /// `handler` was named by the start, not chosen by the policy: a relaunch that finds
    /// it no longer ready fails the job instead of choosing again.
    #[serde(default)]
    pub handler_named: bool,
    /// The stream kinds the start needed (`job_start`'s `needs`): a relaunch that chooses
    /// the handler again chooses among those giving them. Empty for none.
    #[serde(default)]
    pub needs: Vec<crate::types::StreamKind>,
    /// The Send to group the job runs in: the id of the job the first Send to named
    /// (`job_start`'s `beside`), on that job and on every sibling started beside it or
    /// beside a sibling; `null` on a job never sent from. A bare key: the job it names may
    /// have stopped, or its record have been pruned. Not `plan_start`'s or `jobs_stop`'s
    /// `group` (a channel plan's group).
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<JobId>")]
    pub group: Option<JobId>,
}

/// Schemars omits serde-defaulted fields from `required` (and `#[schemars(required)]`
/// cannot override that for deserialisation); a record is always written with
/// `format`, `input`, `capture_rate_hz`, `elastic`, `operator_graph`, `owner`,
/// `audio_policy`, `stream`, `sinks`, `handler`, `handler_named`, `needs` and `group`,
/// so the published schema requires them.
fn require_format(schema: &mut schemars::Schema) {
    if let Some(req) = schema
        .ensure_object()
        .entry("required")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
    {
        for f in [
            "format",
            "input",
            "capture_rate_hz",
            "elastic",
            "operator_graph",
            "owner",
            "audio_policy",
            "stream",
            "sinks",
            "handler",
            "handler_named",
            "needs",
            "group",
        ] {
            if !req.iter().any(|r| r == f) {
                req.push(f.into());
            }
        }
    }
}

impl JobRecord {
    fn current_format() -> u32 {
        JOB_RECORD_FORMAT
    }

    /// Whether the job runs on no radio: its record's source is the node's placeholder
    /// [`NODE_SOURCE`](crate::node::NODE_SOURCE) (a `mix`, a `graph_start` job). It takes
    /// no lease, launches with no port, resumes after the jobs on a radio and has no band
    /// to retune.
    pub fn leaseless(&self) -> bool {
        self.source.0 == crate::node::NODE_SOURCE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_serialises_tagged_snake_case() {
        assert_eq!(
            serde_json::to_value(JobState::Running).unwrap(),
            serde_json::json!({"status": "running"})
        );
        let f = JobState::Failed { reason: "x".into() };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v, serde_json::json!({"status": "failed", "reason": "x"}));
        assert_eq!(serde_json::from_value::<JobState>(v).unwrap(), f);
    }

    #[test]
    fn a_record_without_input_loads_as_on_the_radio() {
        let v = serde_json::json!({
            "format": 1,
            "id": "01J00000000000000000000000",
            "kind": "passthrough",
            "graph": {"id": "g", "nodes": [], "edges": []},
            "source": "s",
            "tuning": {"center_hz": 1, "bandwidth_hz": 1},
            "state": {"status": "stopped"},
            "context": {},
            "created_ns": 0
        });
        let r: JobRecord = serde_json::from_value(v).unwrap();
        assert_eq!(r.input, None);
        assert_eq!(
            serde_json::to_value(&r).unwrap()["input"],
            serde_json::Value::Null
        );
        // A record from before voices: no policy and the main stream, written as `null`.
        assert_eq!((r.audio_policy, r.stream.as_deref()), (None, None));
        let written = serde_json::to_value(&r).unwrap();
        assert_eq!(written["audio_policy"], serde_json::Value::Null);
        assert_eq!(written["stream"], serde_json::Value::Null);
        // A record from before handlers: none named, written as `null` and `false`.
        assert_eq!((r.handler.as_deref(), r.handler_named), (None, false));
        assert_eq!(written["handler"], serde_json::Value::Null);
        assert_eq!(written["handler_named"], false);
        assert!(r.needs.is_empty());
        assert_eq!(written["needs"], serde_json::json!([]));
    }

    #[test]
    fn a_record_without_group_loads_and_writes_null() {
        let v = serde_json::json!({
            "id": "01J00000000000000000000000",
            "kind": "adsb_decode",
            "graph": {"id": "g", "nodes": [], "edges": []},
            "source": "s",
            "tuning": {"center_hz": 1, "bandwidth_hz": 1},
            "state": {"status": "running"},
            "context": {},
            "created_ns": 0
        });
        let r: JobRecord = serde_json::from_value(v).unwrap();
        // A record from before Send to: in no group, written as `null`.
        assert_eq!(r.group, None);
        assert_eq!(
            serde_json::to_value(&r).unwrap()["group"],
            serde_json::Value::Null
        );
        let schema = serde_json::to_value(schemars::schema_for!(JobRecord)).unwrap();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f == "group"),
            "a record is always written with its group"
        );
    }
}
