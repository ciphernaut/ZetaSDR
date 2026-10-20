//! The ops log: one JSON line per applied op, in `<state_dir>/ops.log`.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Size past which the log is rotated to `ops.log.1`.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// An append-only JSON-lines file; at most two files (`ops.log`, `ops.log.1`) exist.
pub(super) struct OpsLog {
    path: PathBuf,
    max_bytes: u64,
    /// Held across the rotate and the write, so two appends never both rotate
    /// (the second renaming the fresh file over `ops.log.1`, or failing).
    lock: std::sync::Mutex<()>,
}

impl OpsLog {
    pub(super) fn new(state_dir: &Path) -> Self {
        Self::with_max_bytes(state_dir.join("ops.log"), MAX_BYTES)
    }

    pub(super) fn with_max_bytes(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            lock: std::sync::Mutex::new(()),
        }
    }

    /// Append `line` as one `\n`-terminated line. A file already longer than
    /// `max_bytes` is first renamed to `<path>.1`, replacing any older one.
    /// Concurrent appends are serialised.
    pub(super) fn append(&self, line: &serde_json::Value) -> std::io::Result<()> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
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
    fn concurrent_appends_rotate_once_and_lose_no_line() {
        // 8 threads x 25 lines of 40 bytes = 8000 bytes: past 5000 the log rotates
        // exactly once, so every line is in `ops.log.1` or `ops.log`. Repeated, as
        // a race between two rotations needs luck to show.
        for round in 0..50 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("ops.log");
            let log = std::sync::Arc::new(OpsLog::with_max_bytes(path.clone(), 5000));
            let threads: Vec<_> = (0..8)
                .map(|t| {
                    let log = log.clone();
                    std::thread::spawn(move || {
                        for n in 0..25 {
                            log.append(&json!({"n": format!("{t:02}-{n:02}-{}", "x".repeat(23))}))
                                .unwrap();
                        }
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
            let old = std::fs::read_to_string(dir.path().join("ops.log.1")).unwrap_or_default();
            let cur = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                old.lines().count() + cur.lines().count(),
                200,
                "round {round}"
            );
        }
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
