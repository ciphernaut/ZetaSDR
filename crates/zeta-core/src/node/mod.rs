//! The node: owns ports, leases, rings and the stream bus, and runs graphs as
//! persistent jobs that resume after restart.

mod capture;
mod identify;
mod jsonlog;
mod launch;
pub use launch::DRAIN_TIMEOUT_MS;
mod origin;
pub use origin::{
    Agree, Confirmed, From as OriginFrom, FromKind, Grade, Heard, ORIGIN_MODULATION_FITS,
    ORIGIN_MODULATION_UNKNOWN, ORIGIN_NO_BAND_KINDS, ORIGIN_REACH_FLOOR, ORIGIN_REACH_IN,
    ORIGIN_REACH_UNKNOWN, ORIGIN_SITES, ORIGIN_SUMMARY_SITES, ORIGIN_WIDTH_FULL, ORIGIN_WIDTH_HALF,
    ORIGIN_WIDTH_LOOSE, ORIGIN_WIDTH_NONE, ORIGIN_WIDTH_RANGE, Origin, OriginAnswer, OriginQuery,
    STATION_ANONYMOUS, STATION_REPUBLISH_MS, StationSeen, Terms, Vantage, answer as origin_answer,
    apply_stations, confirm, confirm_identity, heard_of, identity_value, job_modulation, score_row,
    station_frame, station_summary, summary,
};
mod persist;
mod propose;
mod recover;
mod retune;
mod scan;
mod sources;
pub mod watch;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::OwnedMutexGuard;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::bank::{BankFactory, ChannelBank, PFB_MIN_RATE_HZ};
use crate::classify::{Classifier, ClassifierSet};
use crate::graph::{self, Graph, GraphError};
use crate::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use crate::knowledge::KnowledgeProvider;
use crate::lease::{Decision, LeaseError, LeaseManager};
use crate::modes::{MODES_CHECKED_FILE, ModeRegistry, ModesHandle};
use crate::ops::{
    FM_DEVIATION_DEFAULT_HZ, JobKind, Op, OpError, OpResult, Sideband, WFM_DEVIATION_DEFAULT_HZ,
};
use crate::port::{GainInfo, Port, PortError, Tuning};
use crate::ring::IqRing;
use crate::stage::{Backend, MixInput, StageRegistry};
use crate::stream::{Frame, Payload, StreamBus};
use crate::types::{
    Capabilities, Hz, JobId, PositionFrom, Provenance, SourceId, SourcePosition, Span, StreamId,
    StreamKind, Timestamp,
};

pub use crate::ops::OPS_RESULT_LIMIT_BYTES;

pub(crate) use launch::AfterGate;
use launch::{feed_bank, feed_ring};
pub(crate) use persist::now;
use persist::{
    SET_INSTANCES_DIR, io, load, load_instances, persist, persist_instance, remove_instance,
};
pub use sources::{GainSetting, SOURCE_RECORD_FORMAT, SourceRecord, load_source};
use sources::{load_sources, persist_source};

const BUS_CAPACITY: usize = 256;

/// Provenance source of the node's own streams (the `ops` stream).
pub const NODE_SOURCE: &str = "node";

fn node_provenance(span: Span) -> Provenance {
    Provenance {
        source: SourceId(NODE_SOURCE.into()),
        job: None,
        channel: None,
        span,
    }
}

/// Ring history in seconds for a source at `rate_hz` within `budget_bytes`
/// (`Complex<f32>`, 8 bytes per sample): `budget / (rate * 8)` clamped to
/// `1..=max_seconds`.
pub fn ring_seconds_for(budget_bytes: u64, rate_hz: u64, max_seconds: f64) -> f64 {
    let s = budget_bytes as f64 / (rate_hz.max(1) as f64 * 8.0);
    s.max(1.0).min(max_seconds)
}

pub struct NodeConfig {
    pub state_dir: PathBuf,
    /// Longest ring history in seconds; a fast source gets less, see [`ring_seconds_for`].
    pub ring_seconds: f64,
    /// Memory budget for one source's IQ ring (8 bytes per sample).
    pub ring_budget_bytes: u64,
    pub backends: Vec<Backend>,
    /// Relaunch `Running` jobs on `open` (a long-lived node). When false, records
    /// are loaded as persisted and nothing is launched, leased or rewritten.
    pub resume: bool,
    /// The default dwell of a `scan` step, ms: the time's worth of settled
    /// samples each step takes (not a time waited), unless `job_start` asks for
    /// another; [`crate::ops::SCAN_DWELL_MS`] (20), and the tests' too.
    pub scan_dwell_ms: u64,
    /// Makes the channel banks of wide sources; installed before any port is
    /// added, so resumed jobs get a bank as fresh ones do. `None`: no banks
    /// (until [`Node::set_bank_factory`]).
    pub bank_factory: Option<Arc<dyn BankFactory>>,
    /// The operator's mode rows (`--modes-dir`), read after the embedded ones (at `open`
    /// and by [`Node::reload_modes`]) and where an accepted signature's row is written;
    /// `None` is no operator directory: only the embedded rows, no row written (the CLI
    /// resolves the default, [`crate::modes::default_operator_dir`]). A missing
    /// directory reads as empty.
    pub modes_dir: Option<PathBuf>,
    /// The operator's named sets (`--sets-dir`), read after the embedded ones at `open`
    /// and validated against the mode rows; `None` is no operator directory: only the
    /// embedded sets (the CLI resolves the default, [`crate::sets::default_operator_dir`]).
    /// A missing directory reads as empty.
    pub sets_dir: Option<PathBuf>,
    /// What the burst store (`state_dir/bursts`) keeps; `BurstPolicy::default()` is
    /// 256 MiB and 7 days.
    pub burst_policy: crate::bursts::BurstPolicy,
    /// Where the operator said sources are (`--source-position`); a source that
    /// reports its own position (`Port::position`) overrides its entry.
    pub source_positions: BTreeMap<SourceId, SourcePosition>,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("unknown source {}", .0.0)]
    UnknownSource(SourceId),
    #[error("unknown job {}", .0.0)]
    UnknownJob(JobId),
    #[error("source {} is already attached", .0.0)]
    DuplicateSource(SourceId),
    #[error(transparent)]
    Graph(#[from] GraphError),
    #[error(transparent)]
    Lease(#[from] LeaseError),
    /// The source's port refused the tune a job's lease needed (a faulted device that
    /// cannot be opened, a band outside its range). Displayed under the source's name,
    /// as [`source_fault_reason`] words it, so a `job_start` against a faulted source
    /// reads the same as a job that failed on it.
    #[error("{}", source_fault_reason(.0, .1))]
    SourcePort(SourceId, PortError),
    #[error("io: {0}")]
    Io(String),
    /// An attached job's input job is not running in this node.
    #[error("input job {} not running", .0.0)]
    InputNotRunning(JobId),
    /// An attached job's input job runs but has no audio stream to read.
    #[error("input job {} has no audio stream", .0.0)]
    NoAudioStream(JobId),
    /// A built-in job's graph could not be re-expanded for its leased capture's rate.
    #[error("{0}")]
    Expand(OpError),
    /// An elastic job's relaunch found it stopped meanwhile; it stays stopped.
    #[error("job {} was stopped", .0.0)]
    Stopped(JobId),
    /// A request the node refuses, with the reason (a `replace` naming a job
    /// that does not hold the source).
    #[error("{0}")]
    Other(String),
}

/// How many stopped or failed job records the node keeps: when a job ends
/// and more than this many are terminal, the oldest beyond the cap are
/// forgotten and their files removed. Running jobs are never counted.
pub const JOB_HISTORY_MAX: usize = 20;

/// How long [`Node::replace_holders`] waits, holding the source's gate, for a
/// stopped job's lease to go.
const REPLACE_RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

struct JobEntry {
    record: JobRecord,
    /// The run this entry is for: each launch of the job (a start, a resume, a
    /// `graph_put` restart, an elastic relaunch) records a new one. Two launches of
    /// one id can overlap (two `graph_put`s at once: the second's stop finds the job
    /// already `Stopped` and returns while the first's old run is still ending), so
    /// a supervisor ends, releases and records only the run it was spawned for and
    /// never a later run that took the id meanwhile. 0 for a record loaded from disk
    /// and not launched.
    run: u64,
    /// When the record turned terminal in this process (its `created_ns` for
    /// one loaded terminal from disk); history is pruned oldest-ended first.
    ended_ns: Timestamp,
    cancel: Option<CancellationToken>,
    /// The supervisor; it releases the lease and unregisters the job's streams.
    done: Option<JoinHandle<()>>,
    /// Closes when run `run`'s supervisor has released the lease and unregistered
    /// the streams (or kept them for a relaunch), before it idles the port. A stop
    /// that finds `done` already taken by another stop waits on this instead, so
    /// every stop returns with the run released and a launch that follows never
    /// meets the old run's supervisor releasing the new lease (both are keyed by
    /// the job id). `None` for a record loaded from disk and not launched.
    ended: Option<tokio::sync::watch::Receiver<()>>,
    /// The job's output streams (those no edge of its graph consumes), in graph order
    /// (each node's in its stage's output order), set at launch; an attached job reads
    /// one of these.
    outputs: Vec<JobOutput>,
    /// Set at launch for a run nothing in which polls its port's fault: it has no IQ
    /// forwarder and no controller loop, and is not attached. A fault does not end it, so
    /// a recovery does not wait for it (see [`Inner::wait_ended`]).
    blind: bool,
    /// Set while the job is relaunched under its id, by [`Inner::relaunch_elastic`] (an
    /// elastic job on a new capture), by [`Inner::relaunch_in_place`] (a
    /// `job_retune`) or by [`Inner::hold_attached`] (an attached job whose input is
    /// retuned or restarted by `graph_put`): its supervisor then leaves the record
    /// `Running` and the lease held for the relaunch.
    relaunch: bool,
    /// Set by a stop under the source's gate ([`Inner::stop_gated`], a `replace`):
    /// the supervisor then does not idle the port, which would take that gate, and
    /// a launch of the job already past its lease (a `graph_put` restart) is
    /// refused, so the replaced job stays stopped (see [`Inner::record_running`]).
    gated_stop: bool,
    /// Set by the supervisor once the lease is released and the streams are
    /// unregistered, when all that is left is the port's idle.
    released: bool,
    /// Set by a `graph_put` before it stops the run to relaunch it under the same id:
    /// a controller (`watch`) then leaves what it started for the next run to re-adopt
    /// instead of stopping it as a `job_stop` does.
    restarting: bool,
    /// The job's fixed-station tally (design 4.2), counted by its rules tee, written into
    /// `context.stations` at the throttle and by the supervisor at the run's end. Seeded
    /// from the record's context, so a new entry (every launch) continues the counts.
    stations: origin::StationTally,
}

impl JobEntry {
    /// The entry of a record that is not running in this process (loaded from disk and
    /// not launched, or whose launch failed), ended at `ended_ns` for the history prune.
    fn loaded(record: JobRecord, ended_ns: Timestamp) -> JobEntry {
        JobEntry {
            ended_ns,
            stations: origin::StationTally::seeded(&record.context),
            record,
            run: 0,
            cancel: None,
            done: None,
            ended: None,
            outputs: Vec::new(),
            blind: false,
            relaunch: false,
            gated_stop: false,
            released: false,
            restarting: false,
        }
    }
}

/// A source's bank for the capture band it was made for; `bank` is `None`
/// when that band is narrower than [`PFB_MIN_RATE_HZ`] or the factory declined.
struct BankSlot {
    band: Tuning,
    bank: Option<Arc<dyn ChannelBank>>,
}

/// The node's three JSON-lines logs in the state directory.
#[derive(Clone, Copy)]
enum LogFile {
    Ops,
    Transcripts,
    Events,
}

impl LogFile {
    fn name(self) -> &'static str {
        match self {
            LogFile::Ops => "ops.log",
            LogFile::Transcripts => "transcripts.log",
            LogFile::Events => "events.log",
        }
    }
}

/// The last audio stream of `outputs` (a job's, in graph order): what is read of the job.
fn last_audio(outputs: &[JobOutput]) -> Option<StreamId> {
    outputs
        .iter()
        .rev()
        .find(|o| o.kind == StreamKind::Audio && o.role.is_none())
        .map(|o| o.id.clone())
}

/// An output stream of a job: its kind and the role its stage gave it, if any. A
/// composite (a video demodulator's) is audio only in kind, so `last_audio` skips it.
#[derive(Clone, Debug)]
pub(crate) struct JobOutput {
    pub kind: StreamKind,
    pub id: StreamId,
    pub role: Option<&'static str>,
}

impl JobOutput {
    pub(crate) fn new(kind: StreamKind, id: StreamId) -> Self {
        Self {
            kind,
            id,
            role: None,
        }
    }
}

/// Whether a mix may read `rec`: not an attached job (it is a chain over a job's audio),
/// a job a controller owns (a trial), another mix (a `mix` job, or any job whose graph
/// holds a `mix` node, as a `graph_start` of one does) or a job centred on a plan
/// channel that carries no voice (CB ch22 and ch23: data, its noise is no one talking).
fn mixable(rec: &JobRecord) -> bool {
    rec.input.is_none()
        && rec.owner.is_none()
        && crate::plans::no_voice_at(rec.tuning.center_hz.0).is_none()
        && rec.kind != JobKind::Mix.name()
        && !rec
            .graph
            .nodes
            .iter()
            .any(|n| n.kind == JobKind::Mix.name())
}

struct Inner {
    state_dir: PathBuf,
    /// The node's station, loaded at `open` and written through on every change.
    station: Mutex<Option<crate::station::Station>>,
    jobs_dir: PathBuf,
    /// `state_dir/sources`, one [`SourceRecord`] per source.
    sources_dir: PathBuf,
    /// The source records, loaded at `open` (those of absent sources included,
    /// kept untouched) and written through on every change.
    sources: Mutex<HashMap<SourceId, SourceRecord>>,
    ring_seconds: f64,
    ring_budget_bytes: u64,
    scan_dwell_ms: u64,
    /// How long a scan waits for a source to start its own sweep
    /// ([`crate::ops::SWEEP_START_TIMEOUT_MS`]; tests shorten it).
    sweep_start_timeout: Mutex<std::time::Duration>,
    /// Where the operator said sources are (`--source-position`).
    source_positions: BTreeMap<SourceId, SourcePosition>,
    backends: Vec<Backend>,
    registry: StageRegistry,
    /// The decoder modes, loaded at `open` against `registry` and replaced whole by
    /// [`Node::reload_modes`]; read through [`Inner::modes`] at each use.
    modes: ModesHandle,
    /// The operator's modes directory as configured (`NodeConfig.modes_dir`): what
    /// `reload_modes` reads and where an accepted signature's row is written (`None`:
    /// none, so only the embedded rows load and no row is written).
    modes_dir: Option<PathBuf>,
    /// Held across [`Node::reload_modes`] (no await inside).
    reloading: Mutex<()>,
    /// The named sets, loaded at `open` from `NodeConfig.sets_dir` over the embedded ones.
    sets: Arc<crate::sets::SetRegistry>,
    /// `state_dir/set-instances` ([`SET_INSTANCES_DIR`]): one [`crate::sets::SetInstance`]
    /// file per running set, apart from an operator `--sets-dir` of set files.
    instances_dir: PathBuf,
    /// The running sets' instances by set id, read back at `open` and written through by
    /// `set_start` and `set_stop`.
    set_instances: Mutex<BTreeMap<String, crate::sets::SetInstance>>,
    /// Held across `set_start` and `set_stop`, so two of them never update one instance
    /// at once. Taken before any source's gate, and no other operation takes it.
    set_gate: tokio::sync::Mutex<()>,
    /// The page themes: the built-in ones until the caller sets its own with `set_themes`.
    themes: RwLock<Arc<crate::themes::ThemeRegistry>>,
    /// The `capture` jobs' windows, opened (and expired) at `open`.
    bursts: Arc<crate::bursts::BurstStore>,
    /// The learned signatures, opened at `open`.
    signatures: Arc<crate::signatures::SignatureStore>,
    /// The exploration jobs' session records (see [`propose::record_session`]),
    /// swept at `open`, after a `capture_delete` and after every `capture` job's end.
    sessions: crate::sessions::SessionStore,
    /// Held across a job's proposal (see [`propose`]), so one job never proposes twice.
    proposing: Mutex<()>,
    leases: LeaseManager,
    bus: Arc<StreamBus>,
    ports: RwLock<HashMap<SourceId, Arc<dyn Port>>>,
    rings: Mutex<HashMap<SourceId, Arc<RwLock<IqRing>>>>,
    /// Per source, bumped by `feed_ring` after each block it pushes into the ring (see
    /// [`Inner::ring_tick`]).
    ring_ticks: Mutex<HashMap<SourceId, Arc<tokio::sync::watch::Sender<u64>>>>,
    bank_factory: RwLock<Option<Arc<dyn BankFactory>>>,
    banks: Mutex<HashMap<SourceId, BankSlot>>,
    jobs: Mutex<HashMap<JobId, JobEntry>>,
    knowledge: RwLock<Vec<Arc<dyn KnowledgeProvider>>>,
    classifiers: RwLock<ClassifierSet>,
    /// Failures to persist a job's natural end; memory and disk disagree for these.
    persist_errors: Mutex<Vec<String>>,
    /// Every applied op, one JSON line each; see [`jsonlog`].
    ops_log: jsonlog::JsonLog,
    /// Every `transcript` event of a `transcribe` job, one JSON line each; see
    /// [`Inner::tee_transcripts`].
    transcripts_log: jsonlog::JsonLog,
    /// Every event frame of every source's feed, one JSON line each; see
    /// [`Inner::tee_events`].
    events_log: jsonlog::JsonLog,
    /// Per [`LogFile`]: whether a failed write is already in `persist_errors`.
    log_failed: [std::sync::atomic::AtomicBool; 3],
    /// Running transcript tees (see [`Inner::tee_transcripts`]).
    tees: Arc<std::sync::atomic::AtomicUsize>,
    /// Running tasks of every job's stage set (its stages, its IQ forwarder, a
    /// scan's step loop and FFT task), so a run left behind by a launch shows (see
    /// `StageRunning`).
    stages: Arc<std::sync::atomic::AtomicUsize>,
    /// The last run handed out (see [`JobEntry::run`]).
    runs: std::sync::atomic::AtomicU64,
    /// Held across [`Inner::relaunch_elastic`], so two retunes never relaunch one
    /// elastic job at once.
    relaunching: tokio::sync::Mutex<()>,
    /// Held across [`Node::mix_start`], so two starts of one set make one mix.
    mix_gate: tokio::sync::Mutex<()>,
    /// Cancelled when the `Node` is dropped; every job token is a child of it.
    shutdown: CancellationToken,
    /// The verdicts of owned `identify` runs, by identify id, until their owner (a
    /// `watch`) takes them: an owned record is dropped as soon as it ends, context and
    /// all (see [`Inner::prune_history`]).
    verdicts: Mutex<HashMap<JobId, serde_json::Value>>,
    /// How owned `capture` runs ended (`{capture, failed}`, see [`capture::capture_finished`]),
    /// by capture id, until their watch takes them: as `verdicts`, the record goes at its end.
    captured: Mutex<HashMap<JobId, serde_json::Value>>,
    /// The open fault episode of each source whose port the node has seen faulted and
    /// not yet recovered (see [`recover::Episode`]). Locked after `jobs` when both are
    /// held, never the other way round.
    episodes: Mutex<HashMap<SourceId, recover::Episode>>,
}

impl Inner {
    /// A receiver that changes each time `feed_ring` has pushed a block of `source`
    /// into its ring, so a reader waits on the ring instead of polling it.
    pub(crate) fn ring_tick(&self, source: &SourceId) -> tokio::sync::watch::Receiver<u64> {
        self.ring_tick_tx(source).subscribe()
    }

    /// The sender behind [`Inner::ring_tick`], made on first use by either side.
    pub(crate) fn ring_tick_tx(&self, source: &SourceId) -> Arc<tokio::sync::watch::Sender<u64>> {
        self.ring_ticks
            .lock()
            .unwrap()
            .entry(source.clone())
            .or_insert_with(|| Arc::new(tokio::sync::watch::channel(0).0))
            .clone()
    }

    /// The decoder modes as they stand now: one registry, which a caller keeps for the
    /// whole of one use so a reload meanwhile never mixes two sets of rows.
    pub(crate) fn modes(&self) -> Arc<ModeRegistry> {
        self.modes.get().unwrap_or_default()
    }

    /// Where source `id` is: what its port reports itself (`from: status`), else the
    /// operator's `--source-position` entry (`from: configured`), else `None`. The one
    /// definition behind [`Node::source_position`] and the origin query's vantage.
    pub(crate) fn source_position(&self, id: &SourceId) -> Option<SourcePosition> {
        self.port(id).and_then(|p| p.position()).or_else(|| {
            self.source_positions.get(id).map(|p| SourcePosition {
                from: PositionFrom::Configured,
                ..*p
            })
        })
    }

    /// The kind a job recorded as `kind` was expanded from, when the node expands its
    /// graph: a built-in kind, or a `Mode` the registry has a row for. A record of any
    /// other kind (a [`Node::job_start`] caller's own name) keeps its graph as stored.
    pub(crate) fn expandable_kind(&self, kind: &str) -> Option<JobKind> {
        JobKind::from_name(kind)
            .filter(|k| !matches!(k, JobKind::Mode(id) if self.modes().get(&id.0).is_none()))
    }

    /// A mode row's proof (`mode`, on the band `tuning`) supersedes each accepted
    /// signature it overlaps: one whose band holds `tuning`'s centre
    /// ([`crate::signatures::SignatureRecord::band_holds`]) when `tuning`'s width is
    /// within [`SUPERSEDE_WIDTH_RANGE`] times the record's bandwidth. A `learned_*` row
    /// (generated from a signature) supersedes nothing. Each one superseded is marked so
    /// in the store at `now_ns` (its captures stay pinned) and named on stderr. A record
    /// a concurrent proof superseded first is left as that one marked it.
    pub(crate) fn supersede_by(&self, mode: &str, tuning: Tuning, now_ns: u64) {
        use crate::signatures::{LEARNED_ROW_PREFIX, SUPERSEDE_WIDTH_RANGE};
        if mode.starts_with(LEARNED_ROW_PREFIX) {
            return;
        }
        let (lo, hi) = SUPERSEDE_WIDTH_RANGE;
        let width = tuning.bandwidth_hz.0 as f64;
        for rec in self.signatures.accepted() {
            let theirs = rec.bandwidth_hz.0 as f64;
            if !rec.band_holds(tuning.center_hz) || width < lo * theirs || width > hi * theirs {
                continue;
            }
            match self.signatures.supersede(&rec.id, mode, now_ns) {
                Ok(_) => eprintln!(
                    "zeta: signature {} ({}) superseded by {mode}",
                    rec.id, rec.name
                ),
                // Superseded (or otherwise answered) meanwhile: nothing failed.
                Err(crate::signatures::StoreError::State { .. }) => {}
                Err(e) => self
                    .persist_errors
                    .lock()
                    .unwrap()
                    .push(format!("signatures: {}: supersede by {mode}: {e}", rec.id)),
            }
        }
    }

    /// Append `line` to the log `which`. A failure never fails anything; the
    /// first per log is recorded in `persist_errors` as `<name>: <e>`, later
    /// ones are dropped.
    fn log_line(&self, which: LogFile, line: &serde_json::Value) {
        let log = match which {
            LogFile::Ops => &self.ops_log,
            LogFile::Transcripts => &self.transcripts_log,
            LogFile::Events => &self.events_log,
        };
        if let Err(e) = log.append(line)
            && !self.log_failed[which as usize].swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            self.persist_errors
                .lock()
                .unwrap()
                .push(format!("{}: {e}", which.name()));
        }
    }

    /// Cache a worker port's capabilities and the link speed their figures were
    /// measured at ([`Port::record_to_cache`], read together) in its source
    /// record, so a later start with the device absent can still list the
    /// source and a worker is passed back what was measured, with the speed it
    /// checks them against. Runs at attach and after each heal, so a respawn
    /// that measured again is cached with its speed. A port that runs in the
    /// node (`Port::worker` is `None`) has nothing to cache and writes no
    /// record. A failed write goes to `persist_errors`; the memory record is
    /// then left as it was.
    pub(crate) fn note_capabilities(&self, port: &dyn Port) {
        if port.worker().is_none() {
            return;
        }
        let source = port.id();
        let (caps, speed) = port.record_to_cache();
        let mut sources = self.sources.lock().unwrap();
        let mut rec = sources
            .get(source)
            .cloned()
            .unwrap_or_else(SourceRecord::new);
        rec.format = SOURCE_RECORD_FORMAT;
        rec.capabilities = Some(caps);
        rec.measured_at_speed = speed;
        match persist_source(&self.sources_dir, source, &rec) {
            Ok(()) => {
                sources.insert(source.clone(), rec);
            }
            Err(e) => self
                .persist_errors
                .lock()
                .unwrap()
                .push(format!("source record {}: {e}", source.0)),
        }
    }

    /// See [`Node::job_stop`].
    async fn stop(&self, id: &JobId) -> Result<(), NodeError> {
        self.stop_with(id, false).await
    }

    /// Store `graph` as job `id`'s (`capture_rate_hz` as in [`Node::job_graph_set`]) and,
    /// when it `running`, restart it under the same id.
    pub(crate) async fn swap_graph(
        self: &Arc<Self>,
        id: &JobId,
        graph: Graph,
        capture_rate_hz: Option<Option<u64>>,
        running: bool,
    ) -> Result<(), NodeError> {
        // Before the stop: the jobs attached to it follow the restart (see
        // `Inner::relaunch_attached`), relaunched once it runs again or failed to.
        let held = match running {
            true => self.hold_attached(id),
            false => Vec::new(),
        };
        let rec = match self
            .store_graph(id, graph, capture_rate_hz, running.then_some(false))
            .await
        {
            Ok(rec) => rec,
            Err(e) => {
                self.relaunch_attached(held).await;
                return Err(e);
            }
        };
        if running {
            let r = self
                .launch_task(rec)
                .await
                .map_err(|e| NodeError::Other(format!("launch task: {e}")));
            self.relaunch_attached(held).await;
            r??;
        }
        Ok(())
    }

    /// The first half of [`Inner::swap_graph`]: stop the job when `stop` is `Some`
    /// (`Some(true)`: gated, see [`Inner::stop_gated`]; a controller's run tells this
    /// restart from a `job_stop` by a flag), persist `graph` as its own and
    /// return the record, for the caller to launch.
    pub(crate) async fn store_graph(
        self: &Arc<Self>,
        id: &JobId,
        graph: Graph,
        capture_rate_hz: Option<Option<u64>>,
        stop: Option<bool>,
    ) -> Result<JobRecord, NodeError> {
        if let Some(gated) = stop {
            if let Some(e) = self.jobs.lock().unwrap().get_mut(id) {
                e.restarting = true;
            }
            if let Err(e) = self.stop_with(id, gated).await {
                if let Some(e) = self.jobs.lock().unwrap().get_mut(id) {
                    e.restarting = false;
                }
                return Err(e);
            }
        }
        let mut jobs = self.jobs.lock().unwrap();
        let entry = jobs
            .get_mut(id)
            .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
        let rec = JobRecord {
            graph,
            capture_rate_hz: capture_rate_hz.flatten(),
            operator_graph: capture_rate_hz.is_none(),
            ..entry.record.clone()
        };
        persist(&self.jobs_dir, &rec)?;
        entry.record = rec.clone();
        // The gated stop was this restart's own, not a `replace`'s: the launch that
        // follows must not be refused as undoing it (see `record_running`).
        if stop == Some(true) {
            entry.gated_stop = false;
        }
        Ok(rec)
    }

    /// [`Inner::stop`] while the caller holds the job's source gate: the port
    /// is not idled (the caller's request is about to tune it, and the idle
    /// would wait on that gate), and a supervisor already past its cleanup and
    /// waiting on the gate to idle is not awaited. Either way the job's lease is
    /// released and its streams unregistered when this returns, unless another
    /// stop took the supervisor first (see [`Node::replace_holders`]).
    async fn stop_gated(&self, id: &JobId) -> Result<(), NodeError> {
        self.stop_with(id, true).await
    }

    async fn stop_with(&self, id: &JobId, gated: bool) -> Result<(), NodeError> {
        let done = {
            let mut jobs = self.jobs.lock().unwrap();
            let entry = jobs
                .get_mut(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            if !matches!(
                entry.record.state,
                JobState::Failed { .. } | JobState::Stopped
            ) {
                let rec = JobRecord {
                    state: JobState::Stopped,
                    ..entry.record.clone()
                };
                persist(&self.jobs_dir, &rec)?;
                entry.record = rec;
                entry.ended_ns = now();
            }
            // Set before the cancel, under the jobs lock the supervisor reads it under.
            entry.gated_stop |= gated;
            if let Some(c) = entry.cancel.take() {
                c.cancel();
            }
            let done = entry.done.take();
            // A supervisor that already released may be waiting on the gate to idle.
            let done = done.filter(|_| !(gated && entry.released));
            let ended = entry.ended.clone();
            self.prune_history(&mut jobs);
            (done, ended)
        };
        if let Some(h) = done.0 {
            let _ = h.await;
        }
        // Another stop (or a relaunch) took the supervisor: wait for the run to
        // release all the same. Closed already once it has, so a gated stop never
        // waits here on a supervisor that waits on the gate to idle.
        if let Some(mut ended) = done.1 {
            let _ = ended.changed().await;
        }
        Ok(())
    }

    /// Forget the terminal records that ended earliest beyond
    /// [`JOB_HISTORY_MAX`], removing their files (best effort: a file that
    /// will not go is retried at the next prune). Called with `jobs` locked
    /// after a record turns terminal; that record ended last, so it survives.
    /// A record the heal of its source would relaunch (recoverable in the
    /// source's open fault episode) is never forgotten while the episode lasts:
    /// it still counts toward the cap, and older ones go in its place.
    fn prune_history(&self, jobs: &mut HashMap<JobId, JobEntry>) {
        let jobs_dir = &self.jobs_dir;
        let is_terminal =
            |e: &JobEntry| matches!(e.record.state, JobState::Stopped | JobState::Failed { .. });
        // An owned record (a watch's identify runs and their children) goes as soon as
        // it ends: it never takes one of the kept slots. Once its run has released (or
        // it was loaded, never launched), so its supervisor still finds the entry and
        // honours a gated stop (no idle under a gate the stopper holds).
        let owned: Vec<JobId> = jobs
            .values()
            .filter(|e| e.record.owner.is_some() && is_terminal(e))
            .filter(|e| e.cancel.is_none() && (e.released || e.run == 0))
            .map(|e| e.record.id.clone())
            .collect();
        for id in owned {
            jobs.remove(&id);
            let _ = std::fs::remove_file(jobs_dir.join(format!("{}.json", id.0)));
        }
        let terminal: Vec<&JobEntry> = jobs
            .values()
            .filter(|e| is_terminal(e) && e.record.owner.is_none())
            .collect();
        if terminal.len() <= JOB_HISTORY_MAX {
            return;
        }
        let over = terminal.len() - JOB_HISTORY_MAX;
        let mut candidates: Vec<(Timestamp, JobId)> = terminal
            .into_iter()
            .filter(|e| !self.keep_from_prune(jobs, e))
            .map(|e| (e.ended_ns, e.record.id.clone()))
            .collect();
        candidates.sort_by(|a, b| a.0.0.cmp(&b.0.0).then_with(|| a.1.0.cmp(&b.1.0)));
        for (_, id) in candidates.iter().take(over) {
            jobs.remove(id);
            let _ = std::fs::remove_file(jobs_dir.join(format!("{}.json", id.0)));
        }
    }

    /// Remove the session records none of whose captures is left in the burst store
    /// (see [`crate::sessions::SessionStore::sweep`]).
    fn sweep_sessions(&self) {
        let live = self.bursts.list().into_iter().map(|r| r.id).collect();
        self.sessions.sweep(&live);
    }

    /// See [`Node::job_context_merge`].
    fn context_merge(&self, id: &JobId, patch: serde_json::Value) -> Result<(), NodeError> {
        let mut jobs = self.jobs.lock().unwrap();
        let entry = jobs
            .get_mut(id)
            .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
        let ctx = &mut entry.record.context;
        match patch {
            serde_json::Value::Object(p) => {
                if !ctx.is_object() {
                    *ctx = serde_json::json!({});
                }
                let obj = ctx.as_object_mut().expect("context is an object");
                for (k, v) in p {
                    if v.is_null() {
                        obj.remove(&k);
                    } else {
                        obj.insert(k, v);
                    }
                }
            }
            other => *ctx = other,
        }
        persist(&self.jobs_dir, &entry.record)
    }

    /// The audio stream an attached job reads from `job`: the last audio output stream
    /// in its graph order (an audio edge between two of its stages does not count).
    /// `InputNotRunning` unless `job` runs in this node (a record loaded without resume
    /// does not), `NoAudioStream` when it outputs no audio.
    fn audio_stream(&self, job: &JobId) -> Result<StreamId, NodeError> {
        let jobs = self.jobs.lock().unwrap();
        let entry = jobs
            .get(job)
            .filter(|e| e.record.state == JobState::Running && e.cancel.is_some())
            .ok_or_else(|| NodeError::InputNotRunning(job.clone()))?;
        last_audio(&entry.outputs).ok_or_else(|| NodeError::NoAudioStream(job.clone()))
    }

    /// What a `mix` stage of the job `except` may read: the audio output of every job
    /// with a live run that is [`mixable`]. A run that ended is not listed, so the stage
    /// never holds a closed stream with no replacement.
    pub(super) fn mix_inputs(&self, except: &JobId) -> Vec<MixInput> {
        let jobs = self.jobs.lock().unwrap();
        jobs.values()
            .filter(|e| {
                e.record.state == JobState::Running
                    && e.cancel.is_some()
                    && mixable(&e.record)
                    && &e.record.id != except
            })
            .filter_map(|e| {
                Some(MixInput {
                    job: e.record.id.clone(),
                    stream: last_audio(&e.outputs)?,
                    source: e.record.source.clone(),
                    center_hz: e.record.tuning.center_hz.0,
                })
            })
            .collect()
    }

    /// The bin rate of `source`'s bank for the capture `band` (made now if
    /// the band is new), or `None` when that band gets no bank.
    fn bank_bin_rate(&self, source: &SourceId, band: Tuning) -> Option<u64> {
        self.sync_bank(source, band).map(|b| 2 * b.bin_hz())
    }

    /// `source`'s bank for the capture `band`: the kept one when it was made
    /// for that band, else a new one from the factory (none below
    /// [`PFB_MIN_RATE_HZ`]) replacing it. Without a factory nothing is kept.
    /// While a scan holds the source (a time-shared lease) the band changes
    /// every step and nothing can subscribe, so no bank is made or replaced:
    /// the first block after the scan makes the one for the band it left.
    fn sync_bank(&self, source: &SourceId, band: Tuning) -> Option<Arc<dyn ChannelBank>> {
        let mut banks = self.banks.lock().unwrap();
        if let Some(slot) = banks.get(source)
            && slot.band == band
        {
            return slot.bank.clone();
        }
        if self.leases.time_shared(source) {
            return None;
        }
        let factory = self.bank_factory.read().unwrap().clone()?;
        let bank = (band.bandwidth_hz.0 >= PFB_MIN_RATE_HZ)
            .then(|| factory.make(band.bandwidth_hz, band.center_hz))
            .flatten();
        banks.insert(
            source.clone(),
            BankSlot {
                band,
                bank: bank.clone(),
            },
        );
        bank
    }
}

/// The capture a source's resumable rigid records shared: the widest one's band
/// when it contains the others (the capture it tuned), else the band bounding them
/// all when the port can capture it (no wider than `max_bandwidth_hz`, its centre in
/// the tune range), else an elastic record's band that contains them all. `None`:
/// no rigid record, or no such band (each launch then tunes as before).
fn resume_capture(
    caps: &Capabilities,
    rigid: &[Tuning],
    elastic: impl IntoIterator<Item = Tuning>,
) -> Option<Tuning> {
    let edges = |t: &Tuning| {
        let half = t.bandwidth_hz.0 / 2;
        (
            t.center_hz.0.saturating_sub(half),
            t.center_hz.0.saturating_add(half),
        )
    };
    let contains = |outer: &Tuning| {
        let (ol, oh) = edges(outer);
        rigid.iter().all(|t| {
            let (l, h) = edges(t);
            l >= ol && h <= oh
        })
    };
    let widest = rigid.iter().max_by_key(|t| t.bandwidth_hz.0)?;
    if contains(widest) {
        return Some(*widest);
    }
    let lo = rigid.iter().map(|t| edges(t).0).min()?;
    let hi = rigid.iter().map(|t| edges(t).1).max()?;
    let bounding = Tuning {
        center_hz: Hz(lo + (hi - lo) / 2),
        bandwidth_hz: Hz(hi - lo),
    };
    let tunable =
        (caps.tune_min_hz.value().0..=caps.tune_max_hz.value().0).contains(&bounding.center_hz.0);
    if tunable && bounding.bandwidth_hz.0 <= caps.max_bandwidth_hz.value().0 {
        return Some(bounding);
    }
    elastic.into_iter().find(|t| contains(t))
}

/// Whether a job in `state` has ended: stopped or failed.
pub(crate) fn job_ended(state: &JobState) -> bool {
    matches!(state, JobState::Stopped | JobState::Failed { .. })
}

/// A job's failure caused by its source's port, `source <id>: <error>`: the reason every
/// fault-caused failure carries (a stage that saw the fault, a launch whose tune the port
/// refused), so the node's recovery of the source can find the jobs it should relaunch.
pub(crate) fn source_fault_reason(source: &SourceId, e: &dyn std::fmt::Display) -> String {
    format!("source {}: {e}", source.0)
}

/// A port's refusal of a gain as the node reports it: an `Unsupported` text
/// verbatim (it names the source and the range), an I/O failure as `Io`.
fn gain_error(e: PortError) -> NodeError {
    match e {
        PortError::Unsupported(m) => NodeError::Other(m),
        PortError::Io(m) => NodeError::Io(m),
        e => NodeError::Other(e.to_string()),
    }
}

/// Dropping the node cancels its tasks without touching persisted state, so
/// `Running` jobs resume on the next `open`.
pub struct Node {
    inner: Arc<Inner>,
    load_errors: Vec<String>,
    /// How many of `load_errors` (the first) are job-record files.
    job_load_errors: usize,
    /// The `ops` event stream: one frame per [`Node::apply`].
    ops_stream: StreamId,
}

impl Drop for Node {
    fn drop(&mut self) {
        self.inner.shutdown.cancel();
    }
}

/// The lease [`Node::hold_band`] took, under an id that is no job: released when
/// this is dropped, whether the caller lets it go on its normal path or its future
/// is dropped between hold and release (a timed-out or disconnected request), so
/// the lease never outlives the request. The release takes no gate and tunes
/// nothing (`LeaseManager::release`).
pub(crate) struct BandHold<'a> {
    node: &'a Node,
    id: JobId,
}

impl Drop for BandHold<'_> {
    fn drop(&mut self) {
        self.node.inner.leases.release(&self.id);
    }
}

impl Node {
    /// Open the node on `cfg.state_dir`, attach `ports`, and, when
    /// `cfg.resume`, resume every `Running` job and every job that `Failed`
    /// because its source was absent and is now attached (same id, context
    /// and creation time): one whose source is absent or cannot be resumed is
    /// kept as `Failed`. A source's capture is restored first (the band its
    /// rigid jobs shared, see [`resume_capture`]), then its jobs resume, a scan
    /// first of all, then widest band first (ties oldest first); one that
    /// still does not fit fails with the `conflict` reason. Elastic jobs
    /// resume after every rigid one and join the capture they left. Attached jobs
    /// resume after every job on the radio, so their input runs first; one
    /// whose input does not run is kept as `Failed` with `input job <id> not
    /// running`, and one that failed that way resumes once its input runs.
    /// Without `cfg.resume` every record is loaded as persisted, untouched.
    /// Either way, before any of that, each attached source pinned in its record
    /// (`state_dir/sources`) is held at its pin (see [`Node::source_pin`]), so a
    /// one-shot node honours the pins too.
    /// `open` itself fails only on
    /// state-directory I/O and on two ports sharing a source id
    /// (`DuplicateSource`).
    pub async fn open(
        cfg: NodeConfig,
        registry: StageRegistry,
        ports: Vec<Arc<dyn Port>>,
    ) -> Result<Self, NodeError> {
        Self::open_with_modes(cfg, registry, ports, None).await
    }

    /// [`Node::open`] with the mode registry `modes` already loaded (and its program rows
    /// probed) by the caller; `None` loads it from `cfg.modes_dir` against `registry`, as
    /// `open` does.
    pub async fn open_with_modes(
        cfg: NodeConfig,
        registry: StageRegistry,
        ports: Vec<Arc<dyn Port>>,
        modes: Option<Arc<ModeRegistry>>,
    ) -> Result<Self, NodeError> {
        let handle = modes.map(ModesHandle::from).unwrap_or_default();
        Self::open_with_handle(cfg, registry, ports, handle).await
    }

    /// [`Node::open`] on the registry `modes` holds (the handle the caller's stages
    /// read the rows through, so a [`Node::reload_modes`] reaches them too); an empty
    /// handle is filled from `cfg.modes_dir` against `registry`, as `open` does.
    pub async fn open_with_handle(
        cfg: NodeConfig,
        registry: StageRegistry,
        ports: Vec<Arc<dyn Port>>,
        modes: ModesHandle,
    ) -> Result<Self, NodeError> {
        let jobs_dir = cfg.state_dir.join("jobs");
        std::fs::create_dir_all(&jobs_dir).map_err(io)?;
        let (records, mut load_errors) = load(&jobs_dir)?;
        let job_load_errors = load_errors.len();
        let (station, station_error) = crate::station::load(&cfg.state_dir);
        load_errors.extend(station_error);
        let sources_dir = cfg.state_dir.join("sources");
        let (source_records, source_errors) = load_sources(&sources_dir);
        load_errors.extend(source_errors);
        let resume = cfg.resume;
        let bursts =
            crate::bursts::BurstStore::open(&cfg.state_dir, cfg.burst_policy).map_err(io)?;
        if cfg.burst_policy.expire_at_open {
            bursts.expire();
        }
        let signatures = crate::signatures::SignatureStore::open(&cfg.state_dir).map_err(io)?;
        load_errors.extend(signatures.load_errors().iter().cloned());
        // Every node lists and scores its accepted signatures.
        let learned_knowledge: Arc<dyn KnowledgeProvider> =
            Arc::new(crate::learned::LearnedKnowledge(signatures.clone()));
        let mut classifiers = ClassifierSet::default();
        classifiers.register(Arc::new(crate::learned::LearnedClassifier(
            signatures.clone(),
        )));
        let sessions = crate::sessions::SessionStore::open(&cfg.state_dir).map_err(io)?;
        sessions.sweep(&bursts.list().into_iter().map(|r| r.id).collect());
        if modes.get().is_none() {
            modes.set(Arc::new(ModeRegistry::load(
                &registry,
                cfg.modes_dir.as_deref(),
            )));
        }
        let sets = Arc::new(crate::sets::SetRegistry::load(
            &modes.get().unwrap_or_default(),
            &registry,
            cfg.sets_dir.as_deref(),
        ));
        // The running sets' instances: an unreadable file is a persist error, never a
        // failure to open.
        let instances_dir = cfg.state_dir.join(SET_INSTANCES_DIR);
        let (instances, instance_errors) = load_instances(&instances_dir);
        let set_instances = instances.into_iter().map(|i| (i.set.clone(), i)).collect();
        let bus = Arc::new(StreamBus::new(BUS_CAPACITY));
        // Registered before jobs resume; their internal applies bypass
        // `Node::apply` and are never logged.
        let t = now();
        let ops_stream = bus.register(
            StreamKind::Event,
            node_provenance(Span {
                start_ns: t,
                end_ns: t,
            }),
        );
        let mut node = Node {
            inner: Arc::new(Inner {
                state_dir: cfg.state_dir.clone(),
                station: Mutex::new(station),
                jobs_dir,
                sources_dir,
                sources: Mutex::new(source_records),
                ring_seconds: cfg.ring_seconds,
                ring_budget_bytes: cfg.ring_budget_bytes,
                scan_dwell_ms: cfg.scan_dwell_ms,
                sweep_start_timeout: Mutex::new(std::time::Duration::from_millis(
                    crate::ops::SWEEP_START_TIMEOUT_MS,
                )),
                source_positions: cfg.source_positions,
                backends: cfg.backends,
                registry,
                modes,
                modes_dir: cfg.modes_dir.clone(),
                reloading: Mutex::new(()),
                sets,
                instances_dir,
                set_instances: Mutex::new(set_instances),
                set_gate: tokio::sync::Mutex::new(()),
                themes: RwLock::new(Arc::new(crate::themes::ThemeRegistry::load(None))),
                bursts,
                signatures,
                sessions,
                proposing: Mutex::new(()),
                leases: LeaseManager::new(),
                bus,
                ports: RwLock::new(HashMap::new()),
                rings: Mutex::new(HashMap::new()),
                ring_ticks: Mutex::new(HashMap::new()),
                bank_factory: RwLock::new(cfg.bank_factory),
                banks: Mutex::new(HashMap::new()),
                jobs: Mutex::new(HashMap::new()),
                knowledge: RwLock::new(vec![learned_knowledge]),
                classifiers: RwLock::new(classifiers),
                persist_errors: Mutex::new(instance_errors),
                ops_log: jsonlog::JsonLog::new(cfg.state_dir.join("ops.log")),
                transcripts_log: jsonlog::JsonLog::new(cfg.state_dir.join("transcripts.log")),
                events_log: jsonlog::JsonLog::with_max_bytes(
                    cfg.state_dir.join("events.log"),
                    jsonlog::EVENTS_MAX_BYTES,
                ),
                log_failed: Default::default(),
                tees: Arc::default(),
                stages: Arc::default(),
                runs: std::sync::atomic::AtomicU64::new(0),
                relaunching: tokio::sync::Mutex::new(()),
                mix_gate: tokio::sync::Mutex::new(()),
                shutdown: CancellationToken::new(),
                verdicts: Mutex::new(HashMap::new()),
                captured: Mutex::new(HashMap::new()),
                episodes: Mutex::new(HashMap::new()),
            }),
            load_errors,
            job_load_errors,
            ops_stream,
        };
        for port in ports {
            node.add_port(port)?;
        }
        // Gains before anything tunes, then pins: each pinned source is held at
        // its band before any record resumes, so the records fit inside it or
        // conflict as they would live.
        node.restore_gains().await;
        node.restore_pins().await;
        // The records in resume order (see `Inner::resume_records`); without `resume`
        // none is launched and every one is loaded as persisted.
        let resumed = node
            .inner
            .resume_records(records, None, |inner, rec| resume && inner.resumable(rec))
            .await;
        if let Some(e) = resumed.error {
            return Err(e);
        }
        {
            let mut jobs = node.inner.jobs.lock().unwrap();
            for rec in resumed.skipped {
                let created = rec.created_ns;
                jobs.insert(rec.id.clone(), JobEntry::loaded(rec, created));
            }
        }
        for (holder, port) in resumed.holds {
            node.inner.leases.release(&holder);
            node.inner.leases.idle_if_unheld(port.as_ref()).await;
        }
        {
            let mut jobs = node.inner.jobs.lock().unwrap();
            node.inner.prune_history(&mut jobs);
        }
        Ok(node)
    }

    /// Set each attached source whose record has a gain to it (see
    /// [`Node::source_set_gain`]); a refusal is an entry `source <id>: gain:
    /// <reason>` in `load_errors`, the record left on disk as it was.
    async fn restore_gains(&mut self) {
        let mut gains: Vec<(SourceId, Option<f64>)> = self
            .inner
            .sources
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(id, r)| r.gain.as_ref().map(|g| (id.clone(), g.db)))
            .collect();
        gains.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        for (id, db) in gains {
            let Some(port) = self.port(&id) else {
                continue;
            };
            if let Err(e) = port.set_gain(db).await {
                self.load_errors
                    .push(format!("source {}: gain: {}", id.0, gain_error(e)));
            }
        }
    }

    /// Lease each attached source whose record has a pin at that band (see
    /// [`Node::source_pin`]). A pin that cannot be taken (its tune fails) is an
    /// entry `source <id>: pin <center_hz>/<bandwidth_hz> Hz: <reason>` in
    /// `load_errors`; the record stays on disk as it was.
    async fn restore_pins(&mut self) {
        let mut pins: Vec<(SourceId, Tuning)> = self
            .inner
            .sources
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(id, r)| r.pin.map(|t| (id.clone(), t)))
            .collect();
        pins.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        for (id, t) in pins {
            let Some(port) = self.port(&id) else {
                continue;
            };
            if let Err(e) = self.inner.leases.pin(port.as_ref(), t).await {
                self.load_errors.push(format!(
                    "source {}: pin {}/{} Hz: {e}",
                    id.0, t.center_hz.0, t.bandwidth_hz.0
                ));
            }
        }
    }

    /// Files in `state_dir/jobs` that could not be read as a `JobRecord`, or
    /// whose record id is not a ULID matching the file name; files in
    /// `state_dir/sources` that could not be read as a [`SourceRecord`]; files in
    /// `state_dir/signatures` that could not be read as a signature record; and the
    /// pins `open` could not restore.
    pub fn load_errors(&self) -> &[String] {
        &self.load_errors
    }

    /// [`Node::load_errors`] split into the job-record files that could not be read
    /// (by file name, or `<file>: format ...`) and the rest (source records, pins
    /// and gains, each entry saying what it is about).
    pub fn load_errors_by_origin(&self) -> (&[String], &[String]) {
        self.load_errors.split_at(self.job_load_errors)
    }

    /// Attach a port and start feeding its blocks into that source's ring and
    /// channel bank (made now for the port's current band, if it has one, and
    /// remade whenever its blocks show a new band). A second port with an
    /// attached source's id is `DuplicateSource`. Does not resume jobs. A port that
    /// reports heals ([`Port::healed`], a supervised worker) is watched from now on:
    /// each heal recovers the source (see [`Inner::recover_source`]); one that reports
    /// its worker's start ([`Port::starting_changed`]) has it said on its feed (see
    /// `recover::watch_starting`).
    pub fn add_port(&self, port: Arc<dyn Port>) -> Result<(), NodeError> {
        let inner = &self.inner;
        let source = port.id().clone();
        {
            let mut ports = inner.ports.write().unwrap();
            if ports.contains_key(&source) {
                return Err(NodeError::DuplicateSource(source));
            }
            ports.insert(source.clone(), port.clone());
        }
        inner.bus.register_feed(&source);
        inner.tee_events(&source);
        inner.note_capabilities(port.as_ref());
        if let Some(band) = port.current() {
            inner.sync_bank(&source, band);
        }
        tokio::spawn(feed_ring(inner.clone(), source.clone(), port.subscribe()));
        tokio::spawn(feed_bank(inner.clone(), source.clone(), port.subscribe()));
        if let Some(rx) = port.starting_changed() {
            tokio::spawn(recover::watch_starting(inner.clone(), source.clone(), rx));
        }
        if let Some(rx) = port.healed() {
            tokio::spawn(recover::watch_healed(inner.clone(), source, rx));
        }
        Ok(())
    }

    /// Use `factory` for the channel banks of wide sources (rate at least
    /// [`PFB_MIN_RATE_HZ`]); existing banks are dropped and remade with it
    /// for every port that has a current band.
    pub fn set_bank_factory(&self, factory: Arc<dyn BankFactory>) {
        *self.inner.bank_factory.write().unwrap() = Some(factory);
        self.inner.banks.lock().unwrap().clear();
        let ports: Vec<_> = self.inner.ports.read().unwrap().values().cloned().collect();
        for p in ports {
            if let Some(band) = p.current() {
                self.inner.sync_bank(p.id(), band);
            }
        }
    }

    /// Use `themes` as the page themes (`themes_list`, the served theme files).
    pub fn set_themes(&self, themes: Arc<crate::themes::ThemeRegistry>) {
        *self.inner.themes.write().unwrap() = themes;
    }

    /// The named sets (`set_list`): the embedded ones and the operator's `sets_dir`.
    pub fn sets(&self) -> Arc<crate::sets::SetRegistry> {
        self.inner.sets.clone()
    }

    /// The running sets' instances by set id (`state_dir/set-instances`, read back at `open`).
    pub fn set_instances(&self) -> BTreeMap<String, crate::sets::SetInstance> {
        self.inner.set_instances.lock().unwrap().clone()
    }

    /// Hold the node's set gate until the guard drops (see `Inner::set_gate`).
    pub(crate) async fn set_gate(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.set_gate.lock().await
    }

    /// Keep `inst` as its set's running instance and write it to `state_dir/set-instances`. A
    /// write that fails is returned; the instance is kept in memory all the same, so a
    /// `set_stop` in this run still finds it.
    pub(crate) fn put_set_instance(&self, inst: crate::sets::SetInstance) -> Result<(), NodeError> {
        let written = persist_instance(&self.inner.instances_dir, &inst);
        self.inner
            .set_instances
            .lock()
            .unwrap()
            .insert(inst.set.clone(), inst);
        written
    }

    /// Forget set `set`'s instance and remove its file.
    pub(crate) fn drop_set_instance(&self, set: &str) -> Result<(), NodeError> {
        self.inner.set_instances.lock().unwrap().remove(set);
        remove_instance(&self.inner.instances_dir, set)
    }

    /// Drop every instance none of whose jobs still runs (each stopped, failed or
    /// forgotten from the history); a file that cannot be removed is a persist error.
    /// It writes the state dir, so only a mutating op calls it, under the set gate
    /// (`set_start`); `set_list` computes the same in memory.
    pub(crate) fn prune_set_instances(&self, _set_gate: &tokio::sync::MutexGuard<'_, ()>) {
        let live: std::collections::HashSet<JobId> = self
            .job_list()
            .into_iter()
            .filter(|r| !job_ended(&r.state))
            .map(|r| r.id)
            .collect();
        let ended: Vec<String> = self
            .inner
            .set_instances
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, i)| !i.jobs().any(|j| live.contains(j)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ended {
            if let Err(e) = self.drop_set_instance(&id) {
                self.inner
                    .persist_errors
                    .lock()
                    .unwrap()
                    .push(format!("{SET_INSTANCES_DIR}/{id}.json: {e}"));
            }
        }
    }

    /// The page themes (the built-in ones unless `set_themes` replaced them).
    pub fn themes(&self) -> Arc<crate::themes::ThemeRegistry> {
        self.inner.themes.read().unwrap().clone()
    }

    /// The channel bank the node keeps for `source`, if any.
    pub fn bank(&self, source: &SourceId) -> Option<Arc<dyn ChannelBank>> {
        self.inner
            .banks
            .lock()
            .unwrap()
            .get(source)
            .and_then(|s| s.bank.clone())
    }

    /// See [`Inner::bank_bin_rate`].
    pub(crate) fn bank_bin_rate(&self, source: &SourceId, band: Tuning) -> Option<u64> {
        self.inner.bank_bin_rate(source, band)
    }

    /// Register a knowledge provider; `KnowledgeLookup` queries all of them.
    pub fn add_knowledge(&self, p: Arc<dyn KnowledgeProvider>) {
        self.inner.knowledge.write().unwrap().push(p);
    }

    /// Register a classifier; `Classify` runs all of them in layer order.
    pub fn add_classifier(&self, c: Arc<dyn Classifier>) {
        self.inner.classifiers.write().unwrap().register(c);
    }

    /// The decoder modes as they stand now (see [`crate::modes`]): loaded at `open`,
    /// replaced by [`Node::reload_modes`].
    pub fn modes(&self) -> Arc<ModeRegistry> {
        self.inner.modes()
    }

    /// The handle the node holds its modes in: a classifier or provider given it reads
    /// the rows as they stand at each use, a reload included.
    pub fn modes_handle(&self) -> ModesHandle {
        self.inner.modes.clone()
    }

    /// Read the mode rows again (the embedded ones, then the operator directory
    /// `NodeConfig.modes_dir`, none when it is `None`, as at `open`) against the
    /// node's stages, carry each program row's probed program and state over from the
    /// rows in force ([`ModeRegistry::carry_programs`]), read the checked times, and put
    /// the new registry in force. Jobs already running keep the graphs they were
    /// expanded to. The broken rows' startup lines
    /// ([`ModeRegistry::startup_lines`]), for the caller to show.
    pub fn reload_modes(&self) -> Result<Vec<String>, NodeError> {
        let inner = &self.inner;
        // One reload at a time: a later one reads every file an earlier one saw.
        let _one = inner.reloading.lock().unwrap_or_else(|e| e.into_inner());
        let mut modes = ModeRegistry::load(&inner.registry, inner.modes_dir.as_deref());
        modes.carry_programs(&inner.modes());
        modes.load_checked(&inner.state_dir.join(MODES_CHECKED_FILE));
        let lines = modes.startup_lines();
        inner.modes.set(Arc::new(modes));
        Ok(lines)
    }

    /// The burst store: the windows `capture` jobs wrote.
    pub fn bursts(&self) -> Arc<crate::bursts::BurstStore> {
        self.inner.bursts.clone()
    }

    /// The learned signatures (`state_dir/signatures`).
    pub fn signatures(&self) -> Arc<crate::signatures::SignatureStore> {
        self.inner.signatures.clone()
    }

    /// The exploration jobs' session records (`state_dir/sessions`), oldest first.
    pub fn sessions(&self) -> Vec<crate::sessions::Session> {
        self.inner.sessions.list()
    }

    /// Remove the session records none of whose captures is left in the burst store.
    pub(crate) fn sweep_sessions(&self) {
        self.inner.sweep_sessions();
    }

    /// An `unknown` error naming `id` and the store's signature ids.
    pub(crate) fn unknown_signature(&self, id: &str) -> OpError {
        OpError::Unknown {
            field: "id".into(),
            value: id.into(),
            known: self
                .signatures()
                .list(None)
                .into_iter()
                .map(|r| r.id)
                .collect(),
        }
    }

    /// The store's refusal as the ops answer it: an unknown id names the store's ids, a
    /// record in the wrong state is `signature <id> is <state>, not proposed`.
    fn signature_error(&self, e: crate::signatures::StoreError) -> OpError {
        use crate::signatures::StoreError;
        match e {
            StoreError::Unknown(id) => self.unknown_signature(&id),
            StoreError::State { id, state } => OpError::Other {
                message: format!("signature {id} is {state}, not proposed"),
            },
            e @ StoreError::Io(_) => OpError::Other {
                message: e.to_string(),
            },
        }
    }

    /// `signature_propose`: a proposal from the capture `capture`, by the transport
    /// `by`. The capture's feature record and its layer-1 label (by
    /// [`crate::ops::classify_capture`]; `noise` is refused) make the waveform; its set's
    /// captures, its job, its first `tx` assignment and the node's station the
    /// provenance. A `framing` the `frame_decode` stage refuses is `framing: <reason>`.
    pub async fn signature_propose(
        &self,
        capture: &str,
        name: Option<String>,
        framing: Option<serde_json::Value>,
        by: &str,
    ) -> Result<crate::signatures::SignatureRecord, OpError> {
        use crate::modes::FRAME_DECODE_STAGE;
        use crate::signatures::{NOISE_LABEL, NewSignature};
        let other = |message: String| OpError::Other { message };
        let bursts = self.bursts();
        let rec = bursts.get(capture).ok_or_else(|| OpError::Unknown {
            field: "capture".into(),
            value: capture.into(),
            known: bursts.list().into_iter().map(|r| r.id).collect(),
        })?;
        let features: crate::classify::FeatureRecord = serde_json::from_value(rec.features.clone())
            .map_err(|_| other(format!("capture {capture} has no feature record")))?;
        if let Some(f) = &framing {
            let registry = &self.inner.registry;
            if registry.spec(FRAME_DECODE_STAGE).is_none() {
                return Err(other(format!(
                    "framing: {FRAME_DECODE_STAGE} is not a registered stage"
                )));
            }
            registry
                .check_params(FRAME_DECODE_STAGE, f)
                .map_err(|e| other(format!("framing: {e}")))?;
        }
        let classified = crate::ops::classify_capture(&self.classifiers(), &rec)
            .await
            .map_err(other)?;
        let label = classified
            .labels
            .into_iter()
            .find(|l| l.layer == 1)
            .ok_or_else(|| other(format!("capture {capture} has no layer-1 label")))?;
        if label.name == NOISE_LABEL {
            return Err(other(format!(
                "capture {capture} is noise: nothing to propose"
            )));
        }
        let allocation =
            serde_json::from_value::<Vec<crate::knowledge::Allocation>>(rec.allocations.clone())
                .unwrap_or_default()
                .into_iter()
                .find(|a| a.role.as_deref() == Some("tx"));
        // The set's captures in the order they were written.
        let mut captures: Vec<String> = bursts
            .list()
            .into_iter()
            .filter(|r| r.set == rec.set)
            .map(|r| r.id)
            .collect();
        captures.reverse();
        let evidence = match framing {
            Some(_) => format!("framing given by hand; {}", label.evidence),
            None => format!("features only: {}", label.evidence),
        };
        let new = NewSignature {
            name,
            features,
            modulation: label.name,
            framing,
            crc_frames: 0,
            evidence,
            source: rec.source,
            station: self.station(),
            captures,
            jobs: vec![rec.job],
            allocation,
            proposed_by: by.to_string(),
        };
        self.signatures()
            .propose(new, now().0)
            .map_err(|e| self.signature_error(e))
    }

    /// `signature_accept`: accept the proposal `id` (renamed to `name` when given) by the
    /// transport `by`, pinning every one of its captures still in the burst store. The
    /// captures are pinned first, so an accepted record never has captures left to expire.
    pub fn signature_accept(
        &self,
        id: &str,
        name: Option<String>,
        by: &str,
    ) -> Result<crate::signatures::SignatureRecord, OpError> {
        use crate::signatures::{SignatureState, StoreError};
        let store = self.signatures();
        let rec = store.get(id).ok_or_else(|| self.unknown_signature(id))?;
        if rec.state != SignatureState::Proposed {
            return Err(self.signature_error(StoreError::State {
                id: rec.id,
                state: rec.state,
            }));
        }
        let bursts = self.bursts();
        for c in &rec.provenance.captures {
            match bursts.pin(c) {
                // Expired or deleted since the proposal: nothing left to keep.
                Ok(()) | Err(crate::bursts::StoreError::Unknown(_)) => {}
                Err(e) => {
                    return Err(OpError::Other {
                        message: format!("capture {c}: {e}"),
                    });
                }
            }
        }
        let rec = store
            .accept(id, name, by, now().0)
            .map_err(|e| self.signature_error(e))?;
        Ok(self.write_learned_row(rec))
    }

    /// After `rec`'s accept, when [`crate::signatures::writes_row`]: write its
    /// [`crate::signatures::learned_row`] to `<modes dir>/learned_<id>.json` (pretty
    /// JSON, the directory made; the modes directory is `NodeConfig.modes_dir`) and
    /// [`Node::reload_modes`], noting the row (`decoder`) and its file in the record. A
    /// file already there is kept as it is (`row learned_<id> already present: kept as
    /// is`), never overwritten, and the rows are reloaded so it is listed at once; with
    /// no modes directory (`NodeConfig.modes_dir` `None`: `no modes directory: row not
    /// written`) or a failed write
    /// (`row not written: <error>`) no row is noted. Never fails: the record stays
    /// accepted, its captures pinned, whatever becomes of the row.
    fn write_learned_row(
        &self,
        rec: crate::signatures::SignatureRecord,
    ) -> crate::signatures::SignatureRecord {
        use crate::signatures::{LEARNED_ROW_PREFIX, learned_row, writes_row};
        use std::io::Write;
        if !writes_row(&rec) {
            return rec;
        }
        let mode = format!("{LEARNED_ROW_PREFIX}{}", rec.id.to_lowercase());
        let reload = || {
            if let Ok(lines) = self.reload_modes() {
                for line in lines {
                    eprintln!("{line}");
                }
            }
        };
        let (decoder, file, note) = match self.inner.modes_dir.clone() {
            None => (
                None,
                None,
                Some("no modes directory: row not written".into()),
            ),
            Some(dir) => {
                let path = dir.join(format!("{mode}.json"));
                let file = path.display().to_string();
                let kept = format!("row {mode} already present: kept as is");
                let written = serde_json::to_vec_pretty(&learned_row(&rec, &mode))
                    .map_err(std::io::Error::other)
                    .and_then(|json| {
                        // `AlreadyExists` below means the row file only (a plain file
                        // where the directory should be is a failure).
                        std::fs::create_dir_all(&dir).map_err(std::io::Error::other)?;
                        // `create_new`: a file there (the operator's, or one written
                        // meanwhile) is never overwritten.
                        let mut f = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)?;
                        let body = f.write_all(&json).and_then(|()| f.write_all(b"\n"));
                        if body.is_err() {
                            // No half-written row left to be kept as the operator's.
                            let _ = std::fs::remove_file(&path);
                        }
                        body
                    });
                match written {
                    Ok(()) => {
                        reload();
                        (Some(mode), Some(file), None)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        // An operator file that appeared after `open` is listed at once.
                        reload();
                        (Some(mode), Some(file), Some(kept))
                    }
                    Err(e) => (None, None, Some(format!("row not written: {e}"))),
                }
            }
        };
        match self.signatures().set_decoder(&rec.id, decoder, file, note) {
            Ok(rec) => rec,
            Err(e) => {
                self.inner
                    .persist_errors
                    .lock()
                    .unwrap()
                    .push(format!("signatures: {}: {e}", rec.id));
                rec
            }
        }
    }

    /// `signature_reject`: reject the proposal `id`, by the transport `by`.
    pub fn signature_reject(
        &self,
        id: &str,
        by: &str,
    ) -> Result<crate::signatures::SignatureRecord, OpError> {
        self.signatures()
            .reject(id, by, now().0)
            .map_err(|e| self.signature_error(e))
    }

    /// A classify's proofs (each `tried` row that `proved`, on the classified band
    /// `band`) supersede the accepted signatures they overlap ([`Inner::supersede_by`]).
    pub(crate) fn supersede_proved(&self, tried: &[crate::classify::Tried], band: Tuning) {
        let t = now().0;
        for p in tried
            .iter()
            .filter(|p| p.outcome == crate::classify::Outcome::Proved)
        {
            self.inner.supersede_by(&p.mode, band, t);
        }
    }

    pub(crate) fn knowledge(&self) -> Vec<Arc<dyn KnowledgeProvider>> {
        self.inner.knowledge.read().unwrap().clone()
    }

    pub(crate) fn classifiers(&self) -> ClassifierSet {
        self.inner.classifiers.read().unwrap().clone()
    }

    pub(crate) fn port(&self, id: &SourceId) -> Option<Arc<dyn Port>> {
        self.inner.port(id)
    }

    /// The fault `id`'s port reports, if any; reading one opens the source's fault
    /// episode (see [`recover::Episode`]), so the jobs that fail of it from now on are
    /// relaunched when the port heals.
    pub(crate) fn source_fault(&self, id: &SourceId) -> Option<String> {
        let fault = self.port(id).and_then(|p| p.fault());
        if fault.is_some() {
            self.inner.note_fault(id);
        }
        fault
    }

    pub(crate) fn state_dir(&self) -> &Path {
        &self.inner.state_dir
    }

    /// [`crate::stage::StageRegistry::readiness`] of the stage kind `kind` on this node.
    pub fn stage_readiness(&self, kind: &str) -> Option<crate::stage::Readiness> {
        self.inner.registry.readiness(kind)
    }

    /// [`crate::ops::schema`] plus `stages`: every registered stage kind with
    /// its stream kinds and params schema (see [`StageRegistry::describe`]), with
    /// `$defs.JobKind.enum` (in the op, result and error schemas) extended by this
    /// node's mode rows, its operator rows included.
    pub fn schema(&self) -> serde_json::Value {
        let mut v = crate::ops::schema();
        v["stages"] = self.inner.registry.describe();
        let ids = self.inner.modes().ids();
        for part in ["op", "result", "error"] {
            let Some(kinds) = v[part]["$defs"]
                .get_mut("JobKind")
                .and_then(|k| k.get_mut("enum"))
                .and_then(serde_json::Value::as_array_mut)
            else {
                continue;
            };
            for id in ids.iter().filter(|id| crate::modes::is_mode_id(id)) {
                if !kinds.iter().any(|k| k == id.as_str()) {
                    kinds.push(id.as_str().into());
                }
            }
        }
        v
    }

    /// Apply one operation; see [`crate::ops`]. Once it completes, success
    /// or failure, one [`crate::ops::ops_payload`] event is published on
    /// [`Node::ops_stream`] (so a tail of that stream never sees its own record).
    /// A cancelled or dropped `apply` future logs nothing. The record's
    /// `transport` is `"local"`; see [`Node::apply_from`].
    pub async fn apply(&self, op: Op) -> Result<OpResult, OpError> {
        self.apply_from(op, "local").await
    }

    /// [`Node::apply`] with the record's `transport` set to `transport`: who
    /// asked (`"local"`, `"http"`, `"ui"`, `"agent"`), so a reader of the
    /// `ops` stream can tell its own calls from others'.
    pub async fn apply_from(&self, op: Op, transport: &'static str) -> Result<OpResult, OpError> {
        let logged = op.clone();
        let start_ns = now();
        let started = std::time::Instant::now();
        let outcome = crate::ops::apply(self, op, transport).await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let payload = crate::ops::ops_payload(&logged, &outcome, elapsed_ms, transport);
        let mut line = payload.clone();
        self.inner.bus.publish(Frame::new(
            self.ops_stream.clone(),
            node_provenance(Span {
                start_ns,
                end_ns: now(),
            }),
            Payload::Event(payload),
        ));
        line["t_ns"] = start_ns.0.into();
        // A successful read is logged without its result: who asked, and when.
        if outcome.is_ok()
            && logged.read_only()
            && let Some(m) = line.as_object_mut()
        {
            m.remove("result");
        }
        self.inner.log_line(LogFile::Ops, &line);
        outcome
    }

    /// The `ops` event stream: kind `Event`, provenance source
    /// [`NODE_SOURCE`], one frame per [`Node::apply`].
    pub fn ops_stream(&self) -> StreamId {
        self.ops_stream.clone()
    }

    pub fn ports(&self) -> Vec<(SourceId, Capabilities)> {
        let ports = self.inner.ports.read().unwrap();
        ports
            .values()
            .map(|p| (p.id().clone(), p.capabilities().clone()))
            .collect()
    }

    /// Start a job of `kind` on `graph`, leasing `tuning` on `source`. `capture_rate_hz`
    /// is the source rate `graph` was expanded for (recorded, so a resume at another
    /// rate re-expands it), `None` for a graph that does not depend on it. An
    /// `elastic` job (a built-in kind) takes an elastic lease: it joins the source's
    /// capture, its `tuning` becomes that capture, and it is relaunched under the same
    /// id on every retune (see [`crate::lease::LeaseManager::ensure`]).
    pub async fn job_start(
        &self,
        kind: &str,
        source: SourceId,
        tuning: Tuning,
        graph: Graph,
        capture_rate_hz: Option<Hz>,
        elastic: bool,
    ) -> Result<JobId, NodeError> {
        self.check_watch_policy(&graph, tuning)?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.to_string(),
            graph,
            source,
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: capture_rate_hz.map(|r| r.0),
            elastic,
            operator_graph: false,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// Start a job of `kind` on `graph` attached to the job `input`: recorded with
    /// `input`, the input's `source` and `tuning`, and launched without a lease (see
    /// [`crate::stream_in`]); the launch points every `stream_in` node at the input's
    /// audio stream. `UnknownJob` for an unknown input; `InputNotRunning` and
    /// `NoAudioStream` as [`Node::job_audio_stream`]; on error nothing is recorded.
    pub async fn job_attach(
        &self,
        kind: &str,
        input: &JobId,
        graph: Graph,
    ) -> Result<JobId, NodeError> {
        let (source, tuning) = {
            let jobs = self.inner.jobs.lock().unwrap();
            let e = jobs
                .get(input)
                .ok_or_else(|| NodeError::UnknownJob(input.clone()))?;
            (e.record.source.clone(), e.record.tuning)
        };
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.to_string(),
            graph,
            source,
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: Some(input.clone()),
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// Start a mix of `sources` (`None` for every source): one leaseless job on the
    /// placeholder source [`NODE_SOURCE`], tuning 0/0. With one already recorded running
    /// on the same set (a mute plays no part; `None` matches only `None`) its id is the
    /// answer. `UnknownSource` for a source not attached, `Other` for an empty list.
    pub async fn mix_start(&self, sources: Option<Vec<SourceId>>) -> Result<JobId, NodeError> {
        if let Some(list) = &sources {
            if list.is_empty() {
                return Err(NodeError::Other(
                    "sources: empty: pass null for every source".into(),
                ));
            }
            if let Some(s) = list.iter().find(|s| self.inner.port(s).is_none()) {
                return Err(NodeError::UnknownSource(s.clone()));
            }
        }
        let graph = crate::ops::mix_graph(sources.as_deref());
        let wanted = crate::ops::mix_graph_sources(&graph);
        let _gate = self.inner.mix_gate.lock().await;
        let running = self.job_list().into_iter().find(|r| {
            r.kind == JobKind::Mix.name()
                && r.state == JobState::Running
                && crate::ops::mix_graph_sources(&r.graph) == wanted
        });
        if let Some(r) = running {
            return Ok(r.id);
        }
        graph::validate(&graph, &self.inner.registry)?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: JobKind::Mix.name().to_string(),
            graph,
            source: SourceId(NODE_SOURCE.into()),
            tuning: Tuning {
                center_hz: Hz(0),
                bandwidth_hz: Hz(0),
            },
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// Start a job from `graph`, whose first node takes no input (`capture_in`,
    /// `stream_in`, `mix`): a leaseless [`GRAPH_KIND`](crate::ops::GRAPH_KIND) record on
    /// [`NODE_SOURCE`], its band the first `capture_in`'s first capture's (0/0 without
    /// one), launched as stored (`operator_graph`). `Graph` when it does not validate;
    /// `Other` for a first node that takes input or a node with no edge into it whose
    /// stage takes input (nothing would feed it: there is no radio); `Expand(Unknown)`
    /// naming `captures[<i>]` for a capture not in the burst store.
    pub async fn graph_start(&self, graph: Graph) -> Result<JobId, NodeError> {
        graph::validate(&graph, &self.inner.registry)?;
        let registry = &self.inner.registry;
        let takes_input = |n: &crate::graph::Node| {
            registry
                .spec(&n.kind)
                .and_then(|s| s.inputs.first().copied())
        };
        let kind_name = |k: StreamKind| {
            serde_json::to_value(k)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default()
        };
        let unfed = |n: &crate::graph::Node, k: StreamKind| {
            NodeError::Other(format!(
                "graph_start: node {} ({}) has an unfed {} input",
                n.id.0,
                n.kind,
                kind_name(k)
            ))
        };
        let first = graph
            .nodes
            .first()
            .ok_or_else(|| NodeError::Other("graph_start: the graph has no nodes".into()))?;
        if let Some(k) = takes_input(first) {
            // A lone stage that takes input is a node nothing feeds; one in a chain is a
            // root that would wait for a radio.
            if !graph
                .edges
                .iter()
                .any(|e| e.from == first.id || e.to == first.id)
            {
                return Err(unfed(first, k));
            }
            return Err(NodeError::Other(format!(
                "graph_start: node {} ({}) takes input; the first node must take none \
                 (capture_in, stream_in, mix)",
                first.id.0, first.kind
            )));
        }
        for n in &graph.nodes {
            if let Some(k) = takes_input(n)
                && !graph.edges.iter().any(|e| e.to == n.id)
            {
                return Err(unfed(n, k));
            }
        }
        let store = self.bursts();
        let mut tuning = Tuning {
            center_hz: Hz(0),
            bandwidth_hz: Hz(0),
        };
        let mut placed = false;
        for n in graph
            .nodes
            .iter()
            .filter(|n| n.kind == crate::ops::CAPTURE_IN_STAGE)
        {
            let ids = n.params.get("captures").and_then(|v| v.as_array());
            for (i, id) in ids.into_iter().flatten().enumerate() {
                let id = id.as_str().unwrap_or_default();
                let Some(rec) = store.get(id) else {
                    return Err(NodeError::Expand(OpError::Unknown {
                        field: format!("captures[{i}]"),
                        value: id.to_string(),
                        known: store.list().into_iter().map(|r| r.id).collect(),
                    }));
                };
                if !placed {
                    tuning = Tuning {
                        center_hz: rec.center_hz,
                        bandwidth_hz: rec.bandwidth_hz,
                    };
                    placed = true;
                }
            }
        }
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: crate::ops::GRAPH_KIND.to_string(),
            graph,
            source: SourceId(NODE_SOURCE.into()),
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: true,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// Start the bits chain ([`crate::bits::bits_graph`]) over the set of the burst
    /// capture `capture`, in time order, each `None` filled from the capture's feature
    /// record or, for `modulation`, the layer-1 label the classifiers give its samples: a
    /// leaseless [`BITS_KIND`](crate::ops::BITS_KIND) record on [`NODE_SOURCE`], its band
    /// the capture's, `context.bits` the plan, launched as stored as `graph_start`
    /// launches. `Expand(Unknown)` naming `capture` for a capture not in the store;
    /// `Other` for what [`crate::bits::bits_plan`] refuses.
    pub async fn bits_start(
        &self,
        capture: &str,
        modulation: Option<crate::bits::Modulation>,
        symbol_rate_baud: Option<crate::types::Baud>,
        order: Option<u8>,
    ) -> Result<JobId, NodeError> {
        let store = self.bursts();
        let Some(named) = store.get(capture) else {
            return Err(NodeError::Expand(OpError::Unknown {
                field: "capture".into(),
                value: capture.to_string(),
                known: store.list().into_iter().map(|r| r.id).collect(),
            }));
        };
        let mut set: Vec<_> = store
            .list()
            .into_iter()
            .filter(|r| r.set == named.set)
            .collect();
        set.sort_by_key(|r| (r.start_ns, r.id.clone()));
        // A record that does not read as one (an older store's `{}`) measured nothing.
        let features: crate::classify::FeatureRecord = serde_json::from_value(
            named.features.clone(),
        )
        .unwrap_or(crate::classify::FeatureRecord {
            bandwidth_hz: named.bandwidth_hz,
            center_hz: named.center_hz,
            symbol_rate_baud: None,
            snr_db: 0.0,
            burst: false,
            duty: None,
            bursts: None,
            burst_ms: None,
            period_ms: None,
            order: None,
            tone_spacing_hz: None,
            symbol_rate_line: None,
        });
        // The layer-1 label names the modulation when the request does not, and a
        // `noise` label keeps the chain off the measured centre; with the modulation
        // given, a capture that does not classify is only unlabelled.
        let classified = crate::ops::classify_capture(&self.classifiers(), &named).await;
        let classified = match (modulation, classified) {
            (_, Ok(c)) => Some(c),
            (Some(_), Err(_)) => None,
            (None, Err(e)) => return Err(NodeError::Other(e)),
        };
        let label =
            classified.and_then(|c| c.labels.into_iter().find(|l| l.layer == 1).map(|l| l.name));
        let plan = crate::bits::bits_plan(
            &set,
            &features,
            label.as_deref(),
            modulation,
            symbol_rate_baud,
            order,
        )
        .map_err(NodeError::Other)?;
        let graph = crate::bits::bits_graph(&plan);
        graph::validate(&graph, &self.inner.registry)?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: crate::ops::BITS_KIND.to_string(),
            graph,
            source: SourceId(NODE_SOURCE.into()),
            tuning: Tuning {
                center_hz: named.center_hz,
                bandwidth_hz: named.bandwidth_hz,
            },
            state: JobState::Running,
            context: serde_json::json!({ "bits": plan }),
            created_ns: now(),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: true,
            owner: None,
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }

    /// The audio stream a job attached to `job` would read: the last audio output stream
    /// (not an edge between its stages) in `job`'s graph order. `InputNotRunning` unless
    /// `job` runs in this node (a one-shot node runs only what it started),
    /// `NoAudioStream` when it outputs no audio.
    pub fn job_audio_stream(&self, job: &JobId) -> Result<StreamId, NodeError> {
        self.inner.audio_stream(job)
    }

    /// The output streams (those no edge of its graph consumes) of `job`'s current run,
    /// in graph order; empty for a job that does not run here. A `watch`'s one event
    /// output carries `detect`'s `signal` events and the watch's own.
    pub fn job_outputs(&self, job: &JobId) -> Vec<(StreamKind, StreamId)> {
        self.inner
            .jobs
            .lock()
            .unwrap()
            .get(job)
            .filter(|e| e.cancel.is_some())
            .map(|e| e.outputs.iter().map(|o| (o.kind, o.id.clone())).collect())
            .unwrap_or_default()
    }

    /// Persist the job as `Stopped`, then cancel its stages and wait for its
    /// supervisor to release the lease and unregister its streams. If the
    /// record cannot be written the job is left running and `Io` is returned.
    /// A `Failed` job keeps its state and reason.
    pub async fn job_stop(&self, id: &JobId) -> Result<(), NodeError> {
        self.inner.stop(id).await
    }

    /// Hold `source`'s gate until the guard drops when `replace` names jobs, so
    /// stopping them and the request that follows happen under one hold (see
    /// [`Node::replace_holders`]); `None` (each step takes the gate itself, as
    /// without replace) when it names none. See [`LeaseManager::gate_guard`].
    pub(crate) async fn gate_for_replace(
        &self,
        source: &SourceId,
        replace: &[JobId],
    ) -> Option<OwnedMutexGuard<()>> {
        match replace.is_empty() {
            true => None,
            false => Some(self.inner.leases.gate_guard(source).await),
        }
    }

    /// Hold `source`'s gate until the guard drops; see [`LeaseManager::gate_guard`].
    pub(crate) async fn gate_guard(&self, source: &SourceId) -> OwnedMutexGuard<()> {
        self.inner.leases.gate_guard(source).await
    }

    /// The first running, rigid, non-attached job on `source` that a start of
    /// `kind` at `tuning` with `deviation_hz` and `sideband` would repeat: the
    /// same kind string and tuning, and the same start params once both sides
    /// are read as the kind's defaults (`fm_demod` 5000 Hz, `wfm_demod` 75000 Hz,
    /// `ssb_demod` usb; other kinds have none). A job another job owns (an
    /// `identify` or `watch` trial child) is never named. A kind the node does not
    /// expand has no such job.
    pub(crate) fn duplicate_of(
        &self,
        source: &SourceId,
        kind: &str,
        tuning: Tuning,
        deviation_hz: Option<u64>,
        sideband: Option<Sideband>,
    ) -> Option<JobId> {
        let k = self.expandable_kind(kind)?;
        let norm = |(d, s): (Option<u64>, Option<Sideband>)| match k {
            JobKind::FmDemod => (Some(d.unwrap_or(FM_DEVIATION_DEFAULT_HZ)), None),
            JobKind::WfmDemod => (Some(d.unwrap_or(WFM_DEVIATION_DEFAULT_HZ)), None),
            JobKind::SsbDemod => (None, Some(s.unwrap_or(Sideband::Usb))),
            _ => (None, None),
        };
        let want = norm((deviation_hz, sideband));
        self.job_list()
            .into_iter()
            .find(|r| {
                r.state == JobState::Running
                    && !r.elastic
                    && r.input.is_none()
                    && r.owner.is_none()
                    && &r.source == source
                    && r.kind == kind
                    && r.tuning == tuning
                    && norm(k.start_params(&r.graph)) == want
            })
            .map(|r| r.id)
    }

    /// Stop the jobs `replace` names, under `source`'s gate (`guard`); see
    /// [`Inner::replace_holders`].
    pub(crate) async fn replace_holders(
        &self,
        source: &SourceId,
        replace: &[JobId],
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(), NodeError> {
        self.inner.replace_holders(source, replace, guard).await
    }

    /// Check the jobs `replace` names, under `source`'s gate (`guard`), stopping
    /// none; see [`Inner::check_replace`].
    pub(crate) fn check_replace(
        &self,
        source: &SourceId,
        replace: &[JobId],
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(), NodeError> {
        self.inner.check_replace(source, replace, guard)
    }

    /// The running watch `graph` has the policy `params` (a set's `watch` member's): its
    /// `detect` params ([`watch::policy_of`]) equal them, a key either leaves out
    /// compared at the `detect` stage's default ([`watch::same_policy`]).
    pub(crate) fn watch_has_policy(&self, graph: &Graph, params: &serde_json::Value) -> bool {
        let schema = self
            .inner
            .registry
            .spec(crate::ops::DETECT_STAGE)
            .and_then(|s| serde_json::to_value(&s.params_schema).ok())
            .unwrap_or_default();
        watch::same_policy(&watch::policy_of(graph), params, &schema)
    }

    /// A watch's `detect` node names only this node's sources in `prefer`
    /// (`bad params on node <id>: prefer[<i>]: unknown source <id>` otherwise), and its
    /// `modes`, when not `null`, only loaded, ready rows no wider than the watch's band
    /// `tuning` (see [`watch::check_modes_list`]). Checked at `job_start` and `graph_put`
    /// only, so a resume on a node started without one of them still runs (the watch
    /// then skips it).
    pub(crate) fn check_watch_policy(
        &self,
        graph: &Graph,
        tuning: Tuning,
    ) -> Result<(), NodeError> {
        let detects = graph
            .nodes
            .iter()
            .filter(|n| n.kind == crate::ops::DETECT_STAGE);
        let modes = self.inner.modes();
        for n in detects.clone() {
            let Some(list) = n.params.get("modes").and_then(|m| m.as_array()) else {
                continue;
            };
            let list: Vec<String> = list
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect();
            if let Err(reason) = watch::check_modes_list(&modes, &list, tuning.bandwidth_hz.0) {
                return Err(GraphError::BadParams {
                    node: n.id.clone(),
                    reason,
                }
                .into());
            }
        }
        let ports = self.inner.ports.read().unwrap();
        for n in detects {
            let Some(prefer) = n.params.get("prefer").and_then(|p| p.as_array()) else {
                continue;
            };
            for (i, id) in prefer.iter().enumerate() {
                let Some(id) = id.as_str() else { continue };
                if !ports.contains_key(&SourceId(id.into())) {
                    return Err(GraphError::BadParams {
                        node: n.id.clone(),
                        reason: format!("prefer[{i}]: unknown source {id}"),
                    }
                    .into());
                }
            }
        }
        Ok(())
    }

    /// [`Node::job_start`], with `guard` under `source`'s gate: the lease is taken
    /// without letting the gate go. What must wait for the gate to be released
    /// (relaunching the elastic jobs a tune moved, stopping those a scan
    /// displaces) is returned, for [`Node::after_gate`] once the guard is dropped.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn job_start_gated(
        &self,
        kind: &str,
        source: SourceId,
        tuning: Tuning,
        graph: Graph,
        capture_rate_hz: Option<Hz>,
        elastic: bool,
        guard: Option<&OwnedMutexGuard<()>>,
    ) -> (Result<JobId, NodeError>, AfterGate) {
        self.scan_start_replacing(
            kind,
            source,
            tuning,
            graph,
            capture_rate_hz,
            elastic,
            guard,
            &[],
        )
        .await
    }

    /// [`Node::job_start_gated`] of a scan with the holders `replace` names, which
    /// the launch stops under `guard` (held whenever `replace` names any) only when
    /// the scan steps; a sweep the source grants stops none (see
    /// [`Inner::launch_replacing`]). Any other kind takes `replace` empty.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn scan_start_replacing(
        &self,
        kind: &str,
        source: SourceId,
        tuning: Tuning,
        graph: Graph,
        capture_rate_hz: Option<Hz>,
        elastic: bool,
        guard: Option<&OwnedMutexGuard<()>>,
        replace: &[JobId],
    ) -> (Result<JobId, NodeError>, AfterGate) {
        if let Err(e) = self.check_watch_policy(&graph, tuning) {
            return (Err(e), AfterGate::Nothing);
        }
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.to_string(),
            graph,
            source,
            tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: capture_rate_hz.map(|r| r.0),
            elastic,
            operator_graph: false,
            owner: None,
        };
        let id = rec.id.clone();
        let (r, after) = self.inner.launch_replacing(rec, guard, replace).await;
        (r.map(|()| id), after)
    }

    /// Run what a gated call left for after the gate (see [`AfterGate`]). Call it
    /// only once the guard is dropped.
    pub(crate) async fn after_gate(&self, after: AfterGate) {
        self.inner.after_gate(after).await;
    }

    /// The kind a job recorded as `kind` was expanded from, when the node expands its
    /// graph; see [`Inner::expandable_kind`].
    pub(crate) fn expandable_kind(&self, kind: &str) -> Option<JobKind> {
        self.inner.expandable_kind(kind)
    }

    /// Idle `port` if nobody holds it; see [`LeaseManager::idle_if_unheld`]. Not
    /// under the source's gate (it takes it).
    pub(crate) async fn idle_if_unheld(&self, port: &dyn Port) {
        self.inner.leases.idle_if_unheld(port).await;
    }

    /// Replace a job's graph, keeping its id. The graph is validated first. A
    /// `Running` job is stopped and relaunched on the new graph; any other job
    /// only has its record rewritten. If the relaunch fails (say, a lease
    /// conflict after the stop) the job is left `Stopped` with the new graph.
    /// The record's `capture_rate_hz` is cleared and `operator_graph` set: the node
    /// cannot know what an operator's graph intends, so a resume launches it as stored.
    /// Except when the job is of a built-in kind the node expanded and `graph` is its
    /// graph with only the params [`JobKind::reexpand`] carries changed (a demodulator's
    /// `squelch`, `squelch_db`, `deviation_hz`, `sideband`): the record then keeps its
    /// rate and stays the node's to re-expand at a later capture.
    pub async fn job_graph_put(&self, id: &JobId, graph: Graph) -> Result<(), NodeError> {
        self.job_graph_set(id, graph, None).await
    }

    /// Whether `graph` is what re-expanding the job `rec` would give, carrying `graph`'s
    /// params: so only carried params differ from the node's graph. Tried at the rate the
    /// record was expanded for (else the source's kept capture) with the source's kept
    /// bank, and without a bank. Makes no bank.
    fn only_carried_params_differ(&self, rec: &JobRecord, graph: &Graph) -> bool {
        let Some(kind) = self.inner.expandable_kind(&rec.kind) else {
            return false;
        };
        if rec.operator_graph || rec.input.is_some() {
            return false;
        }
        let kept = self
            .inner
            .banks
            .lock()
            .unwrap()
            .get(&rec.source)
            .map(|slot| (slot.band, slot.bank.as_ref().map(|b| 2 * b.bin_hz())));
        let Some(rate) = rec
            .capture_rate_hz
            .map(Hz)
            .or(kept.map(|(band, _)| band.bandwidth_hz))
        else {
            return false;
        };
        let bin_rates = [kept.and_then(|(_, b)| b), None];
        bin_rates.into_iter().any(|bin_rate| {
            kind.reexpand(
                &rec.source,
                rec.tuning,
                rate,
                bin_rate,
                graph,
                &self.inner.modes(),
            )
            .is_ok_and(|g| graph::same_pipeline(&g, graph))
        })
    }

    /// [`Node::job_graph_put`] recording `capture_rate_hz` as the rate `graph` was
    /// expanded for (the node's own re-expansion of a built-in kind).
    pub(crate) async fn job_graph_set(
        &self,
        id: &JobId,
        graph: Graph,
        capture_rate_hz: Option<Hz>,
    ) -> Result<(), NodeError> {
        let mut graph = graph;
        crate::ops::fill_squelch_params(&mut graph);
        graph::validate(&graph, &self.inner.registry)?;
        let current = {
            let jobs = self.inner.jobs.lock().unwrap();
            let entry = jobs
                .get(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            entry.record.clone()
        };
        self.check_watch_policy(&graph, current.tuning)?;
        self.inner
            .check_scan_graph(&current.kind, &current.source, current.tuning, &graph)?;
        let running = current.state == JobState::Running;
        // An operator's put of the node's graph with only carried params changed keeps
        // the record's rate (and so stays re-expandable).
        let capture_rate_hz = match capture_rate_hz {
            None if self.only_carried_params_differ(&current, &graph) => {
                Some(current.capture_rate_hz)
            }
            None => None,
            Some(r) => Some(Some(r.0)),
        };
        self.inner
            .swap_graph(id, graph, capture_rate_hz, running)
            .await
    }

    /// Every job record, oldest first.
    pub fn job_list(&self) -> Vec<JobRecord> {
        let mut v: Vec<JobRecord> = self
            .inner
            .jobs
            .lock()
            .unwrap()
            .values()
            .map(|e| e.record.clone())
            .collect();
        v.sort_by(|a, b| {
            a.created_ns
                .0
                .cmp(&b.created_ns.0)
                .then_with(|| a.id.0.cmp(&b.id.0))
        });
        v
    }

    pub fn job_context(&self, id: &JobId) -> Option<serde_json::Value> {
        self.inner
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.record.context.clone())
    }

    /// Shallow JSON merge patch: each top-level key of an object `patch` is set,
    /// or removed when its value is `null`; a non-object `patch` replaces the context.
    pub fn job_context_merge(&self, id: &JobId, patch: serde_json::Value) -> Result<(), NodeError> {
        self.inner.context_merge(id, patch)
    }

    /// The source's IQ history; `None` until its first block arrives.
    pub fn ring(&self, source: &SourceId) -> Option<Arc<RwLock<IqRing>>> {
        self.inner.rings.lock().unwrap().get(source).cloned()
    }

    pub fn bus(&self) -> Arc<StreamBus> {
        self.inner.bus.clone()
    }

    /// Natural job ends that could not be written to disk, one message each.
    pub fn persist_errors(&self) -> Vec<String> {
        self.inner.persist_errors.lock().unwrap().clone()
    }

    /// Transcript tees running (see `Inner::tee_transcripts`), for tests.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn debug_tee_count(&self) -> usize {
        self.inner.tees.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Tasks running for jobs' stages (each stage, IQ forwarder, scan step loop and FFT task);
    /// 0 when no job runs. `GET /health` reports it as `stages_running`.
    pub fn debug_stage_count(&self) -> usize {
        self.inner.stages.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// When `source`'s open fault episode began (see [`recover::Episode`]), if one is
    /// open, for tests of what opens and closes it.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn debug_fault_episode(&self, source: &SourceId) -> Option<u64> {
        self.inner.episode_since(source)
    }

    /// Wait `d` instead of [`crate::ops::SWEEP_START_TIMEOUT_MS`] for a source to
    /// start its own sweep, for tests of a start that never returns.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn debug_set_sweep_start_timeout(&self, d: std::time::Duration) {
        *self.inner.sweep_start_timeout.lock().unwrap() = d;
    }

    /// Hold `source`'s gate, for tests of what waits on it.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn debug_gate(&self, source: &SourceId) -> OwnedMutexGuard<()> {
        self.inner.leases.gate_guard(source).await
    }

    /// Tune `source` to `t` unless a job holds it, under the source's gate the
    /// caller holds (`guard`) or taking it; see [`LeaseManager::tune_if_unheld`].
    /// Also whether it tuned the port: the caller then relaunches the elastic jobs
    /// there ([`Node::relaunch_elastic`]) once the gate is let go.
    pub(crate) async fn tune_if_unheld(
        &self,
        port: &dyn Port,
        t: Tuning,
        guard: Option<&OwnedMutexGuard<()>>,
    ) -> Result<(Tuning, bool), LeaseError> {
        let tuned = match guard {
            Some(g) => self.inner.leases.tune_if_unheld_locked(port, t, g).await,
            None => self.inner.leases.tune_if_unheld_tuned(port, t).await,
        };
        // A tune the faulted port refused: the node has seen the fault.
        if let Err(LeaseError::Port(_)) = &tuned {
            self.inner.note_fault_if_faulted(port.id());
        }
        tuned
    }

    /// What [`Node::hold_band`] of `want` on `port` would do, under the source's gate
    /// the caller holds, with nothing leased or tuned; see [`LeaseManager::decide`].
    // `set_start` decides every source in use before it tunes any.
    pub(crate) fn decide(
        &self,
        port: &dyn Port,
        want: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Decision {
        self.inner.leases.decide(port, want, guard)
    }

    /// Lease `want` on `port` as a rigid request under the source's gate the caller
    /// holds, for a holder that is no job (a fresh id, returned): granted, tuned or a
    /// conflict exactly as a rigid `job_start` of `want` would be (see
    /// [`LeaseManager::ensure_locked`]), so the jobs leased inside it afterwards join its
    /// capture untuned. The lease lives as long as the returned [`BandHold`]: drop it
    /// before the gate goes (it is released however the caller's future ends, a
    /// dropped one included); meanwhile `source_list` may list it among the holders.
    /// Also whether it tuned the port: the caller then relaunches the elastic jobs
    /// ([`Node::after_gate`]).
    pub(crate) async fn hold_band(
        &self,
        port: &dyn Port,
        want: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(BandHold<'_>, Tuning, bool), LeaseError> {
        let holder = JobId::new();
        let held = self
            .inner
            .leases
            .ensure_locked(port, holder.clone(), want, false, guard)
            .await;
        // A tune the faulted port refused: the node has seen the fault.
        if let Err(LeaseError::Port(_)) = &held {
            self.inner.note_fault_if_faulted(port.id());
        }
        held.map(|(t, tuned)| {
            let hold = BandHold {
                node: self,
                id: holder,
            };
            (hold, t, tuned)
        })
    }

    /// Relaunch every running elastic job on `source` on its current capture; see
    /// [`Inner::relaunch_elastic`].
    pub(crate) async fn relaunch_elastic(&self, source: &SourceId) {
        let _ = self.inner.relaunch_elastic(source, None).await;
    }

    /// Where source `id` is: what its port reports itself (`from: status`), else the
    /// operator's `--source-position` entry (`from: configured`), else `None`.
    pub fn source_position(&self, id: &SourceId) -> Option<SourcePosition> {
        self.inner.source_position(id)
    }

    /// The node's station, when one is set.
    pub fn station(&self) -> Option<crate::station::Station> {
        self.inner.station.lock().unwrap().clone()
    }

    /// Store `s` as the station: written to `station.json` first, then kept in memory;
    /// a failed write leaves the old one. Jobs started afterwards see it.
    pub async fn set_station(
        &self,
        s: crate::station::Station,
    ) -> Result<crate::station::Station, NodeError> {
        crate::station::persist(&self.inner.state_dir, &s)?;
        *self.inner.station.lock().unwrap() = Some(s.clone());
        Ok(s)
    }

    /// Forget the station and remove `station.json`.
    pub async fn clear_station(&self) -> Result<(), NodeError> {
        crate::station::remove(&self.inner.state_dir)?;
        *self.inner.station.lock().unwrap() = None;
        Ok(())
    }

    /// Hold `source` at `want` without a job: a rigid lease under
    /// [`crate::lease::pin_holder`] that is never listed among the holders. Under
    /// one hold of the source's gate the `replace` jobs are stopped (see
    /// [`Node::replace_holders`]), any existing pin is set aside and the band is
    /// leased as a rigid request; a conflict keeps the old pin. The pin is then
    /// persisted in `sources/<id>.json`; a write that fails releases it and is
    /// `Io`. A tune relaunches the elastic jobs there once the gate is let go.
    /// The result is the capture, as `source_ensure`'s.
    pub async fn source_pin(
        &self,
        source: &SourceId,
        want: Tuning,
        replace: &[JobId],
    ) -> Result<Tuning, NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        let guard = self.inner.leases.gate_guard(source).await;
        let leased = match self.replace_holders(source, replace, &guard).await {
            Ok(()) => self
                .inner
                .leases
                .pin_locked(port.as_ref(), want, &guard)
                .await
                .map_err(NodeError::Lease),
            Err(e) => Err(e),
        };
        let (result, tuned) = match leased {
            Ok((got, tuned)) => match self.set_source_record(source, |r| r.pin = Some(want)) {
                Ok(()) => (Ok(got), tuned),
                Err(e) => {
                    self.inner.leases.unpin(source);
                    (Err(e), tuned)
                }
            },
            Err(e) => (Err(e), false),
        };
        drop(guard);
        // Outside the gate: the relaunched jobs' leases and the idle take it.
        if tuned {
            self.relaunch_elastic(source).await;
        }
        if let Err(e) = &result {
            // The `replace` jobs were stopped without idling the port (as for
            // `job_start`), and a pin released on a failed write held it: idle it
            // if nobody holds it now. A refused id stopped nothing.
            let stopped =
                !replace.is_empty() && !matches!(e, NodeError::UnknownJob(_) | NodeError::Other(_));
            if stopped || matches!(e, NodeError::Io(_)) {
                self.idle_if_unheld(port.as_ref()).await;
            }
        }
        result
    }

    /// Release `source`'s pin and persist `pin: null` under the source's gate, then
    /// idle the port if nothing else holds it. `Other("source <id> is not pinned")` when it has none; a
    /// write that fails is `Io`, the pin released all the same.
    pub async fn source_unpin(&self, source: &SourceId) -> Result<(), NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        // The release and the write under one hold of the gate, as `source_pin`'s
        // lease and write, so a racing pin cannot leave disk and memory disagreeing.
        let guard = self.inner.leases.gate_guard(source).await;
        if self.inner.leases.unpin(source).is_none() {
            return Err(NodeError::Other(format!(
                "source {} is not pinned",
                source.0
            )));
        }
        let written = self.set_source_record(source, |r| r.pin = None);
        drop(guard);
        // Outside the gate: the idle takes it.
        self.idle_if_unheld(port.as_ref()).await;
        written
    }

    /// Drop `source`'s cached rate figures and have its worker measure the link
    /// again ([`Port::remeasure`]), under one hold of the source's gate: a source
    /// with no worker is `Other("source <id> has no worker")`; one a rigid or
    /// time-shared lease or a pin holds, other than the `replace` jobs, is the
    /// conflict naming them ([`LeaseManager::unheld_locked`]); then the `replace`
    /// jobs are stopped ([`Node::replace_holders`]), the record's figures are no
    /// longer measurements (its `capabilities` kept with every measured cap
    /// asserted, [`sources::unmeasured`], and `measured_at_speed` cleared, persisted:
    /// a start before the new hello probes, and still knows the source when the
    /// device is away), and the port respawns its worker. The
    /// heal that follows the new hello relaunches what failed and caches the new
    /// figures with their speed ([`Inner::note_capabilities`]).
    pub async fn source_measure(
        &self,
        source: &SourceId,
        replace: &[JobId],
    ) -> Result<(), NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        let guard = self.inner.leases.gate_guard(source).await;
        if port.worker().is_none() {
            return Err(NodeError::Other(format!(
                "source {} has no worker",
                source.0
            )));
        }
        self.inner
            .leases
            .unheld_locked(port.as_ref(), replace, &guard)
            .map_err(NodeError::Lease)?;
        self.replace_holders(source, replace, &guard).await?;
        let result = match self.set_source_record(source, |r| {
            r.capabilities = r.capabilities.take().map(sources::unmeasured);
            r.measured_at_speed = None;
        }) {
            Ok(()) => port.remeasure().await.map_err(gain_error),
            Err(e) => Err(e),
        };
        drop(guard);
        // The `replace` jobs were stopped without idling the port (as for
        // `source_pin`); after a failure nothing respawns it, so idle it here.
        if result.is_err() && !replace.is_empty() {
            self.idle_if_unheld(port.as_ref()).await;
        }
        result
    }

    /// Set `source`'s overall receive gain (`None`: automatic gain) through
    /// [`Port::set_gain`] and persist it in `sources/<id>.json`, to be re-applied
    /// when the node opens. A refusal is `Other` with the port's text; a write
    /// that fails is `Io`, the gain applied all the same.
    pub async fn source_set_gain(
        &self,
        source: &SourceId,
        db: Option<f64>,
    ) -> Result<GainInfo, NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        let got = port.set_gain(db).await.map_err(gain_error)?;
        self.set_source_record(source, |r| r.gain = Some(GainSetting { db }))?;
        Ok(got)
    }

    /// The band `source` is pinned at, if it is.
    pub fn pinned(&self, source: &SourceId) -> Option<Tuning> {
        self.inner.leases.pinned(source)
    }

    /// Change `source`'s record with `change` and write it; on a failed write the
    /// record in memory is left as it was.
    fn set_source_record(
        &self,
        source: &SourceId,
        change: impl FnOnce(&mut SourceRecord),
    ) -> Result<(), NodeError> {
        let mut sources = self.inner.sources.lock().unwrap();
        let mut rec = sources
            .get(source)
            .cloned()
            .unwrap_or_else(SourceRecord::new);
        rec.format = SOURCE_RECORD_FORMAT;
        change(&mut rec);
        persist_source(&self.inner.sources_dir, source, &rec)?;
        sources.insert(source.clone(), rec);
        Ok(())
    }

    /// Jobs currently holding a rigid lease on `source`, with the band each asked for.
    pub fn lease_holders(&self, source: &SourceId) -> Vec<(JobId, Tuning)> {
        self.inner.leases.holders(source)
    }

    /// Jobs currently following `source`'s capture on an elastic lease.
    pub fn lease_elastic(&self, source: &SourceId) -> Vec<JobId> {
        self.inner.leases.elastic_holders(source)
    }

    /// Scans holding a sweep claim on `source`: the source sweeps for them by its
    /// own means, and they hold no band.
    pub fn lease_sweepers(&self, source: &SourceId) -> Vec<JobId> {
        self.inner.leases.sweepers(source)
    }

    /// `source_free`: the sources, in id order, that could take the band now
    /// ([`LeaseManager::free_sources`]: a pin over the whole band admits it), and every
    /// other source that covers it with its holders, elastic jobs, pin and fault.
    /// Sources that cannot cover the band are in neither list.
    pub fn source_free(
        &self,
        center_hz: u64,
        bandwidth_hz: u64,
    ) -> (Vec<SourceId>, Vec<crate::ops::BusySource>) {
        let mut ports: Vec<Arc<dyn Port>> =
            self.inner.ports.read().unwrap().values().cloned().collect();
        ports.sort_by(|a, b| a.id().0.cmp(&b.id().0));
        let free = self
            .inner
            .leases
            .free_sources(&ports, center_hz, bandwidth_hz);
        let busy = ports
            .iter()
            .filter(|p| crate::lease::covers(p.capabilities(), center_hz, bandwidth_hz))
            .filter(|p| !free.contains(p.id()))
            .map(|p| crate::ops::BusySource {
                id: p.id().clone(),
                holders: self
                    .lease_holders(p.id())
                    .into_iter()
                    .map(|(j, _)| j)
                    .collect(),
                elastic: self.lease_elastic(p.id()),
                pin: self.pinned(p.id()),
                fault: p.fault(),
            })
            .collect();
        (free, busy)
    }
}

impl Inner {
    /// Check the jobs `replace` names under `source`'s gate (`guard`), stopping none:
    /// `UnknownJob`, or `Other("job <id> does not hold source <src>")` for a job
    /// without a rigid lease on `source` (an elastic job never holds one).
    pub(crate) fn check_replace(
        &self,
        source: &SourceId,
        replace: &[JobId],
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(), NodeError> {
        self.leases.assert_gate(source, guard);
        for id in replace {
            if !self.jobs.lock().unwrap().contains_key(id) {
                return Err(NodeError::UnknownJob(id.clone()));
            }
            if !self.leases.holders(source).iter().any(|(j, _)| j == id) {
                return Err(NodeError::Other(format!(
                    "job {} does not hold source {}",
                    id.0, source.0
                )));
            }
        }
        Ok(())
    }

    /// Stop the jobs `replace` names, under `source`'s gate (`guard`), so the
    /// request that follows under the same guard finds the radio as they left
    /// it. Every id is checked first ([`Inner::check_replace`]), and nothing is
    /// stopped unless all pass. Each is then stopped as by [`Node::job_stop`]
    /// (persisted `Stopped`, streams unregistered) but without idling the port,
    /// and its lease is gone when this returns. A stop that cannot be persisted
    /// is `Io`, that job left running.
    pub(crate) async fn replace_holders(
        &self,
        source: &SourceId,
        replace: &[JobId],
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(), NodeError> {
        self.check_replace(source, replace, guard)?;
        let holds = |id: &JobId| self.leases.holders(source).iter().any(|(j, _)| j == id);
        let mut seen = Vec::new();
        for id in replace {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            self.stop_gated(id).await?;
            // Another stop may have taken the supervisor, or a restart (`graph_put`)
            // may hold the new lease on its way to the launch `record_running`
            // refuses: either release is near (nothing before it takes the gate),
            // and the request must not see the lease. Bounded, so a stage that
            // never ends cannot keep the source's gate.
            let deadline = tokio::time::Instant::now() + REPLACE_RELEASE_TIMEOUT;
            while holds(id) {
                if tokio::time::Instant::now() >= deadline {
                    return Err(NodeError::Other(format!(
                        "job {} did not release source {}",
                        id.0, source.0
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, input: Option<JobId>, owner: Option<JobId>) -> JobRecord {
        JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.into(),
            graph: if kind == "mix" {
                crate::ops::mix_graph(None)
            } else {
                crate::graph::Graph {
                    id: crate::graph::GraphId("g".into()),
                    nodes: vec![],
                    edges: vec![],
                }
            },
            source: SourceId("a".into()),
            tuning: Tuning {
                center_hz: Hz(476_625_000),
                bandwidth_hz: Hz(12_500),
            },
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: Timestamp(0),
            input,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner,
        }
    }

    #[test]
    fn mixable_refuses_attached_owned_and_mix_records() {
        assert!(mixable(&rec("fm_demod", None, None)));
        assert!(!mixable(&rec("transcribe", Some(JobId::new()), None)));
        assert!(!mixable(&rec("fm_demod", None, Some(JobId::new()))));
        assert!(!mixable(&rec("mix", None, None)));
        let mut graph_mix = rec("graph", None, None);
        graph_mix.graph = crate::ops::mix_graph(None);
        assert!(!mixable(&graph_mix));
    }

    #[test]
    fn mixable_refuses_a_job_on_a_no_voice_plan_channel() {
        let mut ch22 = rec("fm_demod", None, None);
        ch22.tuning.center_hz = Hz(476_950_000);
        assert!(!mixable(&ch22));
        let mut ch20 = rec("fm_demod", None, None);
        ch20.tuning.center_hz = Hz(476_900_000);
        assert!(mixable(&ch20));
    }
}
