//! Job records: what a node persists so jobs, ids and context survive restart.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::graph::Graph;
use crate::port::Tuning;
use crate::types::{JobId, SourceId, Timestamp};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum JobState {
    Created,
    Running,
    Paused,
    Stopped,
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct JobRecord {
    pub id: JobId,
    pub kind: String,
    pub graph: Graph,
    pub source: SourceId,
    pub tuning: Tuning,
    pub state: JobState,
    pub context: serde_json::Value,
    pub created: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_serialises_tagged_snake_case() {
        assert_eq!(
            serde_json::to_value(JobState::Running).unwrap(),
            serde_json::json!({"state": "running"})
        );
        let f = JobState::Failed { reason: "x".into() };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v, serde_json::json!({"state": "failed", "reason": "x"}));
        assert_eq!(serde_json::from_value::<JobState>(v).unwrap(), f);
    }
}
