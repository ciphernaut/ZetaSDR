//! Rotating JSON-lines files in the state directory: `ops.log` (one line per
//! applied op), `transcripts.log` (one line per transcript event) and `events.log`
//! (one line per event frame of every source's feed).

use std::io::Write;
use std::path::PathBuf;

/// Size past which a log is rotated to `<name>.1`.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// As [`MAX_BYTES`] for `events.log`: every decoded event of every source goes
/// there, so a night of traffic must fit in its two files.
pub(super) const EVENTS_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// An append-only JSON-lines file; at most two files (`<name>`, `<name>.1`) exist.
pub(super) struct JsonLog {
    path: PathBuf,
    max_bytes: u64,
    /// Held across the rotate and the write, so two appends never both rotate
    /// (the second renaming the fresh file over `<name>.1`, or failing).
    lock: std::sync::Mutex<()>,
}

impl JsonLog {
    /// A log at `path`, rotated past 8 MiB.
    pub(super) fn new(path: PathBuf) -> Self {
        Self::with_max_bytes(path, MAX_BYTES)
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
        let log = JsonLog::new(dir.path().join("ops.log"));
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
            let log = std::sync::Arc::new(JsonLog::with_max_bytes(path.clone(), 5000));
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
        let log = JsonLog::with_max_bytes(path.clone(), 64);
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

    #[test]
    fn two_logs_rotate_independently() {
        let dir = tempfile::tempdir().unwrap();
        let ops = JsonLog::with_max_bytes(dir.path().join("ops.log"), 64);
        let transcripts = JsonLog::with_max_bytes(dir.path().join("transcripts.log"), 64);
        let line = |c: char| json!({"n": c.to_string().repeat(31)});
        // Three 40-byte lines rotate `ops.log` once; one leaves `transcripts.log` alone.
        for c in ['a', 'b', 'c'] {
            ops.append(&line(c)).unwrap();
        }
        transcripts.append(&line('t')).unwrap();
        let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).ok();
        assert_eq!(read("ops.log.1").unwrap().lines().count(), 2);
        assert_eq!(read("ops.log").unwrap().lines().count(), 1);
        assert_eq!(read("transcripts.log").unwrap().lines().count(), 1);
        assert_eq!(read("transcripts.log.1"), None);
        // And past its own size, `transcripts.log` rotates without touching `ops.log.1`.
        for c in ['u', 'v'] {
            transcripts.append(&line(c)).unwrap();
        }
        assert_eq!(read("transcripts.log.1").unwrap().lines().count(), 2);
        assert_eq!(read("transcripts.log").unwrap().lines().count(), 1);
        assert!(read("ops.log.1").unwrap().contains('a'));
    }
}
