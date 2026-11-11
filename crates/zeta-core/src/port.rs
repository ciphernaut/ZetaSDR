//! The port: the boundary every signal source sits behind.

use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use crate::types::{Capabilities, Claim, Depth, Hz, SourceId, Span, Timestamp};

/// A centre frequency and the bandwidth captured around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Tuning {
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
}

/// A source's overall receive gain: `db` in decibels (`null` while automatic
/// gain is on), the range a value may take, and whether automatic gain is on and
/// available at all.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct GainInfo {
    /// The gain in dB; `null` while automatic gain is on.
    #[serde(deserialize_with = "nullable_f64")]
    #[schemars(required, schema_with = "crate::types::nullable::<f64>")]
    pub db: Option<f64>,
    pub min_db: f64,
    pub max_db: f64,
    /// Whether the driver's automatic gain is on.
    pub automatic: bool,
    /// Whether the device has automatic gain (`gain_db: null` is accepted).
    pub automatic_available: bool,
}

/// Deserialize a required `Option<f64>`: present, a number or `null`.
pub(crate) fn nullable_f64<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<f64>, D::Error> {
    <Option<f64> as Deserialize>::deserialize(d)
}

/// A streaming source's health counters since it opened: the sample rate
/// actually delivered over the last 10 s, to the hertz (`null` before two
/// blocks arrived), reads that overflowed (samples lost in the driver), reads
/// that timed out, stream restarts (a stream whose stamps ran ahead of the
/// wall clock), and tune gaps: overflows the reader's own blocked tune caused
/// (it could not read while the device retuned), which fall among the samples
/// a tune discards anyway and so are not in `overflows`. `tune_gaps` defaults
/// when absent, so the stats of an older worker still load; every source of
/// this version writes it, so the schema requires it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
pub struct PortStats {
    #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
    pub measured_rate_hz: Option<u64>,
    pub overflows: u64,
    pub timeouts: u64,
    pub restarts: u64,
    #[serde(default)]
    pub tune_gaps: u64,
}

/// The worker process behind a source, as `source_list` shows it: the live
/// worker's pid (`null` while none is running: dead, respawning or waiting for
/// the device), how many times a worker was spawned after the first, and when
/// the live worker said hello, in ns since the UNIX epoch (`null` while none
/// is running). `transport` is how the live worker hands its blocks over, as
/// its hello said (`null` from a worker of protocol 1, which sends them on
/// the pipe, or while none is running); `transport_dropped` counts the blocks
/// the live worker dropped because the node had not freed a ring slot (or its
/// pipe queue) in time. `starting` is what a worker not yet live is doing at
/// its start (see [`Starting`]). `attachment` is where the live worker's device
/// is attached, as its hello said (`usb:2-1.4@5000`: the bus and port path,
/// then the negotiated speed; `null` while none is running or where the source
/// cannot tell); `link_measure` the figures of the latest hello and the link speed
/// they were measured at (see [`LinkMeasure`]): what the node caches for the
/// source's next start ([`Port::record_to_cache`]), `null` before the first
/// hello. These default when absent, so a source record of an older node still
/// loads; every node of this version writes them, so the schema requires them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = crate::knowledge::require_all)]
pub struct WorkerInfo {
    #[schemars(required, schema_with = "crate::types::nullable::<u32>")]
    pub pid: Option<u32>,
    pub restarts: u64,
    #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
    pub since_ns: Option<u64>,
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<TransportInfo>")]
    pub transport: Option<TransportInfo>,
    #[serde(default)]
    pub transport_dropped: u64,
    /// What a worker that has not yet said hello is doing at its start, while it
    /// says (a respawn measuring its link's rates: up to a minute or more, and the
    /// host is busy meanwhile); `null` otherwise, and once it said hello or ended.
    /// Defaults when absent, so a record of an older node still loads.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<Starting>")]
    pub starting: Option<Starting>,
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<String>")]
    pub attachment: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<LinkMeasure>")]
    pub link_measure: Option<LinkMeasure>,
}

/// A hello's measured figures and the link speed they were measured at: its
/// `max_bandwidth_hz` (the source's own depth's cap) and `depths`, as
/// [`Capabilities`] names them, and `at_speed`, the [`link_speed`] of its
/// `attachment` (`null` where the source cannot tell).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct LinkMeasure {
    pub max_bandwidth_hz: Claim<Hz>,
    pub depths: Vec<Depth>,
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    pub at_speed: Option<String>,
}

/// The negotiated speed in an `attachment` (`usb:2-1.4@5000` gives `5000`): the
/// part after the `@`; `None` without one.
pub fn link_speed(attachment: &str) -> Option<&str> {
    attachment
        .rsplit_once('@')
        .map(|(_, speed)| speed)
        .filter(|s| !s.is_empty())
}

/// Whether figures measured at link speed `measured_at_speed` still hold for a
/// device now at `attachment`: true unless both speeds are known and differ (no
/// speed in the record, no attachment or no speed in it, keeps them). The port
/// path is shown, never compared: another port at the same speed keeps them.
pub fn cached_figures_hold(measured_at_speed: Option<&str>, attachment: Option<&str>) -> bool {
    match (measured_at_speed, attachment.and_then(link_speed)) {
        (Some(was), Some(now)) => was == now,
        _ => true,
    }
}

/// The start of a [`Starting::detail`] whose probe runs because the link's
/// speed changed: `link speed changed (<old> to <new>): `.
pub fn speed_changed_prefix(old: &str, new: &str) -> String {
    format!("link speed changed ({old} to {new}): ")
}

/// The start of a [`Starting::detail`] whose probe runs because `source_measure`
/// asked for it.
pub const AS_ASKED_PREFIX: &str = "as asked: ";

/// Why a start measures, from its `detail`: `link speed changed (<old> to
/// <new>)` or `as asked`, or `None` (a first measure says nothing).
fn measure_reason(detail: &str) -> Option<&str> {
    if detail.starts_with(AS_ASKED_PREFIX) {
        return Some(AS_ASKED_PREFIX.trim_end_matches(": "));
    }
    let rest = detail.strip_prefix("link speed changed (")?;
    let end = rest.find("): ")?;
    Some(&detail[..detail.len() - rest.len() + end + 1])
}

/// A worker's start in progress, as it reports it before its hello: `what` it is
/// doing (`measuring`: the rate probe of its link's sample depths), `detail` the
/// step that follows (`12-bit at 30720000 Hz`: the depth and the rate the probe
/// streams next), `step` of `of` (`of` the most steps it may take, which never
/// rises; a probe that finds a depth's clean rate early takes fewer), and `eta_ms`
/// the most time left, every step retried.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Starting {
    pub what: String,
    pub detail: String,
    pub step: u32,
    pub of: u32,
    pub eta_ms: u64,
}

/// The line the node logs, and puts on the source's feed, when `source`'s worker
/// begins measuring its link: `s` is its first progress report, whose `eta_ms`
/// (whole seconds, rounded up) bounds the wait. A measure that has a reason
/// names it first in the parentheses: `link speed changed (<old> to <new>)` or
/// `as asked` (`source_measure`), from the start of `s.detail`.
pub fn measuring_line(source: &SourceId, s: &Starting) -> String {
    let why = measure_reason(&s.detail).map_or(String::new(), |r| format!("{r}; "));
    format!(
        "source {}: measuring what this radio's link can sustain ({why}up to about {} s; the host will be busy; max_rate_hz= skips it)",
        source.0,
        s.eta_ms.div_ceil(1000)
    )
}

/// The line the node logs, and puts on the source's feed, when `source`'s worker
/// said hello after measuring for `took` (whole seconds, rounded to nearest).
pub fn measured_line(source: &SourceId, took: std::time::Duration) -> String {
    format!(
        "source {}: measured in {} s",
        source.0,
        took.as_secs_f64().round() as u64
    )
}

/// How a worker child hands its blocks to the node: `shm` is a shared-memory
/// ring the node maps (the wire carries only descriptors), `pipe` carries the
/// samples in the frames themselves. `format` is the sample format the blocks
/// travel in: `cf32`, or on the ring `sc16` from a device whose native format
/// is 16-bit (half the bytes, so the ring holds twice the time, converted at
/// the node by [`sc16_to_cf32`]); `ring_bytes` is the ring's size in bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TransportInfo {
    pub path: TransportPath,
    pub format: String,
    pub ring_bytes: u64,
}

/// The path a worker's blocks take to the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransportPath {
    Shm,
    Pipe,
}

/// A block of complex baseband samples.
#[derive(Clone, Debug)]
pub struct IqBlock {
    pub t0: Timestamp,
    pub sample_rate_hz: Hz,
    pub center_hz: Hz,
    pub samples: Arc<[Complex<f32>]>,
}

pub type IqReceiver = tokio::sync::broadcast::Receiver<IqBlock>;

/// A block of complex 16-bit samples as a device whose native format is
/// `CS16` gives them, before conversion: what a source worker writes to its
/// ring unconverted. [`sc16_to_cf32`] turns it into an [`IqBlock`]'s samples.
#[derive(Clone, Debug)]
pub struct Sc16Block {
    pub t0: Timestamp,
    pub sample_rate_hz: Hz,
    pub center_hz: Hz,
    pub samples: Arc<[Complex<i16>]>,
}

pub type Sc16Receiver = tokio::sync::broadcast::Receiver<Sc16Block>;

/// The one conversion of 16-bit samples to `cf32`: each part over 32768.0, so
/// full scale is -1.0 to 32767/32768. A source that converts in its own
/// process and a node that converts a worker's ring use this function alike,
/// so a capture is bit-identical either way. Converts the first
/// `min(x.len(), out.len())` samples, each by [`sc16_sample_to_cf32`].
pub fn sc16_to_cf32(x: &[Complex<i16>], out: &mut [Complex<f32>]) {
    for (o, s) in out.iter_mut().zip(x) {
        *o = sc16_sample_to_cf32(*s);
    }
}

/// One sample of [`sc16_to_cf32`], for a caller that converts into memory it
/// has not filled yet (the node's ring reader, collecting straight into the
/// block's `Arc`).
#[inline]
pub fn sc16_sample_to_cf32(s: Complex<i16>) -> Complex<f32> {
    Complex::new(f32::from(s.re) / 32768.0, f32::from(s.im) / 32768.0)
}

/// What a scan asks a source that sweeps for itself ([`Port::sweep`]): power
/// spectra of `span`, at bins of `bin_hz` or the nearest the source can give,
/// each row averaged over `dwell_ms` (0: one look a step).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SweepRequest {
    pub span: Tuning,
    pub bin_hz: Hz,
    pub dwell_ms: u64,
}

/// How the source chose to sweep a [`SweepRequest`]: `steps` views a sweep,
/// `step_hz` apart, each a row of `size` bins of `bin_hz`; `dwell_ms` is the
/// dwell in force, and `shift_hz` how far the odd sweeps place each view
/// lower than the even ones (0: no shift).
#[derive(Clone, Debug, PartialEq)]
pub struct SweepPlan {
    pub steps: u32,
    pub step_hz: Hz,
    pub size: u32,
    pub bin_hz: f64,
    pub dwell_ms: u64,
    pub shift_hz: Hz,
}

/// One view of a source's own sweep: step `step` of sweep `sweep` (counted
/// from 0 at the sweep's start, and from 0 again at each re-aim,
/// [`SweepRows::retarget`]), centred on `center_hz`, `db` the power of
/// each of [`SweepPlan::size`] bins of `bin_hz` from the low edge up, `masked`
/// the `[lo, hi)` bin ranges not to be read as signal, and `span` when its
/// samples were taken.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepRow {
    pub sweep: u64,
    pub step: u32,
    pub center_hz: Hz,
    pub bin_hz: f64,
    pub db: Vec<f32>,
    pub masked: Vec<[usize; 2]>,
    pub span: Span,
}

/// Where a source's own sweep is: delivering rows, `Waiting` while the source
/// retries what it lost (the reason), or `Ended` for good (the reason), which
/// the source sets before it closes the rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SweepState {
    Running,
    Waiting(String),
    Ended(String),
}

/// A sweep a source runs for itself: the plan it chose, its rows (at most
/// [`SWEEP_ROWS_CAPACITY`] waiting), its state and, for a source that can re-aim
/// a running sweep, its `control` ([`SweepRows::retarget`]; `None`: it cannot).
#[derive(Debug)]
pub struct SweepRows {
    pub plan: SweepPlan,
    pub rows: mpsc::Receiver<SweepRow>,
    pub state: watch::Receiver<SweepState>,
    pub control: Option<SweepControl>,
}

impl SweepRows {
    /// Re-aim the running sweep at `req` (a new span, bin width and dwell) without
    /// ending it: the source re-plans on what it already holds (the Kiwi's open
    /// `W/F` socket) and answers with the new plan, or refuses with the reason
    /// (a span it cannot sweep is `OutOfRange`, as [`Port::sweep`] refuses it), the
    /// sweep then going on as it was. A source that cannot re-aim (`control`
    /// `None`) is `Unsupported`: the caller ends the sweep and asks for a new one.
    ///
    /// Contract: every row the source sends after its answer belongs to the new
    /// plan (its `sweep` and `step` counted from 0 again), and every row it sent
    /// before belongs to the old one, so a reader that drops the rows still queued
    /// when the answer arrives never reads a row of the old plan as the new one's.
    /// A row still holds only pixels of its own view. Called under the source's
    /// gate, like [`Port::sweep`]: answer within a few seconds.
    pub async fn retarget(&self, req: SweepRequest) -> Result<SweepPlan, PortError> {
        match &self.control {
            Some(c) => c.retarget(req).await,
            None => Err(PortError::Unsupported(SWEEP_NO_RETARGET.into())),
        }
    }
}

/// Why a sweep cannot be re-aimed: its source has no [`SweepControl`].
pub const SWEEP_NO_RETARGET: &str = "source cannot re-aim a running sweep";

/// One ask to re-aim a running sweep ([`SweepRows::retarget`]): the new request
/// and where the source answers it.
#[derive(Debug)]
pub struct SweepRetarget {
    pub req: SweepRequest,
    pub reply: oneshot::Sender<Result<SweepPlan, PortError>>,
}

/// The handle that re-aims a running sweep: [`SweepControl::channel`] gives the
/// source the receiving end, which its sweep reads while it runs, and puts this
/// end in its [`SweepRows`]. A sweep that has ended answers `Io`.
#[derive(Clone, Debug)]
pub struct SweepControl(mpsc::Sender<SweepRetarget>);

impl SweepControl {
    /// A control and the receiver its sweep reads.
    pub fn channel() -> (SweepControl, mpsc::Receiver<SweepRetarget>) {
        let (tx, rx) = mpsc::channel(1);
        (SweepControl(tx), rx)
    }

    /// Ask the sweep to re-aim at `req` and wait for its answer (see
    /// [`SweepRows::retarget`]).
    pub async fn retarget(&self, req: SweepRequest) -> Result<SweepPlan, PortError> {
        let ended = || PortError::Io("the sweep has ended".into());
        let (reply, answer) = oneshot::channel();
        self.0
            .send(SweepRetarget { req, reply })
            .await
            .map_err(|_| ended())?;
        answer.await.map_err(|_| ended())?
    }
}

/// The rows a source's sweep holds for a reader that has not taken them; a
/// row for a full channel is dropped.
pub const SWEEP_ROWS_CAPACITY: usize = 64;

/// A port's refusal. It serialises through [`PortErrorWire`] (`kind` names the
/// variant) so a worker process can hand it to the node unchanged; `Display`
/// is the same text after the trip.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PortError {
    /// A `suggested` above `max` means the source itself ships more than the
    /// configured cap allows (a remote node's fixed rate); the message says so.
    #[error("{field} {value} out of range {min}..={max}{}", over_cap(*suggested, *max))]
    OutOfRange {
        field: &'static str,
        value: u64,
        min: u64,
        max: u64,
        suggested: Option<u64>,
    },
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("io: {0}")]
    Io(String),
}

/// The JSON shape of a [`PortError`]: `{"kind": "out_of_range", "field", "value",
/// "min", "max", "suggested"}`, `{"kind": "unsupported", "message"}` or
/// `{"kind": "io", "message"}`. It exists because `OutOfRange::field` is a
/// `&'static str`, which serde cannot fill from data without help.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PortErrorWire {
    OutOfRange {
        field: String,
        value: u64,
        min: u64,
        max: u64,
        suggested: Option<u64>,
    },
    Unsupported {
        message: String,
    },
    Io {
        message: String,
    },
}

// Written by hand: a derive would see `&'static str` in `OutOfRange` and
// demand input that lives for `'static`.
impl Serialize for PortError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        PortErrorWire::from(self.clone()).serialize(s)
    }
}

impl<'de> Deserialize<'de> for PortError {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        PortErrorWire::deserialize(d).map(PortError::from)
    }
}

impl From<PortError> for PortErrorWire {
    fn from(e: PortError) -> Self {
        match e {
            PortError::OutOfRange {
                field,
                value,
                min,
                max,
                suggested,
            } => PortErrorWire::OutOfRange {
                field: field.to_string(),
                value,
                min,
                max,
                suggested,
            },
            PortError::Unsupported(message) => PortErrorWire::Unsupported { message },
            PortError::Io(message) => PortErrorWire::Io { message },
        }
    }
}

impl From<PortErrorWire> for PortError {
    fn from(w: PortErrorWire) -> Self {
        match w {
            PortErrorWire::OutOfRange {
                field,
                value,
                min,
                max,
                suggested,
            } => PortError::OutOfRange {
                // The names ports use map back to their literals; any other
                // is leaked, a few bytes each time such an error is received
                // from a peer (rare: every port today uses those two names).
                field: match field.as_str() {
                    "center_hz" => "center_hz",
                    "bandwidth_hz" => "bandwidth_hz",
                    _ => Box::leak(field.into_boxed_str()),
                },
                value,
                min,
                max,
                suggested,
            },
            PortErrorWire::Unsupported { message } => PortError::Unsupported(message),
            PortErrorWire::Io { message } => PortError::Io(message),
        }
    }
}

/// The hint for an `OutOfRange` whose suggestion exceeds its maximum: the
/// source ships that rate and only a larger cap admits it.
pub fn over_cap(suggested: Option<u64>, max: u64) -> String {
    match suggested {
        Some(s) if s > max => {
            format!("; remote ships {s} Hz; pass max_bandwidth_hz={s} to allow it")
        }
        _ => String::new(),
    }
}

/// A signal source.
///
/// Contract: `tune` and `current` return the capture band, whose
/// `bandwidth_hz` equals the complex sample rate of the blocks the port emits;
/// a port's producer exits when it has no subscribers or is dropped;
/// consecutive blocks have `t0` rising by exactly
/// `samples · 1e9 / sample_rate_hz` (integer nanoseconds) unless samples were
/// really lost, in which case `t0` jumps forward and never backward;
/// unrecoverable failures are reported through [`Port::fault`], never by
/// panicking or by closing the stream silently.
#[async_trait]
pub trait Port: Send + Sync {
    fn id(&self) -> &SourceId;
    fn capabilities(&self) -> &Capabilities;
    /// After `tune` returns a new centre, the first block the port delivers with that centre holds only samples taken after the tune settled.
    ///
    /// When a tune has settled is the port's own knowledge (a device's settle
    /// time, a remote's latency), so a consumer never discards samples after a
    /// tune itself; each port says in its `tune` how it meets this.
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError>;
    fn current(&self) -> Option<Tuning>;
    fn subscribe(&self) -> IqReceiver;
    /// The port's unrecoverable failure, if any (a remote receiver that is
    /// full or gone, a device that was unplugged). Jobs reading the port fail
    /// with it as their reason. A successful `tune` clears it.
    fn fault(&self) -> Option<String> {
        None
    }
    /// Called under the lease gate when the source's last holder released; a
    /// port may stop upstream work and clear `current`. The next `tune` must
    /// work as from scratch. Must not block for long: the gate is held.
    async fn idle(&self) {}
    /// The overall receive gain, if the source has an adjustable one.
    fn gain(&self) -> Option<GainInfo> {
        None
    }
    /// The port's stream statistics, if it keeps any (a device stream does; a
    /// recording or a remote receiver does not).
    fn stats(&self) -> Option<PortStats> {
        None
    }
    /// Set the overall receive gain to `db` dB, or turn the driver's automatic
    /// gain on with `None`; the result is the gain after the change. A refusal
    /// (no adjustable gain, no automatic gain, a value out of range) is
    /// `Unsupported` with the text the caller sees.
    async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        let _ = db;
        Err(PortError::Unsupported(
            "source has no adjustable gain".into(),
        ))
    }
    /// A counter the port bumps each time it recovers from a fault on its own
    /// (a respawned worker that found the device again), so the node can
    /// restore the source's pin, gain and jobs. `None` for a port that never
    /// heals by itself: its faults clear only on the operator's next tune.
    fn healed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
    /// The node has seen every job of the fault the last heal ended, and is about to
    /// relaunch them. A port that bumps [`Port::healed`] may keep reporting [`Port::fault`]
    /// until this call, so a fault shorter than a job's poll of it is not missed.
    fn heal_settled(&self) {}
    /// The worker process behind the port, for a port that runs its device in
    /// a child process; `None` for every other port.
    fn worker(&self) -> Option<WorkerInfo> {
        None
    }
    /// A counter the port bumps each time its [`Port::worker`] record's `starting`
    /// changes (a start begun, a step taken, the start over), so the node can say
    /// what a worker is doing before its hello. `None` for a port with no such
    /// reports.
    fn starting_changed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
    /// What the port itself knows about where it is; never the configured flag.
    fn position(&self) -> Option<crate::types::SourcePosition> {
        None
    }
    /// The sample depth in force on the source's link, one of
    /// [`Capabilities::depths`], with its rate cap; `None` for a source with
    /// one depth. A port bounds `tune`'s bandwidth by its cap, which may differ
    /// from the record's top-level `max_bandwidth_hz` (the source's own
    /// depth's figure, fixed for the run).
    fn depth(&self) -> Option<Depth> {
        None
    }
    /// Run the source's link at `bits` per sample, one of
    /// [`Capabilities::depths`]; the result is the depth in force after it.
    /// The stream is closed and `current` cleared, so the caller tunes next (a
    /// depth is fixed when a stream is set up). A depth the source does not
    /// list is `Unsupported` naming the ones it does.
    async fn set_depth(&self, bits: u8) -> Result<Depth, PortError> {
        let _ = bits;
        Err(PortError::Unsupported("source has one sample depth".into()))
    }
    /// The capability record to cache for the source's next start, and the link
    /// speed its figures were measured at (`None`: unknown): the record of
    /// [`Port::capabilities`], unless the port has measured since what that
    /// record, fixed for the run, could not take (a worker's respawn that
    /// measured a depth the record holds as asserted, or measured again at a
    /// new link speed). The two are read together and cached together, so
    /// figures are never kept beside a speed they were not measured at.
    fn record_to_cache(&self) -> (Capabilities, Option<String>) {
        (self.capabilities().clone(), None)
    }
    /// Where the source's device is attached, where it can tell: a USB radio's
    /// bus and port path and its negotiated speed (`usb:2-1.4@5000`, see
    /// [`link_speed`]); `None` for every other source.
    fn attachment(&self) -> Option<String> {
        None
    }
    /// Drop the source's measured figures and measure its link again: a worker
    /// port ends its worker and spawns one that probes, faulted until its hello.
    /// `Unsupported` for a source with no worker.
    async fn remeasure(&self) -> Result<(), PortError> {
        Err(PortError::Unsupported("source has no worker".into()))
    }
    /// Sweep `req.span` by the source's own means (a source whose
    /// [`Capabilities::sweep`] holds the span), beside its IQ stream and without
    /// retuning it. The source chooses its own stepping and the node never
    /// learns how: [`SweepRows::plan`] says what it chose, `dwell_ms` the dwell
    /// in force and `shift_hz` the odd sweeps' shift (0: none).
    ///
    /// Contract: each [`SweepRow`]'s `db` is `plan.size` values, in dBm when
    /// the capability says `calibrated`; `masked` is `[lo, hi)` bin pairs; a
    /// row holds only samples taken in its own view, after the view settled (as
    /// [`Port::tune`]'s first block after a tune does); a row for a full
    /// channel is dropped, never queued. `state` is `Waiting` (with the reason)
    /// while the source retries what it lost, and `Ended` (with the reason)
    /// before the rows close. Dropping the [`SweepRows`] ends the sweep and
    /// frees what it held upstream. What the source cannot do at once (no
    /// view free beside its IQ session) it refuses here, with the reason; the
    /// default refuses every sweep. It is called under the source's gate:
    /// refuse or return within a few seconds (the node gives up on it after
    /// [`crate::ops::SWEEP_START_TIMEOUT_MS`] and drops the call).
    ///
    /// A source that can re-aim the running sweep without ending it (a new span,
    /// bin width or dwell on what it already holds) gives its rows a
    /// [`SweepControl`] ([`SweepRows::retarget`], whose contract is this one's:
    /// called under the source's gate, answered within a few seconds); without one
    /// the node ends the sweep and asks for a new one. A plan of one view does not
    /// alternate (`shift_hz` 0): the view stays put and its rows come as fast as
    /// the source makes them.
    async fn sweep(&self, req: SweepRequest) -> Result<SweepRows, PortError> {
        let _ = req;
        Err(PortError::Unsupported(
            "source does not sweep for itself".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port that does not override `sweep`.
    struct Plain(SourceId, Capabilities);

    #[async_trait]
    impl Port for Plain {
        fn id(&self) -> &SourceId {
            &self.0
        }
        fn capabilities(&self) -> &Capabilities {
            &self.1
        }
        async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
            Ok(t)
        }
        fn current(&self) -> Option<Tuning> {
            None
        }
        fn subscribe(&self) -> IqReceiver {
            tokio::sync::broadcast::channel(1).1
        }
    }

    #[tokio::test]
    async fn a_port_does_not_sweep_by_default() {
        let port = Plain(
            SourceId("plain".into()),
            Capabilities {
                tune_min_hz: Claim::Asserted(Hz(0)),
                tune_max_hz: Claim::Asserted(Hz(30_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(12_000)),
                formats: vec![crate::types::SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: false,
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
            },
        );
        let req = SweepRequest {
            span: Tuning {
                center_hz: Hz(15_000_000),
                bandwidth_hz: Hz(30_000_000),
            },
            bin_hz: Hz(6_000),
            dwell_ms: 0,
        };
        match port.sweep(req).await {
            Err(e) => assert_eq!(
                e,
                PortError::Unsupported("source does not sweep for itself".into())
            ),
            Ok(rows) => panic!("expected a refusal, got {:?}", rows.plan),
        }
    }

    #[test]
    fn link_speed_is_the_part_after_the_at() {
        assert_eq!(link_speed("usb:2-1.4@5000"), Some("5000"));
        assert_eq!(link_speed("usb:2-1.4"), None);
        assert_eq!(link_speed("usb:2-1.4@"), None);
    }

    #[test]
    fn cached_figures_hold_unless_both_speeds_are_known_and_differ() {
        assert!(!cached_figures_hold(Some("5000"), Some("usb:1-2@480")));
        // Another port at the same speed keeps them: the path is never compared.
        assert!(cached_figures_hold(Some("5000"), Some("usb:3-1@5000")));
        assert!(cached_figures_hold(None, Some("usb:1-2@480")));
        assert!(cached_figures_hold(Some("5000"), None));
        assert!(cached_figures_hold(Some("5000"), Some("usb:1-2")));
        assert!(cached_figures_hold(None, None));
    }

    #[test]
    fn the_measuring_line_says_why() {
        let s = SourceId("b210".into());
        let step = |detail: &str| Starting {
            what: "measuring".into(),
            detail: detail.into(),
            step: 1,
            of: 6,
            eta_ms: 35_500,
        };
        assert_eq!(
            measuring_line(&s, &step("16-bit at 30720000 Hz")),
            "source b210: measuring what this radio's link can sustain (up to about 36 s; the host will be busy; max_rate_hz= skips it)"
        );
        assert_eq!(
            measuring_line(
                &s,
                &step("link speed changed (5000 to 480): 16-bit at 30720000 Hz")
            ),
            "source b210: measuring what this radio's link can sustain (link speed changed (5000 to 480); up to about 36 s; the host will be busy; max_rate_hz= skips it)"
        );
        assert_eq!(
            measuring_line(&s, &step("as asked: 16-bit at 30720000 Hz")),
            "source b210: measuring what this radio's link can sustain (as asked; up to about 36 s; the host will be busy; max_rate_hz= skips it)"
        );
    }
}
