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
}

/// Schemars omits serde-defaulted fields from `required` (and `#[schemars(required)]`
/// cannot override that for deserialisation); a record is always written with
/// `format`, so the published schema requires it.
fn require_format(schema: &mut schemars::Schema) {
    if let Some(req) = schema
        .ensure_object()
        .entry("required")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
    {
        req.push("format".into());
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
}
