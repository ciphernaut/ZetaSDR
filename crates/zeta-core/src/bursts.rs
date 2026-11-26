//! The burst store: SigMF windows a `capture` job's `burst_capture` stage cuts from a
//! channel, under `state_dir/bursts/` with an `index.json` of one [`CaptureRecord`]
//! each, kept within the node's [`BurstPolicy`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
/// [`BurstPolicy::trial_keep_bytes`] by default: 64 MiB.
pub const BURSTS_TRIAL_KEEP_BYTES_DEFAULT: u64 = 64 * 1024 * 1024;
/// [`BurstPolicy::keep_days`] by default.
pub const BURSTS_KEEP_DAYS_DEFAULT: u32 = 7;
const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;

/// How much the store keeps: expiry removes the oldest unpinned captures until the
/// unpinned bytes fit `keep_bytes`, then every unpinned capture older than `keep_days`.
/// Trial captures (a `choose` job's slices) count against `trial_keep_bytes` only and the
/// rest against `keep_bytes` only: neither kind ever expires the other for room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BurstPolicy {
    pub keep_bytes: u64,
    pub keep_days: u32,
    /// The budget of the unpinned trial captures.
    pub trial_keep_bytes: u64,
    /// Whether this process applies the policy: when the store opens and after each
    /// write (true by default). The CLI turns it off for a one-shot that does not own
    /// the state directory or was given no policy of its own, so it never deletes what
    /// `zeta serve`'s policy keeps; such a process writes without expiring.
    pub expire_at_open: bool,
}

impl Default for BurstPolicy {
    fn default() -> Self {
        Self {
            keep_bytes: BURSTS_KEEP_BYTES_DEFAULT,
            keep_days: BURSTS_KEEP_DAYS_DEFAULT,
            trial_keep_bytes: BURSTS_TRIAL_KEEP_BYTES_DEFAULT,
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
    /// A slice a `choose` job's trial wrote: it counts against the trial quota, not the
    /// operator's budget.
    #[serde(default)]
    pub trial: bool,
    /// What a settled `choose` trial slice keeps: each source's guess and the answer
    /// ([`BurstStore::annotate`]); `null` for any other capture.
    #[serde(default)]
    pub training: Option<Value>,
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
    /// The IQ file's centre when it differs from the record's band (a slice of a wider
    /// ring); `None` when the file is centred on `center_hz`.
    pub file_center_hz: Option<Hz>,
    /// Written by a trial: kept under the trial quota.
    pub trial: bool,
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
    /// The trial bytes [`TrialReservation`]s hold now. Added to only under the index
    /// lock, beside the pinned trial bytes it is checked with.
    reserved: Arc<AtomicU64>,
}

/// Bytes of the trial quota held for a trial slice while it is written
/// ([`BurstStore::reserve_trial`]): given back on drop, or taken over by the pinned
/// capture [`BurstStore::write_trial`] stores.
#[must_use = "a reservation is given back when dropped"]
#[derive(Debug)]
pub struct TrialReservation {
    bytes: u64,
    held: Arc<AtomicU64>,
}

impl TrialReservation {
    /// The bytes held.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for TrialReservation {
    fn drop(&mut self) {
        self.held.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

impl BurstStore {
    /// Open (making it if missing) `state_dir/bursts`, reading `index.json`; a record
    /// whose data or meta file is gone is dropped (and the index rewritten). A missing index
    /// reads as empty; an unreadable one is moved aside as `index.json.broken-<ulid>` and
    /// rebuilt from the `.sigmf-meta` files present (pins are lost). Nothing is expired here:
    /// the node calls [`BurstStore::expire`] once it is open, and its owner
    /// [`BurstStore::unpin_trials`].
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
            .map(|mut r| {
                r.meta_path = resolve(&dir, &r.meta_path);
                r.data_path = resolve(&dir, &r.data_path);
                r
            })
            .filter(|r| r.data_path.is_file() && r.meta_path.is_file())
            .collect();
        let store = Self {
            dir,
            policy,
            index: Mutex::new(kept),
            reserved: Arc::new(AtomicU64::new(0)),
        };
        if rebuilt || store.index.lock().unwrap().len() != n {
            store.persist(&store.index.lock().unwrap())?;
        }
        Ok(Arc::new(store))
    }

    /// Unpin every pinned trial slice and rewrite the index: a trial's pin is its running
    /// `choose`'s, and none survives a restart, so a slice left pinned by a crash would
    /// hold trial quota for good. For the node that owns the state directory, at its start
    /// only: a one-shot beside a running `serve` would unpin the slices serve's chooses hold
    /// and write serve's index. How many were unpinned; a failed write keeps them pinned.
    pub fn unpin_trials(&self) -> std::io::Result<usize> {
        let mut index = self.index.lock().unwrap();
        let stale: Vec<usize> = (0..index.len())
            .filter(|&i| index[i].trial && index[i].pinned)
            .collect();
        if stale.is_empty() {
            return Ok(0);
        }
        for &i in &stale {
            index[i].pinned = false;
        }
        if let Err(e) = self.persist(&index) {
            for &i in &stale {
                index[i].pinned = true;
            }
            return Err(e);
        }
        Ok(stale.len())
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What the store keeps.
    pub fn policy(&self) -> BurstPolicy {
        self.policy
    }

    /// Write `samples` as one SigMF pair named by a fresh ULID, its one annotation over
    /// the whole window carrying the `zeta:` keys, add its record to the index (written
    /// atomically), then [`expire`](Self::expire). The files are written before the
    /// index is locked: the store answers meanwhile, and lists the capture once its
    /// record is in.
    pub fn write(
        &self,
        rec: NewCapture,
        samples: &[Complex<f32>],
    ) -> std::io::Result<CaptureRecord> {
        self.store(rec, samples, None)
    }

    /// [`write`](Self::write) a trial slice into the bytes `reservation` holds: stored
    /// `trial` and pinned, the reservation's bytes taken over by the pin under the same
    /// index lock, so the quota counts them once and expiry never takes the slice before
    /// its job unpins it. Refused when the slice is larger than the reservation.
    pub fn write_trial(
        &self,
        rec: NewCapture,
        samples: &[Complex<f32>],
        reservation: TrialReservation,
    ) -> std::io::Result<CaptureRecord> {
        if samples.len() as u64 * 8 > reservation.bytes {
            return Err(std::io::Error::other(format!(
                "the slice ({} bytes) is larger than its reservation ({} bytes)",
                samples.len() as u64 * 8,
                reservation.bytes
            )));
        }
        self.store(
            NewCapture { trial: true, ..rec },
            samples,
            Some(reservation),
        )
    }

    /// [`write`](Self::write), pinned when a trial `reservation` is given (released once
    /// the record is in, under the index lock).
    fn store(
        &self,
        rec: NewCapture,
        samples: &[Complex<f32>],
        reservation: Option<TrialReservation>,
    ) -> std::io::Result<CaptureRecord> {
        let id = ulid::Ulid::generate().to_string();
        let half = rec.bandwidth_hz.0 / 2;
        let mut annotation = serde_json::json!({
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
            "zeta:trial": rec.trial,
        });
        // A record narrower than its file is a selection cut from a wider recording.
        if rec.file_center_hz.is_some() {
            annotation["core:label"] = "selection".into();
        }
        let meta = CaptureMeta {
            sample_rate_hz: rec.sample_rate_hz,
            center_hz: rec.file_center_hz.unwrap_or(rec.center_hz),
            start: Timestamp(rec.start_ns),
            annotations: vec![annotation],
        };
        // A fresh id names files no record has: no lock is needed to write them.
        let meta_path = write_cf32(&self.dir, &id, &meta, samples)?;
        #[cfg(test)]
        tests::writing();
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
            pinned: reservation.is_some(),
            written_ns: now_ns(),
            trial: rec.trial,
            training: None,
        };
        let mut index = self.index.lock().unwrap();
        index.push(record.clone());
        if let Err(e) = self.persist(&index) {
            index.pop();
            drop(index);
            remove_files(&record);
            return Err(e);
        }
        // The pin holds the bytes from here: given back while the index is still locked.
        drop(reservation);
        drop(index);
        if self.policy.expire_at_open {
            self.expire_except(Some(&record.id));
        }
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

    /// Set the training record of the capture `id`: written into its `.sigmf-meta` as
    /// `zeta:training` (atomically, so an index rebuilt from the metas keeps it), then
    /// its index entry rewritten atomically, as [`pin`](Self::pin) does. A failure
    /// leaves the previous record in memory and in the meta. The capture's expiry is
    /// unchanged.
    pub fn annotate(&self, id: &str, training: Value) -> Result<(), StoreError> {
        let mut index = self.index.lock().unwrap();
        let rec = index
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| StoreError::Unknown(id.into()))?;
        let meta_path = rec.meta_path.clone();
        write_meta_training(&meta_path, Some(&training))
            .map_err(|e| StoreError::Io(e.to_string()))?;
        let before = rec.training.replace(training);
        self.persist(&index).map_err(|e| {
            if let Some(r) = index.iter_mut().find(|r| r.id == id) {
                r.training = before.clone();
            }
            let _ = write_meta_training(&meta_path, before.as_ref());
            StoreError::Io(e.to_string())
        })
    }

    /// Unpin the capture `id` ([`pin`](Self::pin) undone): expiry and delete may take it
    /// again.
    pub fn unpin(&self, id: &str) -> Result<(), StoreError> {
        let mut index = self.index.lock().unwrap();
        let rec = index
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| StoreError::Unknown(id.into()))?;
        if !rec.pinned {
            return Ok(());
        }
        rec.pinned = false;
        self.persist(&index).map_err(|e| {
            if let Some(r) = index.iter_mut().find(|r| r.id == id) {
                r.pinned = true;
            }
            StoreError::Io(e.to_string())
        })
    }

    /// Apply the policy: the oldest unpinned captures go until the unpinned rest fit
    /// `keep_bytes` (a pinned capture counts against no budget; trials fit
    /// `trial_keep_bytes` apart), then every unpinned capture older than
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
        // Pinned captures are the operator's: they count against no budget. Trials and
        // the rest each keep to their own budget.
        let mut gone = vec![false; index.len()];
        for (trial, budget) in [
            (false, self.policy.keep_bytes),
            (true, self.policy.trial_keep_bytes),
        ] {
            let mut total: u64 = index
                .iter()
                .filter(|r| !r.pinned && r.trial == trial)
                .map(|r| r.bytes)
                .sum();
            for (i, r) in index.iter().enumerate() {
                if total <= budget {
                    break;
                }
                if !r.pinned && r.trial == trial && Some(r.id.as_str()) != keep {
                    gone[i] = true;
                    total -= r.bytes;
                }
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

    /// The bytes of the pinned trial captures (the slices `choose` jobs are replaying),
    /// which no expiry removes: what a new slice must fit beside under
    /// `trial_keep_bytes`.
    pub fn pinned_trial_bytes(&self) -> u64 {
        self.index
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.pinned && r.trial)
            .map(|r| r.bytes)
            .sum()
    }

    /// The trial bytes free for a new slice: `trial_keep_bytes` less the pinned trial
    /// captures and the reservations held. A size, not a hold:
    /// [`reserve_trial`](Self::reserve_trial) may still refuse it once another slice
    /// reserved.
    pub fn trial_room(&self) -> u64 {
        let index = self.index.lock().unwrap();
        self.room(&index)
    }

    /// Hold `bytes` of the trial quota for a slice to be written
    /// ([`write_trial`](Self::write_trial)); `None` when they do not fit beside the
    /// pinned trial captures and the reservations held. Taken under the index lock, so
    /// two slices reserved at once never both pass the quota.
    pub fn reserve_trial(&self, bytes: u64) -> Option<TrialReservation> {
        let index = self.index.lock().unwrap();
        if bytes > self.room(&index) {
            return None;
        }
        self.reserved.fetch_add(bytes, Ordering::SeqCst);
        Some(TrialReservation {
            bytes,
            held: self.reserved.clone(),
        })
    }

    /// [`trial_room`](Self::trial_room) of the locked `index`.
    fn room(&self, index: &[CaptureRecord]) -> u64 {
        let pinned: u64 = index
            .iter()
            .filter(|r| r.pinned && r.trial)
            .map(|r| r.bytes)
            .sum();
        self.policy
            .trial_keep_bytes
            .saturating_sub(pinned)
            .saturating_sub(self.reserved.load(Ordering::SeqCst))
    }

    /// The bytes of every capture held.
    pub fn total_bytes(&self) -> u64 {
        self.index.lock().unwrap().iter().map(|r| r.bytes).sum()
    }

    /// Write `index` to `index.json` atomically (a temp file, then a rename), compact,
    /// each record's files named relative to the store's directory (resolved against it
    /// again at [`open`](Self::open), so a moved state directory keeps its captures).
    fn persist(&self, index: &[CaptureRecord]) -> std::io::Result<()> {
        let relative: Vec<CaptureRecord> = index
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.meta_path = relative_to(&self.dir, &r.meta_path);
                r.data_path = relative_to(&self.dir, &r.data_path);
                r
            })
            .collect();
        let json = serde_json::to_vec(&relative)?;
        let tmp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.dir.join(INDEX_FILE))
    }
}

/// Set (or, with `None`, remove) `zeta:training` in the first annotation of the
/// `.sigmf-meta` at `path`: a temp file, then a rename.
fn write_meta_training(path: &Path, training: Option<&Value>) -> std::io::Result<()> {
    let mut json: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let ann = json["annotations"][0]
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("the meta has no annotation"))?;
    match training {
        Some(t) => ann.insert("zeta:training".into(), t.clone()),
        None => ann.remove("zeta:training"),
    };
    let tmp = path.with_extension("sigmf-meta.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&json)?)?;
    std::fs::rename(&tmp, path)
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
    cap["core:frequency"].as_u64()?;
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
        // The band's centre, which is the file's `center` unless the file is a wider
        // recording the record's band was cut from.
        center_hz: Hz(lower + upper.saturating_sub(lower) / 2),
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
        trial: ann["zeta:trial"].as_bool().unwrap_or(false),
        training: Some(ann["zeta:training"].clone()).filter(|v| !v.is_null()),
    })
}

/// A file name as the index stores it: relative to `dir` when it lies there.
fn relative_to(dir: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(dir)
        .map_or_else(|_| p.to_path_buf(), Path::to_path_buf)
}

/// A file name read from the index: a relative one under `dir`; an absolute one (an index
/// written before names were relative) as it is while it exists, else its file name under
/// `dir` (the state directory moved).
fn resolve(dir: &Path, p: &Path) -> PathBuf {
    if p.is_relative() {
        return dir.join(p);
    }
    if p.exists() {
        return p.to_path_buf();
    }
    p.file_name()
        .map_or_else(|| p.to_path_buf(), |f| dir.join(f))
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

    thread_local! {
        /// Run on this thread once a capture's files are written (a slow write).
        static WRITING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// [`BurstStore::write`]'s hook: the thread's [`WRITING`], once.
    pub(super) fn writing() {
        if let Some(f) = WRITING.with_borrow_mut(Option::take) {
            f();
        }
    }

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
            file_center_hz: None,
            trial: false,
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
                trial_keep_bytes: u64::MAX,
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
    fn the_index_is_free_while_a_capture_is_written() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), u64::MAX);
        let a = s.write(new_capture(t(0)), &samples()).unwrap();
        // The second write stalls once its files are written, until `go`.
        let (entered, started) = mpsc::channel();
        let (go, wait) = mpsc::channel::<()>();
        let writer = {
            let s = s.clone();
            std::thread::spawn(move || {
                WRITING.with_borrow_mut(|w| {
                    *w = Some(Box::new(move || {
                        entered.send(()).unwrap();
                        let _ = wait.recv();
                    }));
                });
                s.write(new_capture(t(1)), &samples()).unwrap()
            })
        };
        started
            .recv_timeout(Duration::from_secs(10))
            .expect("the write starts");
        // Listed from another thread, so an index held by the write cannot hang the test.
        let (listed, got) = mpsc::channel();
        let lister = {
            let s = s.clone();
            std::thread::spawn(move || listed.send(s.list()).unwrap())
        };
        let during = got.recv_timeout(Duration::from_secs(2));
        go.send(()).unwrap();
        let b = writer.join().unwrap();
        lister.join().unwrap();
        let during = during.expect("list() answers while a capture is written");
        // The capture being written is not listed until its record is in.
        let ids: Vec<String> = during.into_iter().map(|r| r.id).collect();
        assert_eq!(ids, std::slice::from_ref(&a.id));
        let ids: Vec<String> = s.list().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, [b.id, a.id]);
    }

    #[test]
    fn the_index_is_compact_with_relative_file_names() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), u64::MAX);
        let a = s.write(new_capture(t(0)), &samples()).unwrap();
        let text = std::fs::read_to_string(dir.path().join(BURSTS_DIR).join(INDEX_FILE)).unwrap();
        assert!(!text.contains('\n'), "compact: {text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v[0]["meta_path"], format!("{}.sigmf-meta", a.id));
        assert_eq!(v[0]["data_path"], format!("{}.sigmf-data", a.id));
        // The API keeps absolute paths.
        assert!(a.meta_path.is_absolute() && a.data_path.is_absolute());
    }

    #[test]
    fn a_moved_state_dir_keeps_every_record() {
        let root = tempfile::tempdir().unwrap();
        let (from, to) = (root.path().join("a"), root.path().join("b"));
        let ids: Vec<String> = {
            let s = store(&from, u64::MAX);
            (0..3)
                .map(|i| s.write(new_capture(t(i)), &samples()).unwrap().id)
                .collect()
        };
        std::fs::rename(&from, &to).unwrap();
        let s = store(&to, u64::MAX);
        let listed = s.list();
        assert_eq!(listed.len(), 3);
        for r in &listed {
            assert!(ids.contains(&r.id));
            assert!(r.meta_path.starts_with(&to) && r.meta_path.is_file());
            assert!(r.data_path.starts_with(&to) && r.data_path.is_file());
        }
    }

    #[test]
    fn an_index_with_absolute_names_from_a_moved_dir_still_reads() {
        let root = tempfile::tempdir().unwrap();
        let (from, to) = (root.path().join("a"), root.path().join("b"));
        {
            let s = store(&from, u64::MAX);
            s.write(new_capture(t(0)), &samples()).unwrap();
        }
        // Rewrite the index as an older store did: absolute names under `from`.
        let index = from.join(BURSTS_DIR).join(INDEX_FILE);
        let mut v: Value = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
        for k in ["meta_path", "data_path"] {
            let name = v[0][k].as_str().unwrap().to_string();
            v[0][k] = from
                .join(BURSTS_DIR)
                .join(name)
                .display()
                .to_string()
                .into();
        }
        std::fs::write(&index, serde_json::to_vec(&v).unwrap()).unwrap();
        std::fs::rename(&from, &to).unwrap();
        assert_eq!(store(&to, u64::MAX).list().len(), 1);
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

    fn two_budgets(dir: &Path, keep_bytes: u64, trial_keep_bytes: u64) -> Arc<BurstStore> {
        BurstStore::open(
            dir,
            BurstPolicy {
                keep_bytes,
                keep_days: 7,
                trial_keep_bytes,
                expire_at_open: true,
            },
        )
        .unwrap()
    }

    fn trial_capture(start_ns: u64) -> NewCapture {
        NewCapture {
            trial: true,
            ..new_capture(start_ns)
        }
    }

    #[test]
    fn a_trial_never_expires_an_operator_capture() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        // The operator's budget holds one; the trial quota holds two.
        let s = two_budgets(dir.path(), w, w * 2);
        let operator = s.write(new_capture(t(0)), &samples()).unwrap();
        let trials: Vec<CaptureRecord> = (1..5)
            .map(|i| s.write(trial_capture(t(i)), &samples()).unwrap())
            .collect();
        let ids: Vec<String> = s.list().into_iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            [&trials[3].id, &trials[2].id, &operator.id].map(String::clone)
        );
        assert!(operator.data_path.exists() && !trials[0].data_path.exists());
        assert!(s.get(&trials[3].id).unwrap().trial);
    }

    #[test]
    fn a_reserved_trial_is_stored_pinned_and_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        let s = two_budgets(dir.path(), u64::MAX, w * 2);
        let r = s.reserve_trial(w).expect("one window fits");
        assert_eq!((r.bytes(), s.trial_room()), (w, w));
        assert!(s.reserve_trial(w + 1).is_none());
        let rec = s.write_trial(new_capture(t(0)), &samples(), r).unwrap();
        assert!(rec.trial && rec.pinned, "{rec:?}");
        assert_eq!(s.get(&rec.id), Some(rec.clone()));
        // The pin took the reservation's bytes over: one window is free, not none.
        assert_eq!(s.trial_room(), w);
        s.unpin(&rec.id).unwrap();
        assert_eq!(s.trial_room(), w * 2);
        // A slice larger than its reservation is refused, and nothing is held after.
        let small = s.reserve_trial(w / 2).unwrap();
        assert!(
            s.write_trial(trial_capture(t(1)), &samples(), small)
                .is_err()
        );
        assert_eq!(s.trial_room(), w * 2);
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn open_leaves_a_pinned_trial_and_unpin_trials_lets_it_go() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        let s = two_budgets(dir.path(), u64::MAX, w * 2);
        // A choose's slice, pinned, and an operator's pinned capture: then a restart.
        let r = s.reserve_trial(w).unwrap();
        let slice = s.write_trial(trial_capture(t(0)), &samples(), r).unwrap();
        let operator = s.write(new_capture(t(1)), &samples()).unwrap();
        s.pin(&operator.id).unwrap();
        drop(s);
        let path = dir.path().join(BURSTS_DIR).join(INDEX_FILE);
        let before = std::fs::read(&path).unwrap();
        // Opening reads: the pin stands, the index is untouched (a one-shot's open).
        let s = two_budgets(dir.path(), u64::MAX, w * 2);
        assert!(s.get(&slice.id).unwrap().pinned);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // The owner lets the trial go, the operator's pin stays, and the index says so.
        assert_eq!(s.unpin_trials().unwrap(), 1);
        assert!(!s.get(&slice.id).unwrap().pinned);
        assert!(s.get(&operator.id).unwrap().pinned);
        assert_eq!(s.trial_room(), w * 2);
        assert_eq!(s.unpin_trials().unwrap(), 0);
        let index: Vec<CaptureRecord> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let rec = index.iter().find(|r| r.id == slice.id).unwrap();
        assert!(!rec.pinned, "{rec:?}");
    }

    #[test]
    fn pinned_trial_bytes_count_only_pinned_trials() {
        let dir = tempfile::tempdir().unwrap();
        let s = two_budgets(dir.path(), u64::MAX, u64::MAX);
        let a = s.write(trial_capture(t(0)), &samples()).unwrap();
        let b = s.write(trial_capture(t(1)), &samples()).unwrap();
        let operator = s.write(new_capture(t(2)), &samples()).unwrap();
        assert_eq!(s.pinned_trial_bytes(), 0);
        s.pin(&a.id).unwrap();
        s.pin(&operator.id).unwrap();
        assert_eq!(s.pinned_trial_bytes(), a.bytes);
        s.pin(&b.id).unwrap();
        assert_eq!(s.pinned_trial_bytes(), a.bytes + b.bytes);
        s.unpin(&a.id).unwrap();
        assert_eq!(s.pinned_trial_bytes(), b.bytes);
    }

    #[test]
    fn operator_captures_never_expire_a_trial() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        let s = two_budgets(dir.path(), w, w);
        let trial = s.write(trial_capture(t(0)), &samples()).unwrap();
        let a = s.write(new_capture(t(1)), &samples()).unwrap();
        let b = s.write(new_capture(t(2)), &samples()).unwrap();
        assert!(s.get(&trial.id).is_some());
        assert!(s.get(&a.id).is_none() && s.get(&b.id).is_some());
    }

    fn training() -> Value {
        serde_json::json!({"sources": [], "settled": {"row": "dmr_decode", "step": 2}})
    }

    #[test]
    fn annotate_an_unknown_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), u64::MAX);
        assert_eq!(
            s.annotate("nope", training()),
            Err(StoreError::Unknown("nope".into()))
        );
    }

    #[test]
    fn an_annotation_survives_a_reopen_and_a_rebuilt_index() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), u64::MAX);
        let rec = s.write(trial_capture(t(0)), &samples()).unwrap();
        s.annotate(&rec.id, training()).unwrap();
        drop(s);
        let back = store(dir.path(), u64::MAX);
        assert_eq!(back.get(&rec.id).unwrap().training, Some(training()));
        drop(back);
        // An unreadable index is rebuilt from the metas.
        std::fs::write(dir.path().join(BURSTS_DIR).join(INDEX_FILE), b"not json").unwrap();
        let rebuilt = store(dir.path(), u64::MAX);
        let r = rebuilt.get(&rec.id).unwrap();
        assert_eq!(r.training, Some(training()));
        assert!(r.trial);
    }

    #[test]
    fn a_failed_annotate_keeps_the_previous_record() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), u64::MAX);
        let rec = s.write(trial_capture(t(0)), &samples()).unwrap();
        s.annotate(&rec.id, training()).unwrap();
        // A directory where the index's temp file goes makes its write fail.
        let tmp = dir
            .path()
            .join(BURSTS_DIR)
            .join(format!("{INDEX_FILE}.tmp"));
        std::fs::create_dir(&tmp).unwrap();
        let other = serde_json::json!({"settled": {"row": "other"}});
        assert!(matches!(s.annotate(&rec.id, other), Err(StoreError::Io(_))));
        assert_eq!(s.get(&rec.id).unwrap().training, Some(training()));
        let meta: Value = serde_json::from_slice(&std::fs::read(&rec.meta_path).unwrap()).unwrap();
        assert_eq!(meta["annotations"][0]["zeta:training"], training());
    }

    #[test]
    fn an_annotated_trial_still_expires_under_the_trial_quota() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        let s = two_budgets(dir.path(), w * 10, w);
        let a = s.write(trial_capture(t(0)), &samples()).unwrap();
        s.annotate(&a.id, training()).unwrap();
        assert!(!s.get(&a.id).unwrap().pinned);
        // The quota holds one: the next trial takes the annotated one's place.
        let b = s.write(trial_capture(t(1)), &samples()).unwrap();
        assert!(s.get(&a.id).is_none() && !a.data_path.exists());
        assert!(s.get(&b.id).is_some());
        // An annotated trial counts: the older one goes when it is the newest.
        s.annotate(&b.id, training()).unwrap();
        let c = s.write(trial_capture(t(2)), &samples()).unwrap();
        assert!(s.get(&b.id).is_none() && s.get(&c.id).is_some());
    }

    #[test]
    fn a_pinned_trial_counts_against_neither_budget() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        let s = two_budgets(dir.path(), w, w);
        let pinned = s.write(trial_capture(t(0)), &samples()).unwrap();
        s.pin(&pinned.id).unwrap();
        let next = s.write(trial_capture(t(1)), &samples()).unwrap();
        let op = s.write(new_capture(t(2)), &samples()).unwrap();
        for r in [&pinned, &next, &op] {
            assert!(s.get(&r.id).is_some());
        }
    }

    #[test]
    fn a_store_not_applying_the_policy_never_expires_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let w = WINDOW as u64 * 8;
        let s = BurstStore::open(
            dir.path(),
            BurstPolicy {
                keep_bytes: w,
                keep_days: 7,
                trial_keep_bytes: w,
                expire_at_open: false,
            },
        )
        .unwrap();
        let a = s.write(new_capture(t(0)), &samples()).unwrap();
        let b = s.write(new_capture(t(1)), &samples()).unwrap();
        let c = s.write(trial_capture(t(2)), &samples()).unwrap();
        let d = s.write(trial_capture(t(3)), &samples()).unwrap();
        for r in [&a, &b, &c, &d] {
            assert!(s.get(&r.id).is_some(), "{} expired", r.id);
        }
    }

    #[test]
    fn a_trial_flag_is_always_written_to_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let s = two_budgets(dir.path(), u64::MAX, u64::MAX);
        s.write(new_capture(t(0)), &samples()).unwrap();
        let index: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(BURSTS_DIR).join(INDEX_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(index[0]["trial"], false);
    }

    #[test]
    fn a_trial_flag_and_a_file_centre_survive_a_rebuilt_index() {
        let dir = tempfile::tempdir().unwrap();
        let s = two_budgets(dir.path(), u64::MAX, u64::MAX);
        let rec = s
            .write(
                NewCapture {
                    file_center_hz: Some(Hz(145_000_000)),
                    ..trial_capture(t(0))
                },
                &samples(),
            )
            .unwrap();
        let meta: Value = serde_json::from_slice(&std::fs::read(&rec.meta_path).unwrap()).unwrap();
        assert_eq!(meta["captures"][0]["core:frequency"], 145_000_000);
        assert_eq!(rec.center_hz, Hz(144_800_000));
        drop(s);
        std::fs::write(dir.path().join(BURSTS_DIR).join(INDEX_FILE), b"x").unwrap();
        let s = two_budgets(dir.path(), u64::MAX, u64::MAX);
        let back = s.get(&rec.id).unwrap();
        assert!(back.trial);
        assert_eq!(back.center_hz, Hz(144_800_000));
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
