//! The packet capture store: pcapng files a `packet_capture` stage appends decoded
//! frames to, under `state_dir/packets/` with an `index.json` of one
//! [`PacketCaptureRecord`] each, kept within a [`BurstPolicy`] of its own
//! ([`packet_policy`]: [`PACKET_CAPTURE_KEEP_BYTES`] and the node's `keep_days`, apart
//! from the burst store's budget), each capture aged by its last write. A capture a job
//! is still writing is never expired nor deleted; one cut off by a crash is trimmed to
//! whole blocks at open. A process that does not own the state directory (a one-shot
//! beside `zeta serve`) opens the store read-only ([`PacketStore::open_read_only`]): it
//! reads the index as serve last wrote it and changes nothing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::bursts::BurstPolicy;
use crate::modes::{Access, Content};
use crate::pcapng;
use crate::stream::LinkType;
use crate::types::{Hz, JobId, SourceId, Timestamp};

/// The largest one capture file grows to before the writer starts a fresh one.
pub const PACKET_CAPTURE_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// The bytes the store keeps by default before the oldest closed captures go: 16 files
/// at the roll size, so a rolled file outlives the next capture's start.
pub const PACKET_CAPTURE_KEEP_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// The store's directory under the state directory.
pub const PACKETS_DIR: &str = "packets";
const INDEX_FILE: &str = "index.json";
const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;
/// How often `write` rewrites the index at most.
const INDEX_EVERY: Duration = Duration::from_secs(1);

/// One capture file in the store and what is known of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
#[schemars(transform = crate::knowledge::require_all)]
pub struct PacketCaptureRecord {
    /// A fresh ULID; the file is `<dir>/<id>.pcapng`.
    pub id: String,
    /// The decoder job captured.
    pub job: JobId,
    pub source: SourceId,
    pub center_hz: Hz,
    pub link_type: LinkType,
    /// The decoder job's row id.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub mode: Option<String>,
    /// The row's access.
    #[schemars(required, schema_with = "crate::modes::nullable_enum::<Access>")]
    pub access: Option<Access>,
    /// The row's content; `null` for none, and in an index written before it.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::modes::nullable_enum::<Content>")]
    pub content: Option<Content>,
    /// The handler the decoder job ran (`modes_list`'s `handlers`); `null` for a job of
    /// no row, and in an index written before handlers.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub handler: Option<String>,
    pub created_ns: Timestamp,
    /// When a packet was last written to it (its start until the first); the index is
    /// rewritten at most once a second while it is written, so this lags by up to that.
    /// What `keep_days` ages it by.
    #[serde(default = "unknown_last")]
    pub last_ns: Timestamp,
    pub bytes: u64,
    pub packets: u64,
    /// A job in this node is appending to it.
    pub writing: bool,
    #[serde(skip)]
    #[schemars(skip)]
    pub path: PathBuf,
}

/// What a writer knows of a capture before it is stored.
#[derive(Clone, Debug, PartialEq)]
pub struct NewPacketCapture {
    pub job: JobId,
    pub source: SourceId,
    pub center_hz: Hz,
    pub link_type: LinkType,
    pub mode: Option<String>,
    pub access: Option<Access>,
    pub content: Option<Content>,
    /// The handler the decoder job runs; `None` for a job of no row.
    pub handler: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PacketStoreError {
    #[error("unknown packet capture {0}")]
    Unknown(String),
    #[error("packet capture {id} is being written by job {}", job.0)]
    Writing { id: String, job: JobId },
    #[error("io: {0}")]
    Io(String),
    #[error("{READ_ONLY}")]
    ReadOnly,
}

/// The packet store's policy beside the burst store's `burst`: its own byte budget,
/// [`PACKET_CAPTURE_KEEP_BYTES`], with `burst`'s `keep_days` and `expire_at_open`.
pub fn packet_policy(burst: BurstPolicy) -> BurstPolicy {
    BurstPolicy {
        keep_bytes: PACKET_CAPTURE_KEEP_BYTES,
        ..burst
    }
}

/// `last_ns` of a record from an index written before it: filled at open ([`fill_last`]).
fn unknown_last() -> Timestamp {
    Timestamp(0)
}

/// A record read with no `last_ns` takes its file's modification time, else its
/// `created_ns` (never earlier than that).
fn fill_last(r: &mut PacketCaptureRecord, meta: Option<&std::fs::Metadata>) {
    if r.last_ns.0 != 0 {
        return;
    }
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64);
    r.last_ns = Timestamp(mtime.unwrap_or(0).max(r.created_ns.0));
}

/// Why a read-only store refuses a change.
const READ_ONLY: &str = "the packet store is read-only: another process owns the state directory";

/// The store: the directory and its index, every change written through.
pub struct PacketStore {
    dir: PathBuf,
    policy: BurstPolicy,
    max_bytes: u64,
    index: Mutex<Vec<PacketCaptureRecord>>,
    /// Opened by [`PacketStore::open_read_only`]: nothing is written.
    read_only: bool,
}

impl PacketStore {
    /// Open (making it if missing) `state_dir/packets`, reading `index.json`. A record
    /// whose file is gone is dropped; every other is no longer being written, and its
    /// file is cut back to whole blocks when its length disagrees with the record. A
    /// file the index does not know is left alone. Nothing is expired here.
    pub fn open(state_dir: &Path, policy: BurstPolicy) -> std::io::Result<Arc<Self>> {
        Self::open_with_cap(state_dir, policy, PACKET_CAPTURE_MAX_BYTES)
    }

    /// [`open`](Self::open) with a file cap of `max_bytes` (tests).
    pub fn open_with_cap(
        state_dir: &Path,
        policy: BurstPolicy,
        max_bytes: u64,
    ) -> std::io::Result<Arc<Self>> {
        let dir = state_dir.join(PACKETS_DIR);
        std::fs::create_dir_all(&dir)?;
        let index_path = dir.join(INDEX_FILE);
        let read: Vec<PacketCaptureRecord> = match std::fs::read(&index_path) {
            Err(_) => Vec::new(),
            Ok(b) => match serde_json::from_slice(&b) {
                Ok(v) => v,
                Err(e) => {
                    let aside = dir.join(format!("{INDEX_FILE}.broken-{}", ulid::Ulid::generate()));
                    eprintln!("zeta: packets: index unreadable ({e}); moved aside");
                    if let Err(e) = std::fs::rename(&index_path, aside) {
                        eprintln!("zeta: packets: index not moved aside ({e})");
                    }
                    Vec::new()
                }
            },
        };
        let mut changed = false;
        let mut kept = Vec::with_capacity(read.len());
        for mut r in read {
            r.path = dir.join(format!("{}.pcapng", r.id));
            let Ok(meta) = std::fs::metadata(&r.path) else {
                changed = true;
                continue;
            };
            if r.last_ns.0 == 0 {
                fill_last(&mut r, Some(&meta));
                changed = true;
            }
            if r.writing {
                r.writing = false;
                changed = true;
            }
            if meta.len() != r.bytes {
                if let Err(e) = trim(&mut r) {
                    eprintln!(
                        "zeta: packets: capture {} unreadable ({e}); dropped from the index",
                        r.id
                    );
                    changed = true;
                    continue;
                }
                changed = true;
            }
            kept.push(r);
        }
        let store = Self {
            dir,
            policy,
            max_bytes,
            index: Mutex::new(kept),
            read_only: false,
        };
        if changed {
            store.persist(&store.index.lock().unwrap())?;
        }
        Ok(Arc::new(store))
    }

    /// Open `state_dir/packets` read-only, for a process that does not own the state
    /// directory while another (`zeta serve`) writes it: the index is read as it is, a
    /// capture being written stays `writing`, and no file, index or directory is made,
    /// trimmed or rewritten. A missing or unreadable index reads as empty. `begin`,
    /// `delete` and `expire` change nothing ([`PacketStoreError::ReadOnly`]).
    pub fn open_read_only(state_dir: &Path, policy: BurstPolicy) -> Arc<Self> {
        let dir = state_dir.join(PACKETS_DIR);
        let mut index: Vec<PacketCaptureRecord> = match std::fs::read(dir.join(INDEX_FILE)) {
            Err(_) => Vec::new(),
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                eprintln!("zeta: packets: index unreadable ({e}); listed as empty");
                Vec::new()
            }),
        };
        for r in &mut index {
            r.path = dir.join(format!("{}.pcapng", r.id));
            let meta = std::fs::metadata(&r.path).ok();
            fill_last(r, meta.as_ref());
        }
        Arc::new(Self {
            dir,
            policy,
            max_bytes: PACKET_CAPTURE_MAX_BYTES,
            index: Mutex::new(index),
            read_only: true,
        })
    }

    /// Start a capture file for `new`, with its header written, and the writer that
    /// appends to it. Each packet's comment is `job=<id> handler=<id> source=<id>
    /// freq_hz=<hz>`, without `handler=` for a job of no handler.
    pub fn begin(self: &Arc<Self>, new: NewPacketCapture) -> std::io::Result<PacketWriter> {
        let (id, file, bytes) = self.start(&new)?;
        let handler = new
            .handler
            .as_deref()
            .map_or_else(String::new, |h| format!(" handler={h}"));
        let comment = format!(
            "job={}{handler} source={} freq_hz={}",
            new.job.0, new.source.0, new.center_hz.0
        );
        Ok(PacketWriter {
            store: self.clone(),
            new,
            comment,
            id,
            file,
            bytes,
            packets: 0,
            last_index: Instant::now(),
            scratch: Vec::new(),
            poisoned: false,
        })
    }

    /// Make a record and its file (header only), add it to the index and expire.
    fn start(&self, new: &NewPacketCapture) -> std::io::Result<(String, std::fs::File, u64)> {
        if self.read_only {
            return Err(std::io::Error::other(READ_ONLY));
        }
        let id = ulid::Ulid::generate().to_string();
        let path = self.dir.join(format!("{id}.pcapng"));
        let mut head = Vec::new();
        pcapng::header(&mut head, new.link_type);
        let mut file = std::fs::File::create(&path)?;
        file.write_all(&head)?;
        let record = PacketCaptureRecord {
            id: id.clone(),
            job: new.job.clone(),
            source: new.source.clone(),
            center_hz: new.center_hz,
            link_type: new.link_type,
            mode: new.mode.clone(),
            access: new.access,
            content: new.content,
            handler: new.handler.clone(),
            created_ns: Timestamp(now_ns()),
            last_ns: Timestamp(now_ns()),
            bytes: head.len() as u64,
            packets: 0,
            writing: true,
            path: path.clone(),
        };
        let mut index = self.index.lock().unwrap();
        index.push(record);
        if let Err(e) = self.persist(&index) {
            index.pop();
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        drop(index);
        self.expire();
        Ok((id, file, head.len() as u64))
    }

    /// The store's directory, `state_dir/packets`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every record, newest first.
    pub fn list(&self) -> Vec<PacketCaptureRecord> {
        let mut v = self.index.lock().unwrap().clone();
        v.sort_by(|a, b| (b.created_ns.0, &b.id).cmp(&(a.created_ns.0, &a.id)));
        v
    }

    /// The record `id`.
    pub fn get(&self, id: &str) -> Option<PacketCaptureRecord> {
        self.index
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.id == id)
            .cloned()
    }

    /// Remove the capture `id` and its file; one being written is refused.
    pub fn delete(&self, id: &str) -> Result<PacketCaptureRecord, PacketStoreError> {
        if self.read_only {
            return Err(PacketStoreError::ReadOnly);
        }
        let mut index = self.index.lock().unwrap();
        let i = index
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| PacketStoreError::Unknown(id.into()))?;
        if index[i].writing {
            return Err(PacketStoreError::Writing {
                id: id.into(),
                job: index[i].job.clone(),
            });
        }
        let rec = index.remove(i);
        if let Err(e) = self.persist(&index) {
            index.insert(i, rec);
            return Err(PacketStoreError::Io(e.to_string()));
        }
        let _ = std::fs::remove_file(&rec.path);
        Ok(rec)
    }

    /// Apply the policy: the closed captures written longest ago go until all the bytes
    /// held fit `keep_bytes`, then every closed capture not written to for `keep_days`
    /// (aged by `last_ns`, so a capture that ran for days is kept from its last packet,
    /// not its start). A capture being written stays (and counts against the budget).
    /// Returns the ids removed; a read-only store removes none.
    pub fn expire(&self) -> Vec<String> {
        if self.read_only {
            return Vec::new();
        }
        let mut index = self.index.lock().unwrap();
        index.sort_by(|a, b| (a.last_ns.0, &a.id).cmp(&(b.last_ns.0, &b.id)));
        let mut total: u64 = index.iter().map(|r| r.bytes).sum();
        let mut gone = vec![false; index.len()];
        for (i, r) in index.iter().enumerate() {
            if total <= self.policy.keep_bytes {
                break;
            }
            if !r.writing {
                gone[i] = true;
                total -= r.bytes;
            }
        }
        let cutoff =
            now_ns().saturating_sub(u64::from(self.policy.keep_days).saturating_mul(NS_PER_DAY));
        for (i, r) in index.iter().enumerate() {
            if !r.writing && r.last_ns.0 < cutoff {
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
        let removed: Vec<PacketCaptureRecord> = removed.into_iter().map(|(r, _)| r).collect();
        let kept: Vec<PacketCaptureRecord> = kept.into_iter().map(|(r, _)| r).collect();
        if self.persist(&kept).is_err() {
            *index = kept.into_iter().chain(removed).collect();
            return Vec::new();
        }
        *index = kept;
        removed
            .iter()
            .map(|r| {
                let _ = std::fs::remove_file(&r.path);
                r.id.clone()
            })
            .collect()
    }

    /// Set the size of the record `id`, and its last write to now when `wrote` (a packet
    /// was just appended); the index is rewritten when `persist`.
    fn update(
        &self,
        id: &str,
        bytes: u64,
        packets: u64,
        writing: bool,
        wrote: bool,
        persist: bool,
    ) {
        let mut index = self.index.lock().unwrap();
        if let Some(r) = index.iter_mut().find(|r| r.id == id) {
            r.bytes = bytes;
            r.packets = packets;
            r.writing = writing;
            if wrote {
                r.last_ns = Timestamp(now_ns());
            }
        }
        if persist {
            let _ = self.persist(&index);
        }
    }

    /// Write `index` to `index.json` atomically (a temp file, then a rename).
    fn persist(&self, index: &[PacketCaptureRecord]) -> std::io::Result<()> {
        if self.read_only {
            return Err(std::io::Error::other(READ_ONLY));
        }
        let json = serde_json::to_vec(index)?;
        let tmp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, self.dir.join(INDEX_FILE))
    }
}

/// Cut `r`'s file back to whole blocks and recount its size and packets.
fn trim(r: &mut PacketCaptureRecord) -> std::io::Result<()> {
    let bytes = std::fs::read(&r.path)?;
    let whole = pcapng::whole_blocks(&bytes);
    if whole < bytes.len() {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&r.path)?
            .set_len(whole as u64)?;
    }
    r.bytes = whole as u64;
    r.packets = pcapng::read(&bytes[..whole]).map_or(0, |(_, p)| p.len() as u64);
    Ok(())
}

/// Appends packets to one capture file, rolling to a fresh one at the cap. Marks its
/// record closed when dropped.
pub struct PacketWriter {
    store: Arc<PacketStore>,
    new: NewPacketCapture,
    comment: String,
    id: String,
    file: std::fs::File,
    bytes: u64,
    packets: u64,
    last_index: Instant,
    scratch: Vec<u8>,
    /// A failed write could not be undone: the file may hold a torn block.
    poisoned: bool,
}

impl PacketWriter {
    /// The file now being written.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Append one packet at `t_ns`. When it would take the file past the cap (and the
    /// file holds a packet already), the file is closed and a fresh one begun first:
    /// `Some(new id)` then. A packet larger than the cap is written whole in a file of
    /// its own.
    pub fn write(&mut self, t_ns: u64, data: &[u8]) -> std::io::Result<Option<String>> {
        if self.poisoned {
            return Err(std::io::Error::other(
                "packet capture file left torn by an earlier failed write",
            ));
        }
        self.scratch.clear();
        pcapng::packet(&mut self.scratch, t_ns, data, &self.comment);
        let mut rolled = None;
        if self.packets > 0 && self.bytes + self.scratch.len() as u64 > self.store.max_bytes {
            let (id, file, bytes) = self.store.start(&self.new)?;
            self.store
                .update(&self.id, self.bytes, self.packets, false, false, true);
            self.id = id.clone();
            self.file = file;
            self.bytes = bytes;
            self.packets = 0;
            self.last_index = Instant::now();
            rolled = Some(id);
        }
        if let Err(e) = self.file.write_all(&self.scratch) {
            self.rewind();
            return Err(e);
        }
        self.bytes += self.scratch.len() as u64;
        self.packets += 1;
        let due = self.last_index.elapsed() >= INDEX_EVERY;
        if due {
            self.last_index = Instant::now();
        }
        self.store
            .update(&self.id, self.bytes, self.packets, true, true, due);
        Ok(rolled)
    }
}

impl PacketWriter {
    /// Cut the file back to the last whole block after a failed write; when that
    /// fails too the writer is poisoned.
    fn rewind(&mut self) {
        use std::io::{Seek, SeekFrom};
        let undone = self
            .file
            .set_len(self.bytes)
            .and_then(|()| self.file.seek(SeekFrom::Start(self.bytes)).map(|_| ()));
        if undone.is_err() {
            self.poisoned = true;
        }
    }
}

impl Drop for PacketWriter {
    fn drop(&mut self) {
        self.store
            .update(&self.id, self.bytes, self.packets, false, false, true);
    }
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_capture() -> NewPacketCapture {
        NewPacketCapture {
            job: JobId("j1".into()),
            source: SourceId("s1".into()),
            center_hz: Hz(144_800_000),
            link_type: LinkType::Ax25Kiss,
            mode: Some("aprs".into()),
            access: None,
            content: None,
            handler: None,
        }
    }

    fn open(dir: &Path, cap: u64) -> Arc<PacketStore> {
        PacketStore::open_with_cap(dir, BurstPolicy::default(), cap).unwrap()
    }

    fn read_file(r: &PacketCaptureRecord) -> Vec<pcapng::Packet> {
        pcapng::read(&std::fs::read(&r.path).unwrap()).unwrap().1
    }

    #[test]
    fn a_written_capture_is_listed_and_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut w = store.begin(new_capture()).unwrap();
        for i in 0..3u8 {
            assert_eq!(w.write(1_000 + u64::from(i), &[i; 20]).unwrap(), None);
        }
        let r = store.list().remove(0);
        assert_eq!((r.packets, r.writing), (3, true));
        assert_eq!(r.bytes, std::fs::metadata(&r.path).unwrap().len());
        let packets = read_file(&r);
        assert_eq!(packets.len(), 3);
        for p in &packets {
            assert_eq!(
                p.comment.as_deref(),
                Some("job=j1 source=s1 freq_hz=144800000")
            );
        }
        drop(w);
        assert!(!store.get(&r.id).unwrap().writing);
        // A job's handler is in its record and after the job in each comment.
        let with = NewPacketCapture {
            handler: Some("alt".into()),
            ..new_capture()
        };
        let mut w = store.begin(with).unwrap();
        w.write(2_000, &[7; 20]).unwrap();
        let r = store.list().remove(0);
        assert_eq!(r.handler.as_deref(), Some("alt"));
        assert_eq!(
            read_file(&r)[0].comment.as_deref(),
            Some("job=j1 handler=alt source=s1 freq_hz=144800000")
        );
    }

    #[test]
    fn a_captures_content_is_in_its_record_and_an_old_index_reads_it_as_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let with = NewPacketCapture {
            content: Some(Content::NetworkTraffic),
            ..new_capture()
        };
        drop(store.begin(with).unwrap());
        let r = store.list().remove(0);
        assert_eq!(r.content, Some(Content::NetworkTraffic));
        let wire = serde_json::to_value(&r).unwrap();
        assert_eq!(wire["content"], "network_traffic");
        // The same record in an index written before `content`.
        drop(store);
        let index = tmp.path().join("packets").join(INDEX_FILE);
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
        v[0].as_object_mut().unwrap().remove("content");
        std::fs::write(&index, v.to_string()).unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        assert_eq!(store.list().remove(0).content, None);
    }

    #[test]
    fn a_full_capture_rolls_to_a_fresh_id() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), 4096);
        let mut w = store.begin(new_capture()).unwrap();
        let mut rolls = 0;
        for i in 0..100u64 {
            let before = w.id().to_string();
            match w.write(i, &[7; 200]).unwrap() {
                Some(id) => {
                    rolls += 1;
                    assert_ne!(id, before);
                    assert_eq!(w.id(), id);
                }
                None => assert_eq!(w.id(), before),
            }
        }
        drop(w);
        let all = store.list();
        assert_eq!(all.len(), rolls + 1);
        assert!(all.len() > 1);
        let mut total = 0;
        for r in &all {
            assert!(std::fs::metadata(&r.path).unwrap().len() <= 4096);
            total += read_file(r).len();
            assert!(!r.writing);
        }
        assert_eq!(total, 100);
        assert_eq!(all.iter().map(|r| r.packets).sum::<u64>(), 100);
    }

    #[test]
    fn a_packet_larger_than_the_cap_is_still_written_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), 4096);
        let mut w = store.begin(new_capture()).unwrap();
        w.write(1, &[1; 50]).unwrap();
        let first = w.id().to_string();
        let rolled = w.write(2, &vec![9; 8000]).unwrap();
        assert!(rolled.is_some());
        drop(w);
        let big = store.get(rolled.as_deref().unwrap()).unwrap();
        assert_eq!(big.packets, 1);
        assert_eq!(read_file(&big)[0].data.len(), 8000);
        assert_eq!(store.get(&first).unwrap().packets, 1);
    }

    #[test]
    fn open_trims_a_capture_cut_inside_a_block() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut w = store.begin(new_capture()).unwrap();
        for i in 0..3u8 {
            w.write(u64::from(i), &[i; 30]).unwrap();
        }
        let id = w.id().to_string();
        std::mem::forget(w);
        let path = store.get(&id).unwrap().path;
        let len = std::fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(len - 5)
            .unwrap();
        drop(store);
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let r = store.get(&id).unwrap();
        assert!(!r.writing);
        assert_eq!(r.packets, 2);
        assert_eq!(r.bytes, std::fs::metadata(&path).unwrap().len());
        assert_eq!(read_file(&r).len(), 2);
    }

    /// A one-shot beside a live writer: the store it opens read-only lists the capture as
    /// being written and touches neither the index nor the file, though the file has grown
    /// past what the (throttled) index says; it begins, deletes and expires nothing.
    #[test]
    fn a_read_only_open_beside_a_live_writer_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut w = store.begin(new_capture()).unwrap();
        // Within the index's throttle: the file grows, the index on disk does not.
        for i in 0..3u8 {
            w.write(u64::from(i), &[i; 30]).unwrap();
        }
        let id = w.id().to_string();
        let path = store.get(&id).unwrap().path;
        let index_path = tmp.path().join(PACKETS_DIR).join(INDEX_FILE);
        let (index_before, file_before) = (
            std::fs::read(&index_path).unwrap(),
            std::fs::read(&path).unwrap(),
        );
        let on_disk: Vec<PacketCaptureRecord> = serde_json::from_slice(&index_before).unwrap();
        assert!(on_disk[0].writing);
        assert!(
            on_disk[0].bytes < file_before.len() as u64,
            "the index lags"
        );

        let ro = PacketStore::open_read_only(tmp.path(), BurstPolicy::default());
        let r = ro.get(&id).unwrap();
        assert!(r.writing);
        assert_eq!(r.bytes, on_disk[0].bytes);
        assert_eq!(r.path, path);
        assert!(ro.begin(new_capture()).is_err());
        assert_eq!(ro.delete(&id), Err(PacketStoreError::ReadOnly));
        assert!(ro.expire().is_empty());
        drop(ro);
        assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
        assert_eq!(std::fs::read(&path).unwrap(), file_before);
        let files = std::fs::read_dir(tmp.path().join(PACKETS_DIR))
            .unwrap()
            .count();
        assert_eq!(files, 2, "the index and the one capture, nothing new");
        // The writer goes on as before.
        w.write(9, &[9; 30]).unwrap();
        assert_eq!(store.get(&id).unwrap().packets, 4);
    }

    #[test]
    fn a_read_only_open_of_no_store_makes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let ro = PacketStore::open_read_only(tmp.path(), BurstPolicy::default());
        assert!(ro.list().is_empty());
        assert!(!tmp.path().join(PACKETS_DIR).exists());
    }

    #[test]
    fn open_drops_a_record_whose_file_is_gone_and_adopts_none_it_does_not_know() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut w = store.begin(new_capture()).unwrap();
        w.write(1, &[1; 10]).unwrap();
        let id = w.id().to_string();
        drop(w);
        let path = store.get(&id).unwrap().path;
        std::fs::remove_file(&path).unwrap();
        let stray = tmp.path().join(PACKETS_DIR).join("stray.pcapng");
        std::fs::write(&stray, b"x").unwrap();
        drop(store);
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        assert!(store.list().is_empty());
        assert!(stray.exists());
    }

    #[test]
    fn deleting_a_capture_being_written_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut w = store.begin(new_capture()).unwrap();
        w.write(1, &[1; 10]).unwrap();
        let id = w.id().to_string();
        assert_eq!(
            store.delete(&id),
            Err(PacketStoreError::Writing {
                id: id.clone(),
                job: JobId("j1".into())
            })
        );
        assert!(store.get(&id).unwrap().path.is_file());
        drop(w);
        assert!(store.delete(&id).is_ok());
        assert!(store.get(&id).is_none());
    }

    #[test]
    fn expiry_takes_the_oldest_closed_captures_and_never_one_being_written() {
        let tmp = tempfile::tempdir().unwrap();
        let one = {
            // The size of one capture of a single 100-byte packet.
            let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
            let mut w = store.begin(new_capture()).unwrap();
            w.write(1, &[1; 100]).unwrap();
            let id = w.id().to_string();
            drop(w);
            let bytes = store.get(&id).unwrap().bytes;
            store.delete(&id).unwrap();
            bytes
        };
        let policy = BurstPolicy {
            keep_bytes: one * 2 + one / 2,
            ..BurstPolicy::default()
        };
        let store =
            PacketStore::open_with_cap(tmp.path(), policy, PACKET_CAPTURE_MAX_BYTES).unwrap();
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut w = store.begin(new_capture()).unwrap();
            w.write(1, &[1; 100]).unwrap();
            ids.push(w.id().to_string());
        }
        // Two closed captures, then one open: together over the budget.
        let mut open_w = store.begin(new_capture()).unwrap();
        open_w.write(1, &[1; 100]).unwrap();
        assert_eq!(store.expire(), vec![ids[0].clone()]);
        let live: Vec<String> = store.list().into_iter().map(|r| r.id).collect();
        assert!(live.contains(&ids[1]));
        assert!(live.contains(&open_w.id().to_string()));
    }

    /// Set the record `id`'s start and last write `created_days` and `last_days` ago.
    fn age(store: &PacketStore, id: &str, created_days: u64, last_days: u64) {
        let now = now_ns();
        let mut index = store.index.lock().unwrap();
        let r = index.iter_mut().find(|r| r.id == id).unwrap();
        r.created_ns = Timestamp(now - created_days * NS_PER_DAY);
        r.last_ns = Timestamp(now - last_days * NS_PER_DAY);
    }

    /// `keep_days` ages a capture by its last write: one closed and not written to for
    /// longer goes; one begun long ago but written to lately stays; one being written
    /// stays however old.
    #[test]
    fn keep_days_ages_a_capture_by_its_last_write() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let keep = u64::from(BurstPolicy::default().keep_days);
        let closed = || {
            let mut w = store.begin(new_capture()).unwrap();
            w.write(1, &[1; 10]).unwrap();
            w.id().to_string()
        };
        let (stale, ran_for_days) = (closed(), closed());
        let mut live = store.begin(new_capture()).unwrap();
        live.write(1, &[1; 10]).unwrap();
        let live_id = live.id().to_string();
        // Aged once all three are begun (each begin expires the store).
        age(&store, &stale, keep + 30, keep + 1);
        age(&store, &ran_for_days, keep + 30, 0);
        age(&store, &live_id, keep + 30, keep + 30);
        assert_eq!(store.expire(), vec![stale.clone()]);
        assert!(store.get(&stale).is_none());
        assert!(
            !tmp.path()
                .join(PACKETS_DIR)
                .join(format!("{stale}.pcapng"))
                .exists()
        );
        assert!(store.get(&ran_for_days).is_some());
        assert!(store.get(&live_id).is_some());
    }

    #[test]
    fn a_write_moves_the_last_write_on() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut w = store.begin(new_capture()).unwrap();
        let id = w.id().to_string();
        age(&store, &id, 3, 3);
        let before = store.get(&id).unwrap().last_ns;
        w.write(1, &[1; 10]).unwrap();
        let r = store.get(&id).unwrap();
        assert!(r.last_ns.0 > before.0);
        assert!(r.last_ns.0 > r.created_ns.0);
        // Closing it is no write.
        drop(w);
        assert_eq!(store.get(&id).unwrap().last_ns, r.last_ns);
    }

    /// An index written before `last_ns` loads, each record taking its file's
    /// modification time.
    #[test]
    fn a_record_without_a_last_write_takes_its_files_time() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut w = store.begin(new_capture()).unwrap();
        w.write(1, &[1; 10]).unwrap();
        let id = w.id().to_string();
        drop(w);
        age(&store, &id, 20, 20);
        store.persist(&store.index.lock().unwrap()).unwrap();
        drop(store);
        let index_path = tmp.path().join(PACKETS_DIR).join(INDEX_FILE);
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
        v[0].as_object_mut().unwrap().remove("last_ns");
        std::fs::write(&index_path, v.to_string()).unwrap();
        let ro = PacketStore::open_read_only(tmp.path(), BurstPolicy::default());
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let day = now_ns() - NS_PER_DAY;
        for r in [ro.get(&id).unwrap(), store.get(&id).unwrap()] {
            assert!(r.last_ns.0 > day, "the file was written just now: {r:?}");
        }
    }

    /// With the node's defaults ([`packet_policy`] of the default burst policy) a capture
    /// that rolled once keeps both its files through the next capture's start: the budget
    /// is many roll sizes, not one. The full file is made sparse rather than written.
    #[test]
    fn a_capture_that_rolled_once_keeps_both_files_under_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = packet_policy(BurstPolicy::default());
        assert_eq!(policy.keep_bytes, PACKET_CAPTURE_KEEP_BYTES);
        assert_eq!(policy.keep_days, BurstPolicy::default().keep_days);
        let store = PacketStore::open(tmp.path(), policy).unwrap();
        let mut w = store.begin(new_capture()).unwrap();
        w.write(1, &[1; 10]).unwrap();
        let first = w.id().to_string();
        // The file all but full, as 256 MiB of packets leave it.
        let full = PACKET_CAPTURE_MAX_BYTES - 8;
        w.file.set_len(full).unwrap();
        {
            use std::io::{Seek, SeekFrom};
            w.file.seek(SeekFrom::Start(full)).unwrap();
        }
        w.bytes = full;
        let second = w.write(2, &[2; 100]).unwrap().expect("a roll");
        drop(w);
        // The next capture's start expires the store.
        let next = store.begin(new_capture()).unwrap();
        for id in [&first, &second, &next.id().to_string()] {
            assert!(store.get(id).is_some_and(|r| r.path.is_file()), "{id} kept");
        }
        assert!(store.get(&first).unwrap().bytes >= full);
    }

    #[test]
    fn open_survives_a_capture_file_it_cannot_read() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut w = store.begin(new_capture()).unwrap();
            w.write(1, &[1; 10]).unwrap();
            ids.push(w.id().to_string());
        }
        let bad = store.get(&ids[0]).unwrap().path;
        std::fs::remove_file(&bad).unwrap();
        std::fs::create_dir(&bad).unwrap();
        drop(store);
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let live: Vec<String> = store.list().into_iter().map(|r| r.id).collect();
        assert_eq!(live, vec![ids[1].clone()]);
    }

    #[test]
    fn a_torn_write_is_cut_back_and_a_failed_cut_poisons_the_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), PACKET_CAPTURE_MAX_BYTES);
        let mut w = store.begin(new_capture()).unwrap();
        w.write(1, &[1; 10]).unwrap();
        // A block cut short in the file, as a failed write_all leaves it.
        w.file.write_all(&[0xAB; 7]).unwrap();
        w.rewind();
        w.write(2, &[2; 10]).unwrap();
        let r = store.get(w.id()).unwrap();
        assert_eq!(read_file(&r).len(), 2);
        assert_eq!(r.bytes, std::fs::metadata(&r.path).unwrap().len());
        // A handle that cannot write nor truncate: the write fails, the cut fails.
        w.file = std::fs::File::open(&r.path).unwrap();
        assert!(w.write(3, &[3; 10]).is_err());
        assert!(w.write(4, &[4; 10]).is_err());
        assert_eq!(read_file(&store.get(w.id()).unwrap()).len(), 2);
        assert_eq!(store.get(w.id()).unwrap().packets, 2);
    }

    #[test]
    fn a_failed_roll_leaves_the_old_capture_marked_as_being_written() {
        let tmp = tempfile::tempdir().unwrap();
        let store = open(tmp.path(), 4096);
        let mut w = store.begin(new_capture()).unwrap();
        w.write(1, &[1; 3000]).unwrap();
        let id = w.id().to_string();
        std::fs::remove_dir_all(tmp.path().join(PACKETS_DIR)).unwrap();
        assert!(w.write(2, &[2; 3000]).is_err());
        assert_eq!(w.id(), id);
        assert!(store.get(&id).unwrap().writing);
        assert!(matches!(
            store.delete(&id),
            Err(PacketStoreError::Writing { .. })
        ));
    }
}
