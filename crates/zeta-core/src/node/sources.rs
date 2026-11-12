//! Source records on disk: `<state_dir>/sources/<source id>.json`, what the node
//! keeps about a source across restarts (its pin and its gain). Written
//! atomically as job records are; a record for a source that is not attached is
//! kept untouched (sources are not persisted, the flags may return).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::NodeError;
use super::persist::io;
use crate::port::Tuning;
use crate::types::{Capabilities, CapturePolicy, Claim, Hz, SourceId};

/// The format a source record is written in; a newer one on disk is a load error.
pub const SOURCE_RECORD_FORMAT: u32 = 1;

fn current_format() -> u32 {
    SOURCE_RECORD_FORMAT
}

/// What the node keeps about one source. Every field after `format` defaults, so
/// an older record loads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceRecord {
    #[serde(default = "current_format")]
    pub format: u32,
    /// The band `source_pin` holds the source at; `None` when unpinned.
    #[serde(default)]
    pub pin: Option<Tuning>,
    /// The receive gain `source_set_gain` set, re-applied when the node opens;
    /// `None` when never set (the flag or the driver's default applies).
    #[serde(default)]
    pub gain: Option<GainSetting>,
    /// The capabilities a worker port reported the last time it was live, so a
    /// node that starts while the device is absent can still list the source.
    /// `None` for a source that runs in the node (nothing to cache).
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
    /// The link speed the figures in `capabilities` were measured at (the
    /// [`crate::port::link_speed`] of the worker's attachment then); `None` when
    /// unknown. A worker whose link now runs at another speed measures again.
    #[serde(default)]
    pub measured_at_speed: Option<String>,
    /// The capture policy `source_set_capture` set, applied when the node opens;
    /// the default (`fit`) is not written.
    #[serde(default, skip_serializing_if = "is_fit")]
    pub capture: CapturePolicy,
}

fn is_fit(p: &CapturePolicy) -> bool {
    *p == CapturePolicy::Fit
}

impl SourceRecord {
    /// An empty record in the current format.
    pub fn new() -> Self {
        Self {
            format: SOURCE_RECORD_FORMAT,
            ..Self::default()
        }
    }
}

/// `caps` with no figure held as a measurement: the top-level `max_bandwidth_hz`
/// and each depth's cap turned `Asserted` at the same value. What `source_measure`
/// leaves in the record: the source stays known (a start with the device away
/// attaches it faulted), and a worker is passed back no cap, so it probes.
pub(crate) fn unmeasured(mut caps: Capabilities) -> Capabilities {
    let assert = |c: &mut Claim<Hz>| *c = Claim::Asserted(*c.value());
    assert(&mut caps.max_bandwidth_hz);
    for d in &mut caps.depths {
        assert(&mut d.max_bandwidth_hz);
    }
    caps
}

/// A gain the op set: `db` in decibels, `None` for automatic gain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GainSetting {
    #[serde(default)]
    pub db: Option<f64>,
}

/// Load every `<id>.json` record in `dir` (none when `dir` does not exist),
/// deleting leftover `*.json.tmp` files from an interrupted write. A file that
/// cannot be read or parsed, or whose format is newer than
/// [`SOURCE_RECORD_FORMAT`], is skipped and reported by name (`unreadable source
/// record <file>`, `source record <file>: format ...`).
pub fn load_sources(dir: &Path) -> (HashMap<SourceId, SourceRecord>, Vec<String>) {
    let (mut records, mut errors) = (HashMap::new(), Vec::new());
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (records, errors),
        Err(e) => {
            errors.push(format!("sources: {e}"));
            return (records, errors);
        }
    };
    for entry in entries {
        let path = match entry {
            Ok(e) => e.path(),
            Err(e) => {
                errors.push(format!("sources: {e}"));
                continue;
            }
        };
        if path.to_string_lossy().ends_with(".json.tmp") {
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
            .unwrap_or(u64::from(SOURCE_RECORD_FORMAT));
        if format > u64::from(SOURCE_RECORD_FORMAT) {
            errors.push(format!(
                "source record {name}: format {format} newer than supported {SOURCE_RECORD_FORMAT}"
            ));
            continue;
        }
        match value.and_then(|v| serde_json::from_value::<SourceRecord>(v).ok()) {
            Some(r) if !stem.is_empty() => {
                records.insert(SourceId(stem.to_string()), r);
            }
            _ => errors.push(format!("unreadable source record {name}")),
        }
    }
    (records, errors)
}

/// Load the one record `<dir>/<id>.json`; `None` when it is missing, unreadable,
/// or in a newer format than [`SOURCE_RECORD_FORMAT`].
pub fn load_source(dir: &Path, id: &SourceId) -> Option<SourceRecord> {
    let text = std::fs::read_to_string(dir.join(format!("{}.json", id.0))).ok()?;
    let rec: SourceRecord = serde_json::from_str(&text).ok()?;
    (rec.format <= SOURCE_RECORD_FORMAT).then_some(rec)
}

/// Write `<dir>/<id>.json` atomically (temp file, then rename), making `dir` if
/// it is missing.
pub fn persist_source(dir: &Path, id: &SourceId, rec: &SourceRecord) -> Result<(), NodeError> {
    std::fs::create_dir_all(dir).map_err(io)?;
    let json = serde_json::to_vec_pretty(rec).map_err(io)?;
    let path = dir.join(format!("{}.json", id.0));
    let tmp = dir.join(format!("{}.json.tmp", id.0));
    std::fs::write(&tmp, json).map_err(io)?;
    std::fs::rename(&tmp, &path).map_err(io)
}
