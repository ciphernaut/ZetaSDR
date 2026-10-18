//! Job records on disk: atomic writes, loading, and the clock.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::NodeError;
use crate::job::{JOB_RECORD_FORMAT, JobRecord};
use crate::types::Timestamp;

pub(super) fn now() -> Timestamp {
    Timestamp(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64),
    )
}

pub(super) fn io(e: impl std::fmt::Display) -> NodeError {
    NodeError::Io(e.to_string())
}

/// Write `<id>.json` atomically: temp file, then rename.
pub(super) fn persist(dir: &Path, rec: &JobRecord) -> Result<(), NodeError> {
    let json = serde_json::to_vec_pretty(rec).map_err(io)?;
    let path = dir.join(format!("{}.json", rec.id.0));
    let tmp = dir.join(format!("{}.json.tmp", rec.id.0));
    std::fs::write(&tmp, json).map_err(io)?;
    std::fs::rename(&tmp, &path).map_err(io)
}

/// Load every `*.json` record and delete leftover `*.json.tmp` files from an
/// interrupted `persist`. A file that cannot be read or parsed, or whose
/// record id is not a ULID equal to the file stem, is skipped and reported by
/// name.
pub(super) fn load(dir: &Path) -> Result<(Vec<JobRecord>, Vec<String>), NodeError> {
    let (mut records, mut errors) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.to_string_lossy().ends_with(".json.tmp") {
            // Best effort: a leftover that cannot be removed is harmless.
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let format = value
            .as_ref()
            .and_then(|v| v.get("format"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(u64::from(JOB_RECORD_FORMAT));
        if format > u64::from(JOB_RECORD_FORMAT) {
            errors.push(format!(
                "{name}: format {format} newer than supported {JOB_RECORD_FORMAT}"
            ));
            continue;
        }
        let parsed = value
            .and_then(|v| serde_json::from_value::<JobRecord>(v).ok())
            .filter(|r| r.id.0 == stem && r.id.0.parse::<ulid::Ulid>().is_ok());
        match parsed {
            Some(r) => records.push(r),
            None => errors.push(name),
        }
    }
    Ok((records, errors))
}
