//! The session records: what one exploration job (`capture`, `bits`, `graph`, a frame
//! row's job) saw and what came of it, one [`Session`] per job under
//! `state_dir/sessions/<job id>.json`, written at the job's end. A session goes with
//! its captures: once none of the captures it lists is left in the burst store, the
//! sweep removes it (a session that lists none, a frame-row job on a radio, stays).

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::graph::Graph;
use crate::types::{Hz, JobId, SourceId};

/// The store's directory under the state directory.
pub const SESSIONS_DIR: &str = "sessions";

/// What came of a session: the signature it proposed, the mode its frames proved, or
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Outcome {
    Proposal { id: String },
    Proved { mode: String },
    None,
}

/// One exploration job's record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub job: JobId,
    /// The job's kind (`capture`, `bits`, `graph`, or the frame row's id).
    pub kind: String,
    pub source: SourceId,
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
    /// The captures it wrote or read, in order; empty for none.
    pub captures: Vec<String>,
    /// The first capture's feature record (a frame-row job on a radio: its tuning and
    /// the row's demodulator), `null` for none.
    pub features: Value,
    pub graph: Graph,
    /// `bit_view`'s last `hypotheses`, `null` for none.
    pub hypotheses: Value,
    /// `frame_decode`'s last note, `null` for none.
    pub frame_decode: Value,
    pub outcome: Outcome,
    pub started_ns: u64,
    pub ended_ns: u64,
}

/// The store: the directory; every read goes to disk.
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// Open (making it if missing) `state_dir/sessions`.
    pub fn open(state_dir: &Path) -> io::Result<Self> {
        let dir = state_dir.join(SESSIONS_DIR);
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, job: &JobId) -> PathBuf {
        self.dir.join(format!("{}.json", job.0))
    }

    /// Write `s` as `<job id>.json`, replacing the job's earlier record: written to a
    /// `.json.tmp` beside it, then renamed over it.
    pub fn write(&self, s: &Session) -> io::Result<()> {
        let path = self.path(&s.job);
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(s).map_err(io::Error::other)?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
    }

    /// Every record that reads as one, oldest start first (then by job id).
    pub fn list(&self) -> Vec<Session> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<Session> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .filter_map(|p| std::fs::read(&p).ok())
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect();
        out.sort_by(|a, b| (a.started_ns, &a.job.0).cmp(&(b.started_ns, &b.job.0)));
        out
    }

    /// Remove every session whose `captures` is not empty and holds none of
    /// `live_captures`; the jobs removed.
    pub fn sweep(&self, live_captures: &HashSet<String>) -> Vec<JobId> {
        let mut gone = Vec::new();
        for s in self.list() {
            if s.captures.is_empty() || s.captures.iter().any(|c| live_captures.contains(c)) {
                continue;
            }
            if std::fs::remove_file(self.path(&s.job)).is_ok() {
                gone.push(s.job);
            }
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(job: &str, captures: &[&str]) -> Session {
        Session {
            job: JobId(job.into()),
            kind: "capture".into(),
            source: SourceId("rx".into()),
            center_hz: Hz(433_920_000),
            bandwidth_hz: Hz(12_500),
            captures: captures.iter().map(|c| c.to_string()).collect(),
            features: Value::Null,
            graph: Graph {
                id: crate::graph::GraphId("g".into()),
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            hypotheses: Value::Null,
            frame_decode: Value::Null,
            outcome: Outcome::None,
            started_ns: 1,
            ended_ns: 2,
        }
    }

    #[test]
    fn a_session_round_trips_and_goes_with_its_captures() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).unwrap();
        let a = Session {
            outcome: Outcome::Proposal { id: "01S".into() },
            ..session("a", &["c1", "c2"])
        };
        store.write(&a).unwrap();
        store.write(&session("b", &[])).unwrap();
        assert_eq!(store.list().len(), 2);
        let v: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("sessions/a.json")).unwrap())
                .unwrap();
        assert_eq!(
            v["outcome"],
            serde_json::json!({"kind": "proposal", "id": "01S"})
        );
        let live: HashSet<String> = ["c2".to_string()].into();
        assert!(store.sweep(&live).is_empty());
        assert_eq!(store.sweep(&HashSet::new()), [JobId("a".into())]);
        let left = store.list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].job, JobId("b".into()));
    }
}
