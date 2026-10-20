//! Job records: what a node persists so jobs, ids and context survive restart.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::graph::Graph;
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
}

/// Schemars omits serde-defaulted fields from `required` (and `#[schemars(required)]`
/// cannot override that for deserialisation); a record is always written with
/// `format`, `input`, `capture_rate_hz`, `elastic` and `operator_graph`, so the published schema requires them.
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
    }
}
