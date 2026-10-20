//! The ops log: one JSON line per applied op, in `<state_dir>/ops.log`.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Size past which the log is rotated to `ops.log.1`.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// An append-only JSON-lines file; at most two files (`ops.log`, `ops.log.1`) exist.
pub(super) struct OpsLog {
    path: PathBuf,
    max_bytes: u64,
}

impl OpsLog {
    pub(super) fn new(state_dir: &Path) -> Self {
        Self::with_max_bytes(state_dir.join("ops.log"), MAX_BYTES)
    }

    pub(super) fn with_max_bytes(path: PathBuf, max_bytes: u64) -> Self {
        Self { path, max_bytes }
    }

    /// Append `line` as one `\n`-terminated line. A file already longer than
    /// `max_bytes` is first renamed to `<path>.1`, replacing any older one.
    pub(super) fn append(&self, line: &serde_json::Value) -> std::io::Result<()> {
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > self.max_bytes) {
            let mut old = self.path.clone().into_os_string();
            old.push(".1");
            std::fs::rename(&self.path, old)?;
        }
        let mut text = line.to_string();
        text.push('\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_writes_one_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let log = OpsLog::new(dir.path());
        log.append(&json!({"n": 1})).unwrap();
        log.append(&json!({"n": 2})).unwrap();
        let text = std::fs::read_to_string(dir.path().join("ops.log")).unwrap();
        assert_eq!(text, "{\"n\":1}\n{\"n\":2}\n");
    }

    #[test]
    fn rotates_when_over_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.log");
        let log = OpsLog::with_max_bytes(path.clone(), 64);
        // `{"n":"…"}` plus the newline is exactly 40 bytes.
        let line = |c: char| json!({"n": c.to_string().repeat(31)});
        for c in ['a', 'b', 'c'] {
            log.append(&line(c)).unwrap();
        }
        let old = std::fs::read_to_string(dir.path().join("ops.log.1")).unwrap();
        assert_eq!(old.lines().count(), 2);
        assert!(old.contains('a') && old.contains('b'));
        let cur = std::fs::read_to_string(&path).unwrap();
        assert_eq!(cur.lines().count(), 1);
        assert!(cur.contains('c'));
    }
}
