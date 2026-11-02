//! The burst store: SigMF windows a `capture` job's `burst_capture` stage cuts from a
//! channel, under `state_dir/bursts/` with an `index.json` of one [`CaptureRecord`]
//! each, kept within the node's [`BurstPolicy`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use num_complex::Complex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capture::{CaptureMeta, write_cf32};
use crate::types::{Hz, JobId, SourceId, Timestamp};

/// The store's directory under the state directory.
pub const BURSTS_DIR: &str = "bursts";
/// The index file in [`BURSTS_DIR`].
const INDEX_FILE: &str = "index.json";
/// [`BurstPolicy::keep_bytes`] by default: 256 MiB.
pub const BURSTS_KEEP_BYTES_DEFAULT: u64 = 256 * 1024 * 1024;
/// [`BurstPolicy::keep_days`] by default.
pub const BURSTS_KEEP_DAYS_DEFAULT: u32 = 7;
const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;

/// How much the store keeps: expiry removes the oldest unpinned captures until the
/// unpinned bytes fit `keep_bytes`, then every unpinned capture older than `keep_days`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BurstPolicy {
    pub keep_bytes: u64,
    pub keep_days: u32,
    /// Whether the node expires the store when it opens (true by default). The CLI
    /// turns it off for a one-shot that does not own the state directory or was given
    /// no policy of its own, so it never deletes what `zeta serve`'s policy keeps; the
    /// expiry after each write applies either way.
    pub expire_at_open: bool,
}

impl Default for BurstPolicy {
    fn default() -> Self {
        Self {
            keep_bytes: BURSTS_KEEP_BYTES_DEFAULT,
            keep_days: BURSTS_KEEP_DAYS_DEFAULT,
            expire_at_open: true,
        }
    }
}

/// One capture in the store: a SigMF pair and what is known of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
#[schemars(transform = crate::knowledge::require_all)]
pub struct CaptureRecord {
    /// A ULID; the stem of its files.
    pub id: String,
    pub meta_path: PathBuf,
    pub data_path: PathBuf,
    pub source: SourceId,
    /// The captures written by one job share it: the job's id.
    pub set: JobId,
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
    pub sample_rate_hz: Hz,
    pub start_ns: u64,
    pub end_ns: u64,
    /// The size of the data file (cf32, 8 bytes per sample).
    pub bytes: u64,
    /// The window's layer-1 feature record.
    #[serde(default)]
    pub features: Value,
    /// The signal as detected (centre, width, SNR), when a watch found it.
    #[serde(default)]
    pub signal: Option<Value>,
    /// The capture job that wrote it.
    pub job: JobId,
    /// The watch owning that job, when one does.
    #[serde(default)]
    pub watch: Option<JobId>,
    /// The allocations the register gave for the band; `[]` for none.
    #[serde(default = "empty_array")]
    pub allocations: Value,
    /// A pinned capture is never expired nor deleted.
    #[serde(default)]
    pub pinned: bool,
    /// When the store wrote it, ns since the epoch (0 in an index older than the
    /// field: `start_ns` stands in). A capture's age counts from here, not from
    /// `start_ns`: a window cut from an old recording is as new as its writing.
    #[serde(default)]
    pub written_ns: u64,
}

impl CaptureRecord {
    /// The time the policy ages the capture from: `written_ns`, else `start_ns`.
    pub fn age_from_ns(&self) -> u64 {
        if self.written_ns > 0 {
            self.written_ns
        } else {
            self.start_ns
        }
    }
}

fn empty_array() -> Value {
    Value::Array(Vec::new())
}

/// What a writer knows of a capture before it is stored: [`CaptureRecord`] without
/// the id, the paths, the bytes and `pinned`.
#[derive(Clone, Debug, PartialEq)]
pub struct NewCapture {
    pub source: SourceId,
    pub set: JobId,
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
    pub sample_rate_hz: Hz,
    pub start_ns: u64,
    pub end_ns: u64,
    pub features: Value,
    pub signal: Option<Value>,
    pub job: JobId,
    pub watch: Option<JobId>,
    pub allocations: Value,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("unknown capture {0}")]
    Unknown(String),
    #[error("capture {0} is pinned")]
    Pinned(String),
    #[error("io: {0}")]
    Io(String),
}

/// The store: the directory and its index, every change written through.
pub struct BurstStore {
    dir: PathBuf,
    policy: BurstPolicy,
    index: Mutex<Vec<CaptureRecord>>,
}

impl BurstStore {
    /// Open (making it if missing) `state_dir/bursts`, reading `index.json`; a record
    /// whose data or meta file is gone is dropped (and the index rewritten). A missing index
    /// reads as empty; an unreadable one is moved aside as `index.json.broken-<ulid>` and
    /// rebuilt from the `.sigmf-meta` files present (pins are lost). Nothing is expired here: the node calls
    /// [`BurstStore::expire`] once it is open.
    pub fn open(state_dir: &Path, policy: BurstPolicy) -> std::io::Result<Arc<Self>> {
        let dir = state_dir.join(BURSTS_DIR);
        std::fs::create_dir_all(&dir)?;
        let index_path = dir.join(INDEX_FILE);
        let (read, rebuilt) = match std::fs::read(&index_path) {
            Err(_) => (Vec::new(), false),
            Ok(b) => match serde_json::from_slice::<Vec<CaptureRecord>>(&b) {
                Ok(v) => (v, false),
                Err(e) => {
                    let aside = dir.join(format!("{INDEX_FILE}.broken-{}", ulid::Ulid::generate()));
                    eprintln!("zeta: bursts: index unreadable ({e}); moved aside");
                    std::fs::rename(&index_path, aside)?;
                    (rebuild_index(&dir), true)
                }
            },
        };
        let n = read.len();
        let kept: Vec<CaptureRecord> = read
            .into_iter()
            .filter(|r| r.data_path.is_file() && r.meta_path.is_file())
            .collect();
        let store = Self {
            dir,
            policy,
            index: Mutex::new(kept),
        };
        if rebuilt || store.index.lock().unwrap().len() != n {
            store.persist(&store.index.lock().unwrap())?;
        }
        Ok(Arc::new(store))
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write `samples` as one SigMF pair named by a fresh ULID, its one annotation over
    /// the whole window carrying the `zeta:` keys, add its record to the index (written
    /// atomically), then [`expire`](Self::expire).
    pub fn write(
        &self,
        rec: NewCapture,
        samples: &[Complex<f32>],
    ) -> std::io::Result<CaptureRecord> {
        let id = ulid::Ulid::generate().to_string();
        let half = rec.bandwidth_hz.0 / 2;
        let annotation = serde_json::json!({
            "core:sample_start": 0,
            "core:sample_count": samples.len(),
            "core:freq_lower_edge": rec.center_hz.0.saturating_sub(half),
            "core:freq_upper_edge": rec.center_hz.0 + half,
            "zeta:features": rec.features,
            "zeta:signal": rec.signal,
            "zeta:job": rec.job.0,
            "zeta:watch": rec.watch.as_ref().map(|w| w.0.clone()),
            "zeta:source": rec.source.0,
            "zeta:set": rec.set.0,
            "zeta:allocations": rec.allocations,
        });
        let meta = CaptureMeta {
            sample_rate_hz: rec.sample_rate_hz,
            center_hz: rec.center_hz,
            start: Timestamp(rec.start_ns),
            annotations: vec![annotation],
        };
        let mut index = self.index.lock().unwrap();
        let meta_path = write_cf32(&self.dir, &id, &meta, samples)?;
        let record = CaptureRecord {
            data_path: self.dir.join(format!("{id}.sigmf-data")),
            meta_path,
            id,
            source: rec.source,
            set: rec.set,
            center_hz: rec.center_hz,
            bandwidth_hz: rec.bandwidth_hz,
            sample_rate_hz: rec.sample_rate_hz,
            start_ns: rec.start_ns,
            end_ns: rec.end_ns,
            bytes: samples.len() as u64 * 8,
            features: rec.features,
            signal: rec.signal,
            job: rec.job,
            watch: rec.watch,
            allocations: rec.allocations,
            pinned: false,
            written_ns: now_ns(),
        };
        index.push(record.clone());
        if let Err(e) = self.persist(&index) {
            index.pop();
            remove_files(&record);
            return Err(e);
        }
        drop(index);
        self.expire_except(Some(&record.id));
        Ok(record)
    }

    /// Every record, newest first (by [`CaptureRecord::age_from_ns`], then id).
    pub fn list(&self) -> Vec<CaptureRecord> {
        let mut v = self.index.lock().unwrap().clone();
        v.sort_by(|a, b| (b.age_from_ns(), &b.id).cmp(&(a.age_from_ns(), &a.id)));
        v
    }

    /// The record `id`.
    pub fn get(&self, id: &str) -> Option<CaptureRecord> {
        self.index
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.id == id)
            .cloned()
    }

    /// Remove the capture `id` and its files; a pinned one is refused.
    pub fn delete(&self, id: &str) -> Result<CaptureRecord, StoreError> {
        let mut index = self.index.lock().unwrap();
        let i = index
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| StoreError::Unknown(id.into()))?;
        if index[i].pinned {
            return Err(StoreError::Pinned(id.into()));
        }
        let rec = index.remove(i);
        if let Err(e) = self.persist(&index) {
            index.insert(i, rec);
            return Err(StoreError::Io(e.to_string()));
        }
        remove_files(&rec);
        Ok(rec)
    }

    /// Pin the capture `id`: it is never expired nor deleted from then on.
    pub fn pin(&self, id: &str) -> Result<(), StoreError> {
        let mut index = self.index.lock().unwrap();
        let rec = index
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| StoreError::Unknown(id.into()))?;
        if rec.pinned {
            return Ok(());
        }
        rec.pinned = true;
        self.persist(&index).map_err(|e| {
            if let Some(r) = index.iter_mut().find(|r| r.id == id) {
                r.pinned = false;
            }
            StoreError::Io(e.to_string())
        })
    }

    /// Apply the policy: the oldest unpinned captures go until the unpinned rest fit
    /// `keep_bytes` (a pinned capture counts against no budget), then every unpinned capture older than
    /// `keep_days`. Returns the ids removed. A failed index write keeps the records
    /// (their files are left) for the next expiry.
    pub fn expire(&self) -> Vec<String> {
        self.expire_except(None)
    }

    /// [`expire`](Self::expire) sparing the record `keep` (the one just written, whose
    /// id its job has yet to publish); it still counts against the budget.
    fn expire_except(&self, keep: Option<&str>) -> Vec<String> {
        let mut index = self.index.lock().unwrap();
        index.sort_by(|a, b| (a.age_from_ns(), &a.id).cmp(&(b.age_from_ns(), &b.id)));
        // Pinned captures are the operator's: they count against no budget.
        let mut total: u64 = index.iter().filter(|r| !r.pinned).map(|r| r.bytes).sum();
        let mut gone = vec![false; index.len()];
        for (i, r) in index.iter().enumerate() {
            if total <= self.policy.keep_bytes {
                break;
            }
            if !r.pinned && Some(r.id.as_str()) != keep {
                gone[i] = true;
                total -= r.bytes;
            }
        }
        let cutoff =
            now_ns().saturating_sub(u64::from(self.policy.keep_days).saturating_mul(NS_PER_DAY));
        for (i, r) in index.iter().enumerate() {
            if !r.pinned && Some(r.id.as_str()) != keep && r.age_from_ns() < cutoff {
                gone[i] = true;
            }
        }
        if !gone.contains(&true) {
            return Vec::new();
        }
        let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut *index)
            .into_iter()
            .zip(gone)
            .partition(|(_, g)| *g);
        let (removed, kept): (Vec<CaptureRecord>, Vec<CaptureRecord>) = (
            removed.into_iter().map(|(r, _)| r).collect(),
            kept.into_iter().map(|(r, _)| r).collect(),
        );
        if self.persist(&kept).is_err() {
            *index = kept.into_iter().chain(removed).collect();
            return Vec::new();
        }
        *index = kept;
        removed
            .iter()
            .map(|r| {
                remove_files(r);
                r.id.clone()
            })
            .collect()
    }

    /// The bytes of every capture held.
    pub fn total_bytes(&self) -> u64 {
        self.index.lock().unwrap().iter().map(|r| r.bytes).sum()
    }

    /// Write `index` to `index.json` atomically (a temp file, then a rename).
    fn persist(&self, index: &[CaptureRecord]) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(index)?;
        let tmp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.dir.join(INDEX_FILE))
    }
}

/// Rebuild the records from the `.sigmf-meta` files in `dir`, each read from its
/// `zeta:` annotation; a meta that does not read, or without its data file, is skipped.
/// `written_ns` is the data file's modification time.
fn rebuild_index(dir: &Path) -> Vec<CaptureRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sigmf-meta"))
        .filter_map(|p| record_from_meta(&p))
        .collect()
}

fn record_from_meta(meta_path: &Path) -> Option<CaptureRecord> {
    let json: Value = serde_json::from_slice(&std::fs::read(meta_path).ok()?).ok()?;
    let id = meta_path.file_stem()?.to_str()?.to_string();
    let data_path = meta_path.with_extension("sigmf-data");
    let md = std::fs::metadata(&data_path).ok()?;
    let rate = json["global"]["core:sample_rate"].as_u64()?;
    let cap = &json["captures"][0];
    let center = cap["core:frequency"].as_u64()?;
    let start_ns = cap["core:datetime"]
        .as_str()
        .and_then(crate::capture::parse_iso8601)
        .map_or(0, |t| t.0);
    let ann = &json["annotations"][0];
    let lower = ann["core:freq_lower_edge"].as_u64()?;
    let upper = ann["core:freq_upper_edge"].as_u64()?;
    let count = ann["core:sample_count"].as_u64().unwrap_or(md.len() / 8);
    let text = |k: &str| ann[k].as_str().map(str::to_string);
    let written_ns = md
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    Some(CaptureRecord {
        id,
        meta_path: meta_path.to_path_buf(),
        data_path,
        source: SourceId(text("zeta:source")?),
        set: JobId(text("zeta:set")?),
        center_hz: Hz(center),
        bandwidth_hz: Hz(upper.saturating_sub(lower)),
        sample_rate_hz: Hz(rate),
        start_ns,
        end_ns: start_ns + count.saturating_mul(1_000_000_000) / rate.max(1),
        bytes: md.len(),
        features: ann["zeta:features"].clone(),
        signal: Some(ann["zeta:signal"].clone()).filter(|v| !v.is_null()),
        job: JobId(text("zeta:job")?),
        watch: text("zeta:watch").map(JobId),
        allocations: if ann["zeta:allocations"].is_array() {
            ann["zeta:allocations"].clone()
        } else {
            empty_array()
        },
        pinned: false,
        written_ns,
    })
}

/// Remove a record's files; one already gone is no error.
fn remove_files(r: &CaptureRecord) {
    let _ = std::fs::remove_file(&r.data_path);
    let _ = std::fs::remove_file(&r.meta_path);
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples in one test window.
    const WINDOW: usize = 1_000;

    fn new_capture(start_ns: u64) -> NewCapture {
        NewCapture {
            source: SourceId("s".into()),
            set: JobId("set".into()),
            center_hz: Hz(144_800_000),
            bandwidth_hz: Hz(12_500),
            sample_rate_hz: Hz(48_000),
            start_ns,
            end_ns: start_ns + 20_833_333,
            features: serde_json::json!({"snr_db": 20.0}),
            signal: None,
            job: JobId("job".into()),
            watch: None,
            allocations: serde_json::json!([]),
        }
    }

    fn samples() -> Vec<Complex<f32>> {
        vec![Complex::new(0.5, -0.5); WINDOW]
    }

    /// Recent start times, `i` seconds apart, so the age rule keeps them.
    fn t(i: u64) -> u64 {
        now_ns() - 3_600_000_000_000 + i * 1_000_000_000
    }

    fn store(dir: &Path, keep_bytes: u64) -> Arc<BurstStore> {
        BurstStore::open(
            dir,
            BurstPolicy {
                keep_bytes,
                keep_days: 7,
                expire_at_open: true,
            },
        )
        .unwrap()
    }

    #[test]
    fn write_lists_newest_first_and_get_finds_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), u64::MAX);
        let a = s.write(new_capture(t(0)), &samples()).unwrap();
        let b = s.write(new_capture(t(1)), &samples()).unwrap();
        assert_eq!(a.bytes, WINDOW as u64 * 8);
        assert!(a.data_path.is_file() && a.meta_path.is_file());
        assert_eq!(
            std::fs::metadata(&a.data_path).unwrap().len(),
            WINDOW as u64 * 8
        );
        let meta: Value = serde_json::from_slice(&std::fs::read(&a.meta_path).unwrap()).unwrap();
        let ann = &meta["annotations"][0];
        assert_eq!(ann["zeta:job"], "job");
        assert_eq!(ann["zeta:source"], "s");
        assert_eq!(ann["zeta:set"], "set");
        assert_eq!(ann["zeta:watch"], Value::Null);
        assert_eq!(ann["zeta:features"]["snr_db"], 20.0);
        assert_eq!(ann["zeta:allocations"], serde_json::json!([]));
        assert_eq!(meta["captures"][0]["core:frequency"], 144_800_000);
        let ids: Vec<String> = s.list().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, [b.id.clone(), a.id.clone()]);
        assert_eq!(s.get(&a.id), Some(a.clone()));
        assert_eq!(s.get("nope"), None);
        assert_eq!(s.total_bytes(), 2 * WINDOW as u64 * 8);
        assert_eq!(s.delete(&a.id), Ok(a.clone()));
        assert!(!a.data_path.exists());
        assert_eq!(s.delete(&a.id), Err(StoreError::Unknown(a.id.clone())));
    }

    #[test]
    fn expire_removes_oldest_unpinned_until_the_budget_holds() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        let s = store(dir.path(), w * 5 / 2);
        let recs: Vec<CaptureRecord> = (0..4)
            .map(|i| s.write(new_capture(t(i)), &samples()).unwrap())
            .collect();
        let ids: Vec<String> = s.list().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, [recs[3].id.clone(), recs[2].id.clone()]);
        assert!(!recs[0].data_path.exists() && !recs[1].meta_path.exists());
        assert!(s.total_bytes() <= w * 5 / 2);
    }

    #[test]
    fn expire_never_removes_a_pinned_capture() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        let s = store(dir.path(), w);
        let first = s.write(new_capture(t(0)), &samples()).unwrap();
        s.pin(&first.id).unwrap();
        let _second = s.write(new_capture(t(1)), &samples()).unwrap();
        let third = s.write(new_capture(t(2)), &samples()).unwrap();
        let ids: Vec<String> = s.list().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, [third.id.clone(), first.id.clone()]);
        assert_eq!(
            s.delete(&first.id),
            Err(StoreError::Pinned(first.id.clone()))
        );
        assert_eq!(s.pin("nope"), Err(StoreError::Unknown("nope".into())));
    }

    #[test]
    fn expire_by_age_uses_keep_days() {
        let dir = tempfile::tempdir().unwrap();
        let (old, fresh) = {
            let s = store(dir.path(), u64::MAX);
            let old = s.write(new_capture(t(0)), &samples()).unwrap();
            let fresh = s.write(new_capture(t(1)), &samples()).unwrap();
            (old, fresh)
        };
        // Written 8 days ago (an older index without `written_ns` ages from `start_ns`).
        let path = dir.path().join(BURSTS_DIR).join(INDEX_FILE);
        let mut index: Vec<Value> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for r in &mut index {
            if r["id"] == old.id.as_str() {
                r.as_object_mut().unwrap().remove("written_ns");
                r["start_ns"] = (now_ns() - 8 * NS_PER_DAY).into();
            }
        }
        std::fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();
        let s = store(dir.path(), u64::MAX);
        assert_eq!(s.expire(), std::slice::from_ref(&old.id));
        assert!(!old.data_path.exists());
        assert_eq!(s.list(), std::slice::from_ref(&fresh));
        assert!(s.expire().is_empty());
    }

    #[test]
    fn a_window_of_an_old_recording_ages_from_its_writing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), u64::MAX);
        let rec = s.write(new_capture(0), &samples()).unwrap();
        assert_eq!(s.get(&rec.id), Some(rec));
    }

    #[test]
    fn the_index_survives_reopen_and_a_missing_data_file_drops_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = {
            let s = store(dir.path(), u64::MAX);
            let a = s.write(new_capture(t(0)), &samples()).unwrap();
            let b = s.write(new_capture(t(1)), &samples()).unwrap();
            s.pin(&a.id).unwrap();
            (a, b)
        };
        let s = store(dir.path(), u64::MAX);
        let list = s.list();
        assert_eq!(list.len(), 2);
        assert!(list[1].pinned);
        assert_eq!(list[1].id, a.id);
        std::fs::remove_file(&b.data_path).unwrap();
        drop(s);
        let s = store(dir.path(), u64::MAX);
        let ids: Vec<String> = s.list().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, std::slice::from_ref(&a.id));
        // The drop was written back: a third open reads the same.
        let index: Vec<CaptureRecord> = serde_json::from_slice(
            &std::fs::read(dir.path().join(BURSTS_DIR).join(INDEX_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn a_write_never_expires_the_record_it_just_wrote() {
        let dir = tempfile::tempdir().unwrap();
        // The budget is smaller than one window.
        let s = store(dir.path(), 8);
        let a = s.write(new_capture(t(0)), &samples()).unwrap();
        assert!(a.data_path.is_file() && a.meta_path.is_file());
        assert_eq!(s.get(&a.id), Some(a.clone()));
        // The next write expires the older one, never itself.
        let b = s.write(new_capture(t(1)), &samples()).unwrap();
        assert!(!a.data_path.exists());
        assert_eq!(s.list(), std::slice::from_ref(&b));
    }

    #[test]
    fn an_unreadable_index_is_moved_aside_and_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = {
            let s = store(dir.path(), u64::MAX);
            let mut n = new_capture(t(0));
            n.signal = Some(serde_json::json!({"snr_db": 12.0}));
            n.watch = Some(JobId("w".into()));
            (
                s.write(n, &samples()).unwrap(),
                s.write(new_capture(t(1)), &samples()).unwrap(),
            )
        };
        let bursts = dir.path().join(BURSTS_DIR);
        std::fs::write(bursts.join(INDEX_FILE), b"{ not json").unwrap();
        let s = store(dir.path(), u64::MAX);
        let mut got = s.list();
        got.sort_by(|x, y| x.id.cmp(&y.id));
        let mut want = vec![a, b];
        want.sort_by(|x, y| x.id.cmp(&y.id));
        for (g, w) in got.iter().zip(&want) {
            let mut w = w.clone();
            w.written_ns = g.written_ns;
            assert_eq!(g, &w);
        }
        assert_eq!(got.len(), 2);
        let aside = std::fs::read_dir(&bursts)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("index.json.broken-")
            })
            .count();
        assert_eq!(aside, 1);
        // The rebuilt index was written: a reopen reads it without moving anything aside.
        drop(s);
        assert_eq!(store(dir.path(), u64::MAX).list().len(), 2);
    }
}
