//! The `watch` job (design 6): a controller that holds a rigid lease on its band like
//! `identify`, runs `fft → detect` over the capture, and identifies each signal `detect`
//! announces with an owned `identify` run on the signal's centre, one at a time. A proof
//! leaves the decoder running (inside the capture, or moved to a free source under
//! `decode_on: free`); the watch then owns it: it is listed in `job_context.watch`
//! (`signals`, `decoding`, `current`), stopped once its signal has gone and it has been
//! silent for the `detect` node's `hold_ms` (default [`SILENCE_MS`]) (`lost`), and stopped with the watch on a `job_stop`, a
//! `replace` or a failure (a stage's, the loop's, a panic of the loop included). A
//! `graph_put` restart or a node restart re-adopts the decoders from the persisted
//! context instead: those whose job no longer runs are dropped, those whose signal does
//! not reappear within the detector's `gone_ms` are stopped. Only events in the decoder
//! row's own `protocol` count against its silence (a frame row's `timing` and forwarded
//! `bits` flow on noise). An unknown signal is tried again after [`RETRY_MS`] while it
//! persists, at most [`RETRY_MAX`] times, each identify run's rows dwelling longer
//! ([`IDENTIFY_DWELL_SCALES`]: 1, 3, then 6 times their own `identify.dwell_ms`). The
//! watch runs one identify at a time, so while a long one dwells no other signal is
//! identified or started. With `on_unknown: capture` its bursts are
//! captured once per run by an owned `capture` child (see [`capture`]), unless identify
//! named a learned signature for it (summary `learned <name> at <MHz>: ...`).
//!
//! With `modes` on the `detect` node (a span watch, spec 4.2), a signal is matched to the
//! listed rows its measured width fits ([`SPAN_WIDTH_FIT`], or [`SPAN_FLOOR_FIT`] for a
//! reading at the detector's floor) instead: none makes it `unknown` at once (`no
//! listed mode fits`, retried and captured as above); else the fitting row nearest its
//! width starts directly on the signal's centroid (`decoding`
//! with `via: modes`, no identify run, in the capture or under `decode_on: free` on a
//! free source), and rows tied for nearest (one channel width) run an identify
//! restricted to them. At the start of each run a span watch learns its known channels
//! (spec 4.3, `job_context.watch.known`): the listed rows' centres in the band and the
//! licensed transmitters there whose emission fits a listed row ([`band_assignments`],
//! asked once). A signal on one ranks first for a slot and a direct start on a
//! register's names the licensee; with `seed_known` a decoder starts on each at once,
//! `lost` `not seen again` unless a `new` on its channel comes within `hold_ms`. A known
//! channel never moves a decoder.
//!
//! With `on_signal: choose` (and `modes: null`, plan 48), each new signal starts the
//! node's choice for it instead ([`Inner::start_choice`]): step 1 of the choice over the
//! signal's band of the ring, its job started at once inside the capture (`via: choose`)
//! with what applies of `attach` attached ([`Inner::attach_to`]: `transcribe` to a job
//! with audio, a row that rides to a job whose row it rides with) and the riders a proof
//! named (a `wfm_demod` proved by `rds_decode` gets `rds_decode`). An unproved
//! demodulator that a decoder row could still prove otherwise starts held ([`PROBATION`],
//! under `probation`: out of the mix, its transcript kept) while the watch's own
//! transient `choose` child climbs the ladder; its verdict ([`settle`]) lifts the hold,
//! attaches the riders it proved of the job's row (the job kept under its id), or
//! replaces the job with another row's, its attached jobs stopped first. A watch only
//! ever adds to or replaces its own job. A demodulator's silence is its squelch staying
//! shut. A signal step 1
//! cannot tell (no job) that decoder rows could still prove (for `unknown`, the rows its
//! width fits) keeps nothing: when the policy's step for the watch reaches step 2, the
//! watch's own `choose` tries them ([`Inner::start_choosing`], event `choosing` naming them
//! in `rows`, listed in `job_context.watch.choosing`), one per signal, holding a decoder
//! slot, and stopped when its signal goes or the watch stops. Its proof starts the proved
//! job `via: choose`, attached as above ([`choosing_ended`]); anything else is the unknown try
//! step 1 would have made. A row listed for `unknown` is never started on a guess.
//!
//! Every watch keeps at most the `detect` node's `max_decoders` decoders (spec 4.4),
//! counting those on free sources: over the cap a signal that would take one is
//! `waiting` (one event per signal), and when a slot frees the present waiting signal
//! that ranks highest ([`rank`]: a known channel, then score, then SNR) takes it; nothing
//! is pre-empted. With `confirm_ms`, a direct start that publishes no event within it
//! while its signal is present is stopped `unproven`. In a span watch, a `lost`
//! decoder's channel, and that of a decoder the operator stopped, is cooled for
//! `cooldown_ms` (`job_context.watch.cooled`): a signal on it is tracked but neither
//! started nor identified until the cooldown passes, and a signal still present after a
//! loss the watch raised counts a try (past [`RETRY_MAX`] it is left alone until it goes
//! and comes back). A span watch's re-adopted decoder is confirmed within the longer of
//! `gone_ms` and `hold_ms`. A watch without `modes` keeps its earlier rules: no
//! cooldown, a lost or stopped decoder's signal left alone until it comes back, and
//! re-adoption confirmed within `gone_ms`.
//!
//! Events, `protocol: "watch"`, go on the job's event stream beside `detect`'s `signal`
//! events: `state` `identifying | choosing | decoding | waiting | unknown | lost |
//! capturing | captured`, with `signal`, `job` (for `choosing`, its `choose`), `mode`,
//! `source`, `rows` (for `choosing`, the rows it tries), `placed` (`capture | free`),
//! `features` and `allocations` and `origins` (for `unknown`), `captures` (for
//! `captured`), `via` (for `decoding`: `identify | modes | choose`), `why` (for `lost`:
//! `gone and silent | not seen again | unproven | relaunch failed`), `summary` and `t_ns`
//! (fields that do not apply are `null`).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::TryRecvError;
use tokio_util::sync::CancellationToken;

use super::choose::{StepOne, choose_record};
use super::persist::now;
use super::{ChooseAsk, Inner, Node, source_fault_reason};
use crate::choice::{Choice, ChoiceJob, NOT_HELD_LEAD, Settled};
use crate::choice_policy::{LeaseSetting, Surface};
use crate::graph::{self, Graph, Node as GraphNode};
use crate::job::{Hold, JOB_RECORD_FORMAT, JobRecord, JobState};
use crate::knowledge::{Allocation, DEFAULT_REGION, band_assignments};
use crate::modes::score::{passed_over, score_rows};
use crate::modes::{ModeRegistry, ModeState, Role};
use crate::ops::{
    ATTACH_LANGUAGE_DEFAULT, DETECT_GONE_MS_DEFAULT, DETECT_STAGE, DETECT_WIDTH_HZ_DEFAULT,
    IDENTIFY_DWELL_SCALES, JobKind, SONDEHUB_STAGE, Sideband, WATCH_CONFIRM_MS_DEFAULT,
    WATCH_COOLDOWN_MS_DEFAULT, WATCH_HOLD_MS_DEFAULT, WATCH_MAX_DECODERS_DEFAULT,
    WATCH_MODES_EMPTY,
};
use crate::port::{Port, Tuning};
use crate::stage::StageError;
use crate::stream::{Frame, Payload};
use crate::types::{Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind};

mod capture;

/// How long after an `unknown` verdict a signal that persists is identified again.
pub const RETRY_MS: u64 = 60_000;
/// How many times an unknown signal is identified again before it is left alone (until
/// it goes and comes back).
pub const RETRY_MAX: u32 = 3;
/// How long a decoder whose signal has gone may stay silent (no event) before it is
/// stopped as `lost`, unless the `detect` node sets `hold_ms`.
pub const SILENCE_MS: u64 = WATCH_HOLD_MS_DEFAULT;
/// A row fits a signal whose measured width lies within these multiples of the row's
/// `band.bandwidth_hz` (the scoring's width term).
pub const WIDTH_FIT: (f64, f64) = (0.5, 1.5);

/// A span watch's listed row fits a detected signal whose width lies within these
/// multiples of the row's `band.bandwidth_hz`: wider than [`WIDTH_FIT`] above, since the
/// detector reads a strong FSK signal's skirts (a 12.5 kHz page reads 19-25 kHz).
pub const SPAN_WIDTH_FIT: (f64, f64) = (0.5, 2.0);
/// A span watch's fit for a signal read at the detector's `width_hz` floor (within
/// [`FLOOR_HAIR`]): the detector drops narrower runs, so a clean FSK carrier that reads
/// narrow shows up at the floor, and a 6 kHz floor must still reach a 12.5 kHz row.
pub const SPAN_FLOOR_FIT: (f64, f64) = (0.4, 2.0);
/// A reading at most this fraction over the detector's floor counts as at the floor.
const FLOOR_HAIR: f64 = 0.1;
/// Two fitting rows whose width ratios lie this close (in |ln ratio|) tie for nearest.
const RATIO_TIE: f64 = 1e-9;

/// Whether a signal `signal_hz` wide fits a row `row_hz` wide within `fit`.
fn fits_within(fit: (f64, f64), row_hz: u64, signal_hz: u64) -> bool {
    let row = row_hz as f64;
    (row * fit.0..=row * fit.1).contains(&(signal_hz as f64))
}

/// Whether a signal read `signal_hz` wide by a detector whose `width_hz` floor is
/// `floor_hz` fits a span watch's row `row_hz` wide: by [`SPAN_FLOOR_FIT`] at the
/// floor, else by [`SPAN_WIDTH_FIT`].
fn span_fits(row_hz: u64, signal_hz: u64, floor_hz: u64) -> bool {
    let at_floor = signal_hz as f64 <= floor_hz as f64 * (1.0 + FLOOR_HAIR);
    let fit = if at_floor {
        SPAN_FLOOR_FIT
    } else {
        SPAN_WIDTH_FIT
    };
    fits_within(fit, row_hz, signal_hz)
}

/// The band an identify run on `s` takes and its queue score: the best-scoring ready
/// row of `modes` (`score_rows` at the signal's centre and width) in range for the
/// signal's centre that fits the signal (its measured width within [`WIDTH_FIT`] of the
/// row's band) and a watch band `watch_hz` wide, else the signal's own width at score 0.
/// A row tried only when named (`identify.when_named`) counts only when the watch's
/// `modes` (`named`, `None` for the default) hold it.
fn identify_band(
    modes: &ModeRegistry,
    watch_hz: u64,
    s: &Signal,
    named: Option<&[String]>,
) -> (u64, f32) {
    score_rows(
        modes.weights(),
        modes,
        s.center_hz,
        s.bandwidth_hz,
        "",
        &|_| 0.0,
        &|_| 0.0,
    )
    .into_iter()
    .filter(|x| x.mode.startable() && x.in_range && !passed_over(x.mode, named))
    .find(|x| {
        let band = &x.mode.row.band;
        width_fits(band.bandwidth_hz, s.bandwidth_hz) && band.bandwidth_hz <= watch_hz
    })
    .map_or((s.bandwidth_hz, 0.0), |x| {
        (x.mode.row.band.bandwidth_hz, x.score)
    })
}

/// The `dwell_scale` of a signal's identify run after `tries` tries that found nothing:
/// [`IDENTIFY_DWELL_SCALES`]' entry for the try, its last for every try after.
fn dwell_scale(tries: u32) -> u32 {
    let last = IDENTIFY_DWELL_SCALES.len() - 1;
    IDENTIFY_DWELL_SCALES[(tries as usize).min(last)]
}

/// Whether a signal `signal_hz` wide fits a row `row_hz` wide by [`WIDTH_FIT`].
fn width_fits(row_hz: u64, signal_hz: u64) -> bool {
    fits_within(WIDTH_FIT, row_hz, signal_hz)
}

/// Of the fitting rows `fits` (`(id, band.bandwidth_hz)`), those whose width ratio
/// (`signal_hz` / the row's width) lies nearest 1 (by |ln ratio|): one row is started
/// directly, several (rows of one channel width) tie and go to a restricted identify.
fn nearest_fits(fits: &[(String, u64)], signal_hz: u64) -> Vec<String> {
    let off = |row_hz: u64| (signal_hz as f64 / row_hz as f64).ln().abs();
    let Some(best) = fits.iter().map(|(_, w)| off(*w)).min_by(f64::total_cmp) else {
        return Vec::new();
    };
    fits.iter()
        .filter(|(_, w)| off(*w) - best <= RATIO_TIE)
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(feature = "test-fixtures")]
static RETRY_OVERRIDE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "test-fixtures")]
static SILENCE_OVERRIDE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Tests only: use `retry_ms` for [`RETRY_MS`] and `silence_ms` for [`SILENCE_MS`] in
/// every watch of the process from now on (0 restores the constant).
#[cfg(feature = "test-fixtures")]
pub fn test_timing(retry_ms: u64, silence_ms: u64) {
    use std::sync::atomic::Ordering;
    RETRY_OVERRIDE_MS.store(retry_ms, Ordering::SeqCst);
    SILENCE_OVERRIDE_MS.store(silence_ms, Ordering::SeqCst);
}

fn retry_ms() -> u64 {
    #[cfg(feature = "test-fixtures")]
    {
        let v = RETRY_OVERRIDE_MS.load(std::sync::atomic::Ordering::SeqCst);
        if v > 0 {
            return v;
        }
    }
    RETRY_MS
}

/// Tests only: the watch event state after whose publication the next watch fails, and
/// whether by a panic of its loop (else the loop ends `Failed` at its next tick).
#[cfg(feature = "test-fixtures")]
static TEST_FAULT: std::sync::Mutex<Option<(String, bool)>> = std::sync::Mutex::new(None);
#[cfg(feature = "test-fixtures")]
static TEST_FAIL_NOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tests only: make the first watch that publishes an event in `state` from now on fail
/// right after it: its loop panics when `panic`, else it ends with the error
/// `watch: test failure` at its next tick.
#[cfg(feature = "test-fixtures")]
pub fn test_fault_after(state: &str, panic: bool) {
    *TEST_FAULT.lock().unwrap() = Some((state.to_string(), panic));
}

/// Tests only: how many of the next sink restarts fail to launch.
#[cfg(feature = "test-fixtures")]
static TEST_FAIL_SINK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Tests only: make the next `times` launches of a decoder graph with the sink fail.
#[cfg(feature = "test-fixtures")]
pub fn test_fail_sink_launch(times: u32) {
    TEST_FAIL_SINK.store(times, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(feature = "test-fixtures")]
fn sink_launch_fails() -> bool {
    use std::sync::atomic::Ordering;
    TEST_FAIL_SINK
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

/// Tests only: while set, a sink restart waits as it begins, before it takes the
/// decoder's restart lock.
#[cfg(feature = "test-fixtures")]
static TEST_HOLD_SINK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tests only: how many sink restarts wait on the hold now.
#[cfg(feature = "test-fixtures")]
static TEST_SINK_HELD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Tests only: the sink restarts are held while this lives; dropped (a test that
/// panicked included) or [`SinkHold::release`]d, they go on.
#[cfg(feature = "test-fixtures")]
pub struct SinkHold(());

#[cfg(feature = "test-fixtures")]
impl SinkHold {
    /// Let the held sink restarts go on.
    pub fn release(&self) {
        TEST_HOLD_SINK.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(feature = "test-fixtures")]
impl Drop for SinkHold {
    fn drop(&mut self) {
        self.release();
    }
}

/// Tests only: hold the sink restarts from now on, until the guard goes.
#[cfg(feature = "test-fixtures")]
pub fn test_hold_sink_restarts() -> SinkHold {
    TEST_HOLD_SINK.store(true, std::sync::atomic::Ordering::SeqCst);
    SinkHold(())
}

/// Tests only: while set, a watch leaves the verdicts of its `choose` children untaken.
#[cfg(feature = "test-fixtures")]
static TEST_HOLD_CHOICES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tests only: the watches' `choose` verdicts wait while this lives; dropped (a test that
/// panicked included) or [`ChoiceHold::release`]d, they are taken.
#[cfg(feature = "test-fixtures")]
pub struct ChoiceHold(());

#[cfg(feature = "test-fixtures")]
impl ChoiceHold {
    /// Let the watches take their `choose` verdicts.
    pub fn release(&self) {
        TEST_HOLD_CHOICES.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(feature = "test-fixtures")]
impl Drop for ChoiceHold {
    fn drop(&mut self) {
        self.release();
    }
}

/// Tests only: hold every watch's `choose` verdicts from now on, until the guard goes.
#[cfg(feature = "test-fixtures")]
pub fn test_hold_choices() -> ChoiceHold {
    TEST_HOLD_CHOICES.store(true, std::sync::atomic::Ordering::SeqCst);
    ChoiceHold(())
}

/// Tests only: the row a taken `choose` verdict is made to name (its job's kind and its
/// row), its `settled` left as it is.
#[cfg(feature = "test-fixtures")]
static TEST_VERDICT_ROW: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Tests only: every `choose` verdict a watch takes names `row` from now on, until the
/// guard goes (an unproved guess at another row, which a test cannot otherwise make).
#[cfg(feature = "test-fixtures")]
pub fn test_verdicts_name(row: &str) -> VerdictRename {
    *TEST_VERDICT_ROW.lock().unwrap() = Some(row.to_string());
    VerdictRename(())
}

/// Tests only: while it lives, taken verdicts name the row [`test_verdicts_name`] gave.
#[cfg(feature = "test-fixtures")]
pub struct VerdictRename(());

#[cfg(feature = "test-fixtures")]
impl Drop for VerdictRename {
    fn drop(&mut self) {
        *TEST_VERDICT_ROW.lock().unwrap() = None;
    }
}

#[cfg(feature = "test-fixtures")]
fn test_rename_verdict(choice: &mut Option<Choice>) {
    let row = TEST_VERDICT_ROW.lock().unwrap().clone();
    if let (Some(row), Some(c)) = (row, choice.as_mut())
        && let Some(job) = c.job.as_mut()
    {
        job.kind = row.clone();
        c.row = Some(row);
    }
}

/// Tests only: the layer-1 label a choosing watch's step 1 is made to read.
#[cfg(feature = "test-fixtures")]
static TEST_LABEL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Tests only: every choosing watch's step 1 reads layer 1's label as `label` from now
/// on, until the guard goes (a label such as `unknown`, which a generator does not give
/// reliably at the watch's capture rate).
#[cfg(feature = "test-fixtures")]
pub fn test_labels_read(label: &str) -> LabelForce {
    *TEST_LABEL.lock().unwrap() = Some(label.to_string());
    LabelForce(())
}

/// Tests only: while it lives, step 1 reads the label [`test_labels_read`] gave.
#[cfg(feature = "test-fixtures")]
pub struct LabelForce(());

#[cfg(feature = "test-fixtures")]
impl Drop for LabelForce {
    fn drop(&mut self) {
        *TEST_LABEL.lock().unwrap() = None;
    }
}

#[cfg(feature = "test-fixtures")]
fn test_force_label(c: &mut crate::classify::Classification) {
    let Some(label) = TEST_LABEL.lock().unwrap().clone() else {
        return;
    };
    if let Some(m) = c.measurement.as_mut() {
        m.label.name = label.clone();
    }
    for l in c.labels.iter_mut().filter(|l| l.layer == 1) {
        l.name = label.clone();
    }
}

/// Whether a test holds the watches' `choose` verdicts.
fn choices_held() -> bool {
    #[cfg(feature = "test-fixtures")]
    {
        TEST_HOLD_CHOICES.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(feature = "test-fixtures"))]
    {
        false
    }
}

/// Tests only: how many sink restarts wait on the hold now.
#[cfg(feature = "test-fixtures")]
pub fn test_sink_restarts_held() -> u32 {
    TEST_SINK_HELD.load(std::sync::atomic::Ordering::SeqCst)
}

/// Tests only: wait while sink restarts are held; `false` when `cancel` fired first.
#[cfg(feature = "test-fixtures")]
async fn sink_hold(cancel: &CancellationToken) -> bool {
    use std::sync::atomic::Ordering;
    if !TEST_HOLD_SINK.load(Ordering::SeqCst) {
        return true;
    }
    TEST_SINK_HELD.fetch_add(1, Ordering::SeqCst);
    let mut passed = true;
    while TEST_HOLD_SINK.load(Ordering::SeqCst) {
        tokio::select! {
            _ = cancel.cancelled() => {
                passed = false;
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
    TEST_SINK_HELD.fetch_sub(1, Ordering::SeqCst);
    passed
}

#[cfg(feature = "test-fixtures")]
fn test_fault_at(state: &str) {
    let hit = {
        let mut f = TEST_FAULT.lock().unwrap();
        match f.as_ref() {
            Some((s, _)) if s == state => f.take(),
            _ => None,
        }
    };
    match hit {
        Some((_, true)) => panic!("watch: test panic after {state}"),
        Some((_, false)) => TEST_FAIL_NOW.store(true, std::sync::atomic::Ordering::SeqCst),
        None => {}
    }
}

#[cfg(feature = "test-fixtures")]
fn test_fail_now() -> bool {
    TEST_FAIL_NOW.swap(false, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(not(feature = "test-fixtures"))]
fn test_fail_now() -> bool {
    false
}

fn silence_ms() -> u64 {
    #[cfg(feature = "test-fixtures")]
    {
        let v = SILENCE_OVERRIDE_MS.load(std::sync::atomic::Ordering::SeqCst);
        if v > 0 {
            return v;
        }
    }
    SILENCE_MS
}

/// The refusal of a sink with no position and no station (the text `sink_sondehub` notes
/// when it idles for the same reason).
const SINK_NO_STATION: &str = "sink.params.lat: no station set (station set --lat --lon --alt-m)";
/// How often the loop looks at its streams, its identify run and its decoders.
const POLL: Duration = Duration::from_millis(50);
/// How long a resumed watch waits for the records it names (its decoders, a stale
/// identify) to be launched: a resume launches the watch first (the widest band).
const ADOPT_WAIT: Duration = Duration::from_secs(5);

/// Where a decoder the watch keeps runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placed {
    /// Inside the watch's own capture, where identify proved it.
    Capture,
    /// On a free source (`decode_on: free`).
    Free,
}

impl Placed {
    fn as_str(self) -> &'static str {
        match self {
            Placed::Capture => "capture",
            Placed::Free => "free",
        }
    }
}

/// The watch's policy, read from its `detect` node's params (which the stage ignores).
#[derive(Clone, Debug)]
pub(super) struct Policy {
    free: bool,
    prefer: Vec<SourceId>,
    gone_ms: u64,
    /// The sinks every decoder the watch keeps gets, fed by its events: the `detect`
    /// node's `sink` (or its `sondehub` shorthand; none appends nothing). Stored beside
    /// the decoder's graph ([`JobRecord::sinks`]).
    sinks: Vec<GraphNode>,
    /// With `on_unknown: capture`, the `burst_capture` params (the `capture` block, `{}`
    /// for the defaults) of the capture an unknown signal gets; `None` with `report`.
    capture: Option<serde_json::Value>,
    /// The mode rows signals are decoded with directly; `None` lets identify decide.
    modes: Option<Vec<String>>,
    /// The detector's `width_hz` floor: a signal read at it fits by [`SPAN_FLOOR_FIT`].
    width_floor_hz: u64,
    /// Kept decoders at once.
    max_decoders: u32,
    /// How long a kept decoder whose signal is `gone` may stay silent before it is `lost`.
    hold_ms: u64,
    /// After a `lost` or `unproven`, how long its channel starts no new decoder.
    cooldown_ms: u64,
    /// With `modes`, how long a directly started decoder has to publish an event (0: off).
    confirm_ms: u64,
    /// With `modes`, start a decoder on every known channel in the band at once.
    seed_known: bool,
    /// With `on_signal: choose` (and `modes: null`), each new signal starts the node's
    /// choice for it ([`Inner::start_choice`]) instead of an identify run.
    choose: bool,
    /// What a choosing watch attaches to each job it starts (`attach`), each entry where
    /// it applies ([`Inner::attach_to`]): `transcribe` to a job with audio, a row that
    /// rides to a job whose row it rides with. Listed once each, in order; a name the
    /// node has no such row for (refused at the start, [`Node::check_watch_policy`]) is
    /// skipped when it applies.
    attach: Vec<JobKind>,
    /// Whether an unproved demodulator a decoder row could still prove otherwise starts
    /// held until the watch's `choose` settles (`probation`, default true).
    probation: bool,
}

/// The sinks the policy `p` (a `detect` node's params) gives every kept decoder
/// ([`graph::watch_sinks`]; the start and put of the watch refuse a bad one first).
fn sinks_of(p: &serde_json::Value) -> Vec<GraphNode> {
    graph::watch_sinks(p)
        .map(|v| v.into_iter().map(|(_, n)| n).collect())
        .unwrap_or_default()
}

impl Policy {
    /// The policy in `graph`'s `detect` node: `decode_on` (`capture`, default, or
    /// `free`), `prefer` (source ids), the detector's `gone_ms`, `sink` (or `sondehub`),
    /// `on_unknown`/`capture`, `modes`, `max_decoders`, `hold_ms` (left out: the
    /// silence constant, or a test's override of it), `cooldown_ms`, `confirm_ms`,
    /// `seed_known`, `on_signal`, `attach` and `probation`.
    fn of(graph: &Graph) -> Policy {
        let p = policy_of(graph);
        Policy {
            free: p["decode_on"] == "free",
            prefer: p["prefer"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(|s| SourceId(s.into())))
                        .collect()
                })
                .unwrap_or_default(),
            gone_ms: p["gone_ms"].as_u64().unwrap_or(DETECT_GONE_MS_DEFAULT),
            sinks: sinks_of(&p),
            capture: (p["on_unknown"] == "capture").then(|| match &p["capture"] {
                serde_json::Value::Null => serde_json::json!({}),
                v => v.clone(),
            }),
            modes: p["modes"].as_array().map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            }),
            width_floor_hz: p["width_hz"][0]
                .as_u64()
                .unwrap_or(DETECT_WIDTH_HZ_DEFAULT[0]),
            max_decoders: p["max_decoders"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(WATCH_MAX_DECODERS_DEFAULT),
            hold_ms: p["hold_ms"].as_u64().unwrap_or_else(silence_ms),
            cooldown_ms: p["cooldown_ms"]
                .as_u64()
                .unwrap_or(WATCH_COOLDOWN_MS_DEFAULT),
            confirm_ms: p["confirm_ms"].as_u64().unwrap_or(WATCH_CONFIRM_MS_DEFAULT),
            seed_known: p["seed_known"].as_bool().unwrap_or(false),
            choose: p["on_signal"] == "choose" && p["modes"].is_null(),
            attach: p["attach"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|k| k.as_str().and_then(JobKind::from_name))
                .fold(Vec::new(), |mut kinds, k| {
                    if !kinds.contains(&k) {
                        kinds.push(k);
                    }
                    kinds
                }),
            probation: p["probation"].as_bool().unwrap_or(true),
        }
    }
}

/// The params of `graph`'s `detect` node, where a watch's policy lives (`{}` without
/// one): what a set's `watch` member is compared with for `existing`.
pub(crate) fn policy_of(graph: &Graph) -> serde_json::Value {
    graph
        .nodes
        .iter()
        .find(|n| n.kind == DETECT_STAGE)
        .map(|n| n.params.clone())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Whether the `detect` params `a` and `b` are one policy: every key of either is
/// compared, a key one of them leaves out at its default in `schema` (the `detect`
/// stage's params schema), numbers by value (`6` is `6.0`).
pub(crate) fn same_policy(
    a: &serde_json::Value,
    b: &serde_json::Value,
    schema: &serde_json::Value,
) -> bool {
    let defaults = schema["properties"].as_object();
    let value = |p: &serde_json::Value, k: &str| -> serde_json::Value {
        match p.get(k) {
            Some(v) => v.clone(),
            None => defaults
                .and_then(|d| d.get(k))
                .and_then(|s| s.get("default"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        }
    };
    let keys = |p: &serde_json::Value| {
        p.as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let mut all = keys(a);
    all.extend(keys(b));
    all.iter().all(|k| json_same(&value(a, k), &value(b, k)))
}

/// `a == b`, numbers compared by value.
fn json_same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value as V;
    match (a, b) {
        (V::Number(x), V::Number(y)) => x == y || x.as_f64() == y.as_f64(),
        (V::Array(x), V::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| json_same(x, y))
        }
        (V::Object(x), V::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_same(v, w)))
        }
        _ => a == b,
    }
}

/// How a kept decoder came to be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// An identify run proved it.
    Identify,
    /// Started directly from the watch's `modes`.
    Modes,
    /// The node's choice for the signal (`on_signal: choose`).
    Choose,
}

impl Via {
    /// The `via` of a decoder persisted before the field existed.
    fn identify() -> Via {
        Via::Identify
    }
}

/// Check a watch's `modes` list against `modes` for a band `band_hz` wide: each id a
/// loaded, ready row no wider than the band, and the list not empty. The reason names
/// the entry: `modes[<i>]: unknown mode <id>`, `modes[<i>]: <id> is broken (<reason>)`
/// (or `unavailable`), `modes[<i>]: <id> is <w> Hz wide; the watch's band is <b> Hz`,
/// or `modes: empty list (null lets identify decide)`.
pub(crate) fn check_modes_list(
    modes: &ModeRegistry,
    list: &[String],
    band_hz: u64,
) -> Result<(), String> {
    if list.is_empty() {
        return Err(WATCH_MODES_EMPTY.into());
    }
    for (i, id) in list.iter().enumerate() {
        let Some(m) = modes.get(id) else {
            return Err(format!("modes[{i}]: unknown mode {id}"));
        };
        let state = match m.state {
            _ if m.startable() => None,
            ModeState::Broken => Some("broken"),
            _ => Some("unavailable"),
        };
        if let Some(state) = state {
            let reason = m.reason.as_deref().unwrap_or("no reason given");
            return Err(format!("modes[{i}]: {id} is {state} ({reason})"));
        }
        check_mode_width(i, m, band_hz)?;
    }
    Ok(())
}

/// Check a watch's `attach` (in its `detect` params `p`) against `modes`: each entry
/// `transcribe` or a row that rides with a demodulator ([`crate::ops::rider_row`],
/// whatever its state: one not ready is noted when it would attach). The reason names
/// the entry: `attach[<i>]: <kind> is not attached by a watch (only transcribe and rows
/// that ride with a demodulator)`.
pub(crate) fn check_attach(modes: &ModeRegistry, p: &serde_json::Value) -> Result<(), String> {
    let list = p.get("attach").and_then(|a| a.as_array());
    for (i, kind) in list.into_iter().flatten().enumerate() {
        let name = kind.as_str().unwrap_or_default();
        let fits = match JobKind::from_name(name) {
            Some(JobKind::Transcribe) => true,
            Some(k) => crate::ops::rider_row(modes, &k).is_some(),
            None => false,
        };
        if !fits {
            return Err(format!(
                "attach[{i}]: {name} is not attached by a watch (only transcribe and rows that ride with a demodulator)"
            ));
        }
    }
    Ok(())
}

/// [`check_modes_list`] as a set file is checked at load: the list not empty, and each
/// row the registry holds (whatever its state) no wider than the band. An unknown,
/// broken or unavailable row passes: a set may name a row this host lacks, refused when
/// the set starts.
pub(crate) fn check_modes_list_at_load(
    modes: &ModeRegistry,
    list: &[String],
    band_hz: u64,
) -> Result<(), String> {
    if list.is_empty() {
        return Err(WATCH_MODES_EMPTY.into());
    }
    for (i, id) in list.iter().enumerate() {
        if let Some(m) = modes.get(id) {
            check_mode_width(i, m, band_hz)?;
        }
    }
    Ok(())
}

/// Entry `i` of a watch's `modes`, the row `m`, no wider than the band `band_hz`.
fn check_mode_width(i: usize, m: &crate::modes::LoadedMode, band_hz: u64) -> Result<(), String> {
    let (id, width) = (&m.row.id.0, m.row.band.bandwidth_hz);
    if width > band_hz {
        return Err(format!(
            "modes[{i}]: {id} is {width} Hz wide; the watch's band is {band_hz} Hz"
        ));
    }
    Ok(())
}

/// A signal as `detect` announced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Signal {
    center_hz: u64,
    bandwidth_hz: u64,
    snr_db: f64,
    first_ns: u64,
    last_ns: u64,
}

impl Signal {
    /// The signal of a `detect` `signal` event.
    fn of(v: &serde_json::Value) -> Option<Signal> {
        Some(Signal {
            center_hz: v["center_hz"].as_u64()?,
            bandwidth_hz: v["bandwidth_hz"].as_u64()?,
            snr_db: v["snr_db"].as_f64().unwrap_or(0.0),
            first_ns: v["first_ns"].as_u64().unwrap_or(0),
            last_ns: v["last_ns"].as_u64().unwrap_or(0),
        })
    }

    /// Whether the band `center_hz` ± `bandwidth_hz`/2 overlaps the signal's.
    fn overlaps(&self, center_hz: u64, bandwidth_hz: u64) -> bool {
        2 * self.center_hz.abs_diff(center_hz) < self.bandwidth_hz + bandwidth_hz
    }

    fn mhz(&self) -> String {
        format!("{:.4} MHz", self.center_hz as f64 / 1e6)
    }
}

/// The index of the tracked signal whose band overlaps `center_hz` ± `bandwidth_hz`/2 and
/// whose centre lies nearest `center_hz`: a wide track that also covers a narrow one's
/// centre (two runs the detector sees side by side) never takes the narrow one's verdict
/// or announcement.
fn nearest(signals: &[Tracked], center_hz: u64, bandwidth_hz: u64) -> Option<usize> {
    signals
        .iter()
        .enumerate()
        .filter(|(_, x)| x.signal.overlaps(center_hz, bandwidth_hz))
        .min_by_key(|(_, x)| x.signal.center_hz.abs_diff(center_hz))
        .map(|(i, _)| i)
}

/// A signal the watch follows (`job_context.watch.signals`).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Tracked {
    #[serde(flatten)]
    signal: Signal,
    /// Whether `detect` still sees it (no `gone` since its last `new`).
    present: bool,
    /// Identify runs that ended without a proof.
    tries: u32,
    /// When it may be identified again after an `unknown`.
    next_retry_ns: Option<u64>,
    /// Decoding, or left alone after [`RETRY_MAX`] retries.
    handled: bool,
    /// The band its identify run takes: the best-scoring fitting row's width.
    band_hz: u64,
    /// That row's score, the queue's order.
    score: f32,
    /// The last unknown verdict's layer-1 record.
    #[serde(default)]
    features: Option<serde_json::Value>,
    /// The last unknown verdict's register answer (identify's `allocations`).
    #[serde(default)]
    allocations: Option<serde_json::Value>,
    /// The learned signature the last unknown verdict named (identify's `learned`):
    /// such a signal is not captured.
    #[serde(default)]
    learned: Option<serde_json::Value>,
    /// Whether this run has started a capture of it (`on_unknown: capture`).
    #[serde(default)]
    captured: bool,
    /// The captures its capture wrote.
    #[serde(default)]
    captures: Vec<String>,
    /// The policy's `modes` rows its width fits by [`SPAN_WIDTH_FIT`] (and the watch's
    /// band holds); empty with `modes: null`.
    #[serde(default)]
    fits: Vec<String>,
    /// Whether it waits for a free decoder slot.
    #[serde(default)]
    waiting: bool,
}

/// What a start of a row by the watch carries beyond the row, its band and its sinks.
#[derive(Default)]
struct Extra {
    /// The deviation a choice gives an `fm_demod` (else the plan's at its centre).
    deviation_hz: Option<Hz>,
    /// The sideband a choice gives an `ssb_demod`.
    sideband: Option<Sideband>,
    /// The hold the job starts under.
    held: Option<Hold>,
}

/// The reason of a hold the watch puts on an unproved demodulator while its `choose`
/// settles.
pub const PROBATION: &str = "probation";

/// What came of giving a kept decoder the watch's sinks (as one set: they restart and
/// fall back together).
enum SinkOutcome {
    /// Not asked for, or nothing to attach it to.
    Off,
    /// The decoder runs with the sink.
    On,
    /// The decoder runs without it, for this reason.
    Refused(String),
    /// The decoder could not be relaunched at all.
    Lost(String),
    /// The watch was stopped meanwhile.
    Cancelled,
}

/// Attach a job of `kind` to `job` as `job_attach` does, in its own task (a plain
/// function: the watch's loop holds only the handle, so its future does not contain the
/// launch's): a transcriber on its main audio, any other kind as a rider on its channel
/// ([`crate::ops::rider_attach`], which takes the node's attach gate itself).
fn attach_task(
    inner: &Arc<Inner>,
    kind: JobKind,
    job: JobId,
) -> tokio::task::JoinHandle<Result<JobId, String>> {
    let node = Node::handle(inner);
    tokio::spawn(async move {
        let attached = match kind {
            JobKind::Transcribe => {
                crate::ops::attach_one(&node, &kind, &job, ATTACH_LANGUAGE_DEFAULT, None).await
            }
            _ => crate::ops::rider_attach(&node, &kind, &job).await,
        };
        attached.map_err(|e| e.to_string())
    })
}

/// A persisted `attached`: a list, or the one job a watch persisted before plan 49 kept.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<JobId>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(JobId),
        Many(Vec<JobId>),
    }
    Ok(match Option::<OneOrMany>::deserialize(d)? {
        None => Vec::new(),
        Some(OneOrMany::One(j)) => vec![j],
        Some(OneOrMany::Many(v)) => v,
    })
}

/// What came of attaching a rider to a kept job ([`Inner::ride`]).
enum Ride {
    /// It rides on the job now.
    Attached(JobId),
    /// A running job of the row already rides on it: `Ok` the watch's own, `Err`
    /// another's (the operator's, or a page's).
    Riding(Result<JobId, JobId>),
    /// It did not attach: `<row> did not attach: <why>`.
    Refused(String),
    /// The watch was stopped meanwhile.
    Cancelled,
}

/// A choosing watch's step 1 in progress ([`Inner::start_choice`]): the signal's centre,
/// the band classified, and the task reading the ring and choosing.
struct Classifying {
    center_hz: u64,
    band: Tuning,
    task: tokio::task::JoinHandle<Result<Classified, String>>,
}

/// A choosing watch's step 1 for one signal: the answer, whether the watch's own
/// `choose` may climb to step 2 (the policy's step for the watch, read once, so the answer
/// is kept for trials exactly when the watch will try it), and whether it may widen: the
/// lease setting `widen` with that step reaching 3, so step 3 also tries the rows step 1
/// set aside at the capture's edge ([`rows_to_try`]).
struct Classified {
    step: StepOne,
    climb: bool,
    widen: bool,
}

/// The rows the watch's `choose` would try for `choice`: its `could_prove`, then, when it
/// may `widen` ([`Classified`]), each row step 1 set aside as one the capture does not hold
/// there ([`NOT_HELD_LEAD`]), which step 3 tries by growing the capture.
fn rows_to_try(choice: &Choice, widen: bool) -> Vec<String> {
    let set_aside = choice
        .not_tried
        .iter()
        .filter(|n| widen && n.reason.starts_with(NOT_HELD_LEAD))
        .filter_map(|n| n.row.clone());
    let mut rows = choice.could_prove.clone();
    for row in set_aside {
        if !rows.contains(&row) {
            rows.push(row);
        }
    }
    rows
}

/// Step 1 of the choice for `band` of `source` over the newest `slice_s` of its ring, in
/// its own task (a plain function, as [`attach_task`]): the answer with what step 2 keeps
/// of what it read, cut to the trial band ([`StepOne::chosen`]; handed to the watch's
/// `choose` child, which then classifies nothing again), or why the band was not
/// classified.
fn classify_task(
    inner: &Arc<Inner>,
    source: &SourceId,
    band: Tuning,
    slice_s: f32,
) -> tokio::task::JoinHandle<Result<Classified, String>> {
    let (inner, source) = (inner.clone(), source.clone());
    tokio::spawn(async move {
        let (sel, held_ns) = inner
            .newest_selection(&source, band, slice_s)
            .ok_or_else(|| format!("source {} is not capturing", source.0))?;
        let node = Node::handle(&inner);
        #[allow(unused_mut)]
        let (mut classified, samples, rate, ring_center) =
            crate::ops::classify_samples(&node, &sel)
                .await
                .map_err(|e| e.to_string())?;
        #[cfg(feature = "test-fixtures")]
        test_force_label(&mut classified);
        // The watch's own `choose` climbs to the policy's step for the watch.
        let (to, lease) = {
            let policy = inner.choice_policy.lock().unwrap();
            (policy.steps(Surface::Watch, None).0, policy.lease)
        };
        let climb = to >= 2;
        let widen = to >= 3 && lease == LeaseSetting::Widen;
        let block = (samples, rate, ring_center);
        let at = (sel, held_ns, climb, slice_s);
        let step = StepOne::chosen(&node, at, classified, block).await;
        Ok(Classified { step, climb, widen })
    })
}

/// `width` about `center_hz`, narrowed so the band lies inside a ring centred
/// `ring_center` that holds `rate` Hz (twice the distance to its nearer edge), but never
/// under `floor_hz`.
fn ring_clamped(width: u64, floor_hz: u64, center_hz: u64, (ring_center, rate): (u64, u64)) -> u64 {
    let (lo, hi) = (ring_center.saturating_sub(rate / 2), ring_center + rate / 2);
    let room = center_hz
        .saturating_sub(lo)
        .min(hi.saturating_sub(center_hz))
        .saturating_mul(2);
    width.min(room).max(floor_hz)
}

/// Why a started decoder runs without the sinks it was asked to have, from `sink`.
fn sink_error_of(sink: SinkOutcome) -> Option<String> {
    match sink {
        SinkOutcome::Refused(why) | SinkOutcome::Lost(why) => Some(why),
        SinkOutcome::Off | SinkOutcome::On | SinkOutcome::Cancelled => None,
    }
}

/// Where a decoder was placed and what the sink made of it.
struct Placement {
    job: JobId,
    source: SourceId,
    placed: Placed,
    /// Why it fell back to the capture, when it did.
    why: Option<String>,
    sink: SinkOutcome,
}

/// A decoder the watch keeps (`job_context.watch.decoding`).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Decoding {
    mode: String,
    job: JobId,
    source: SourceId,
    signal: Signal,
    since_ns: u64,
    placed: Placed,
    /// How it was started (a decoder persisted before the field: `identify`).
    #[serde(default = "Via::identify")]
    via: Via,
    /// Why the decoder runs without the sinks it was asked to have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sink_error: Option<String>,
    /// The jobs the watch attached to it (`attach`, and the riders its choice proved),
    /// in order, each stopped before it.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "one_or_many"
    )]
    attached: Vec<JobId>,
    /// Whether it is held on probation ([`PROBATION`]) while `choose` settles.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    held: bool,
    /// The watch's `choose` child for its signal, until its verdict is taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    choose: Option<JobId>,
    /// The last event of the decoder this run saw (else `since_ns`); for a demodulator,
    /// the last moment its squelch was open.
    #[serde(skip)]
    last_event_ns: u64,
    /// A demodulator's squelch as its last `squelch` event said (`None` before one).
    #[serde(skip)]
    squelch_open: Option<bool>,
    /// When `detect` reported its signal `gone` (cleared when it is seen again).
    #[serde(skip)]
    gone_since_ns: Option<u64>,
    /// A re-adopted decoder whose signal has not been seen again since: stopped when
    /// this passes.
    #[serde(skip)]
    confirm_by_ns: Option<u64>,
    /// A direct start under `confirm_ms` that has published no event yet: stopped
    /// `unproven` when this passes while its signal is present.
    #[serde(skip)]
    unproven_by_ns: Option<u64>,
}

/// A signal step 1 could not tell that decoder rows could still prove, while the watch's
/// own `choose` child tries them with nothing kept (`job_context.watch.choosing`): one per
/// signal, holding a decoder slot until its verdict ([`choosing_ended`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChoosingSignal {
    signal: Signal,
    /// The `choose` child.
    choose: JobId,
    /// The rows it tries: step 1's `could_prove`.
    rows: Vec<String>,
    /// Why step 1 could not tell.
    reason: String,
    since_ns: u64,
}

/// A channel no new decoder starts on until `until_ns` (`job_context.watch.cooled`):
/// the centre and width of a decoder that was `lost` (`why` its loss's), stopped by
/// the operator (`why` [`WHY_STOPPED`]) or that failed (`why` [`WHY_FAILED`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cooled {
    center_hz: u64,
    bandwidth_hz: u64,
    until_ns: u64,
    why: String,
}

/// A known channel (spec 4.3, `job_context.watch.known.channels`): a centre a span
/// watch expected before the detector saw anything, a listed row's centre or a licensed
/// transmitter whose emission fits a listed row. A signal on one ranks first for a free
/// slot and a direct start on a register's names the licensee; with `seed_known` its
/// decoder starts at once.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Known {
    center_hz: u64,
    /// The row's channel width.
    bandwidth_hz: u64,
    /// The listed row it decodes with.
    mode: String,
    from: KnownFrom,
    licensee: Option<String>,
    site: Option<String>,
    distance_km: Option<f64>,
    /// The register's assignments folded into it (0 for a row's centre).
    sites: usize,
    /// The nearest site's assignment, for the `decoding` summary.
    #[serde(skip)]
    allocation: Option<Allocation>,
}

/// Where a known channel comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KnownFrom {
    /// A listed row's `band.centers_hz`.
    Row,
    /// A licence register's transmitter.
    Register,
}

/// The known channel `s` overlaps (by the known's width): a register's first (it names
/// a licensee), then the nearest centre.
fn known_at<'a>(known: &'a [Known], s: &Signal) -> Option<&'a Known> {
    known
        .iter()
        .filter(|k| s.overlaps(k.center_hz, k.bandwidth_hz))
        .min_by_key(|k| (k.allocation.is_none(), k.center_hz.abs_diff(s.center_hz)))
}

/// The `why` of a `lost` decoder whose signal went and that stayed silent for the hold.
const WHY_SILENT: &str = "gone and silent";
/// The `why` of a `lost` re-adopted decoder whose signal was not seen again in time.
const WHY_NOT_SEEN: &str = "not seen again";
/// The `why` of a `lost` direct start that published nothing within `confirm_ms`.
const WHY_UNPROVEN: &str = "unproven";
/// The `why` of a `lost` proved decoder that could not be relaunched with its sink.
const WHY_RELAUNCH: &str = "relaunch failed";
/// The `why` of a cooled channel whose decoder the operator stopped (not a `lost`).
const WHY_STOPPED: &str = "stopped";
/// The `why` of a cooled channel whose decoder ended `Failed` (not a `lost`).
const WHY_FAILED: &str = "failed";

/// What a running watch needs; made at launch from the record ([`Seed`]) and the
/// streams the launch registered.
pub(super) struct Run {
    id: JobId,
    port: Arc<dyn Port>,
    tuning: Tuning,
    prov: Provenance,
    /// The `detect` output: the loop reads its `signal` events and publishes its own.
    out: StreamId,
    rx: Receiver<Frame>,
    cancel: CancellationToken,
    seed: Seed,
}

/// What a launch reads from the watch's record before it is recorded running.
pub(super) struct Seed {
    policy: Policy,
    /// The decoders a run before this one kept (persisted `watch.decoding`).
    adopt: Vec<Decoding>,
    /// The identify a run before a node restart was waiting on.
    stale: Option<JobId>,
    /// The channels a run before this one cooled (persisted `watch.cooled`).
    cooled: Vec<Cooled>,
}

impl Seed {
    pub(super) fn of(rec: &JobRecord) -> Seed {
        let w = &rec.context["watch"];
        Seed {
            policy: Policy::of(&rec.graph),
            adopt: serde_json::from_value(w["decoding"].clone()).unwrap_or_default(),
            stale: w["current"].as_str().map(|s| JobId(s.into())),
            cooled: serde_json::from_value(w["cooled"].clone()).unwrap_or_default(),
        }
    }
}

impl Run {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        id: JobId,
        port: Arc<dyn Port>,
        tuning: Tuning,
        prov: Provenance,
        out: StreamId,
        rx: Receiver<Frame>,
        cancel: CancellationToken,
        seed: Seed,
    ) -> Run {
        Run {
            id,
            port,
            tuning,
            prov,
            out,
            rx,
            cancel,
            seed,
        }
    }
}

/// The loop's state, persisted as `job_context.watch` on every change.
struct State {
    signals: Vec<Tracked>,
    decoding: Vec<Decoding>,
    /// The signals the watch's `choose` children try with nothing kept, reported as
    /// `cap.choosing`.
    choosing: Vec<ChoosingSignal>,
    /// The policy's `max_decoders`, reported as `cap.max`.
    max_decoders: u32,
    /// The channels no new decoder starts on yet.
    cooled: Vec<Cooled>,
    /// A span watch's known channels in the band, built once at the start of a run.
    known: Vec<Known>,
    /// What the register query for `known` asked and was answered (`{"providers",
    /// "assignments"}`); null without `modes`.
    register: serde_json::Value,
    /// The identify run in progress and the centre of the signal it is on.
    current: Option<(JobId, u64)>,
    /// A choosing watch's step 1 in progress (not persisted: a restart asks again).
    classifying: Option<Classifying>,
    /// The decoders' event streams, by job.
    subs: HashMap<JobId, Vec<Receiver<Frame>>>,
    /// A proved decoder while it is placed: neither `current` nor in `decoding` yet.
    pending: Option<JobId>,
    /// The capture children running, with the signal each is on.
    capturing: Vec<(JobId, Signal)>,
    /// The signals this run has captured, with what each capture wrote: a signal that
    /// goes and comes back is not captured again.
    captured: Vec<(Signal, Vec<String>)>,
    /// Set until [`watch_loop`]'s exit path has run: any other end of the state (a panic
    /// unwinding the loop) tears down what it started from a task the drop spawns.
    guard: Option<Guard>,
}

/// What [`State`]'s drop needs to tear down without the loop.
struct Guard {
    inner: Arc<Inner>,
    port: Arc<dyn Port>,
}

impl Drop for State {
    fn drop(&mut self) {
        let Some(Guard { inner, port }) = self.guard.take() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let current = self.current.take().map(|(j, _)| j);
        let pending = self.pending.take();
        let source = port.id().clone();
        let decoders = std::mem::take(&mut self.choosing)
            .into_iter()
            .map(|t| (t.choose, source.clone()))
            .chain(
                std::mem::take(&mut self.decoding)
                    .into_iter()
                    .flat_map(Decoding::teardown),
            )
            .collect();
        let captures = std::mem::take(&mut self.capturing)
            .into_iter()
            .map(|(j, _)| j)
            .collect();
        rt.spawn(async move {
            let source = port.id().clone();
            release(&inner, &source, current, pending, decoders, captures).await;
            inner.leases.idle_if_unheld(port.as_ref()).await;
        });
    }
}

/// Stop what a run of the watch on `source` started: the identify run `current` (and the
/// decoder a proof it landed left, never adopted), a proved decoder being placed,
/// `decoders` (the `choose` children trying signals with nothing kept, then, in order,
/// each kept job's `choose` child, its attached job and the job itself,
/// [`Decoding::teardown`]) and the capture children `captures`. Gated on `source`
/// (the watch's lease is held meanwhile, so there is nothing to idle, and a `replace`
/// holding the gate never waits on it), plainly on another source.
async fn release(
    inner: &Arc<Inner>,
    source: &SourceId,
    current: Option<JobId>,
    pending: Option<JobId>,
    decoders: Vec<(JobId, SourceId)>,
    captures: Vec<JobId>,
) {
    for id in captures {
        let _ = inner.stop_gated(&id).await;
        inner.captured.lock().unwrap().remove(&id);
    }
    if let Some(id) = current {
        let _ = inner.stop_gated(&id).await;
        let verdict = inner.verdicts.lock().unwrap().remove(&id);
        if let Some(job) = verdict
            .as_ref()
            .and_then(|v| v["verdict"]["job"].as_str())
            .map(|s| JobId(s.into()))
        {
            let _ = inner.stop_gated(&job).await;
        }
    }
    if let Some(job) = pending {
        let _ = inner.stop_gated(&job).await;
    }
    for (job, on) in decoders {
        let _ = match on == *source {
            true => inner.stop_gated(&job).await,
            false => inner.stop(&job).await,
        };
        // A `choose` child's verdict, when it ended meanwhile, is nobody's to take.
        inner.verdicts.lock().unwrap().remove(&job);
    }
}

impl Decoding {
    /// What stopping it stops, in order: its `choose` child (no trial outlives it), each
    /// job attached to it (so a transcriber ends while its input is still held, dropping
    /// what it kept, and no rider is left on a channel that is gone), then the job, each
    /// with the source it runs on.
    fn teardown(self) -> Vec<(JobId, SourceId)> {
        let on = self.source;
        (self.choose.into_iter())
            .chain(self.attached)
            .chain([self.job])
            .map(|j| (j, on.clone()))
            .collect()
    }
}

impl State {
    fn context(&self) -> serde_json::Value {
        serde_json::json!({"watch": {
            "signals": self.signals,
            "decoding": self.decoding,
            "choosing": self.choosing,
            "current": self.current.as_ref().map(|(j, _)| j.0.clone()),
            "cooled": self.cooled,
            "known": match &self.register {
                serde_json::Value::Object(r) => {
                    let mut k = r.clone();
                    k.insert("channels".into(), serde_json::json!(self.known));
                    serde_json::Value::Object(k)
                }
                _ => serde_json::Value::Null,
            },
            "cap": {
                "max": self.max_decoders,
                "decoding": self.decoding.len(),
                "waiting": self.signals.iter().filter(|s| s.waiting).count(),
                "choosing": self.choosing.len(),
            },
        }})
    }

    /// Whether every decoder slot is taken: a signal being tried with nothing kept holds
    /// the slot its proof would take.
    fn full(&self) -> bool {
        self.decoding.len() + self.choosing.len() >= self.max_decoders as usize
    }

    /// Drop the cooled channels whose cooldown has passed by `now`; whether any went.
    fn prune_cooled(&mut self, now: u64) -> bool {
        let before = self.cooled.len();
        self.cooled.retain(|c| c.until_ns > now);
        self.cooled.len() != before
    }
}

/// Whether `center_hz` ± `bandwidth_hz`/2 overlaps a channel `w` has cooled until after
/// `now`.
fn cooled_overlaps(w: &State, center_hz: u64, bandwidth_hz: u64, now: u64) -> bool {
    w.cooled.iter().any(|c| {
        c.until_ns > now && 2 * c.center_hz.abs_diff(center_hz) < c.bandwidth_hz + bandwidth_hz
    })
}

/// How `a` ranks against `b` for a free decoder slot (greater first): a signal on a
/// `known` channel first, then the queue's `score`, then SNR.
fn rank(a: &Tracked, b: &Tracked, known: &[Known]) -> Ordering {
    let on_known = |t: &Tracked| known_at(known, &t.signal).is_some();
    on_known(a)
        .cmp(&on_known(b))
        .then(a.score.total_cmp(&b.score))
        .then(a.signal.snr_db.total_cmp(&b.signal.snr_db))
}

/// One `watch` event's fields beyond `state` and `summary`.
#[derive(Default)]
struct Event<'a> {
    signal: Option<&'a Signal>,
    job: Option<&'a JobId>,
    mode: Option<&'a str>,
    source: Option<&'a SourceId>,
    placed: Option<Placed>,
    features: Option<serde_json::Value>,
    /// The register's allocations for an `unknown` (the identify verdict's).
    allocations: Option<serde_json::Value>,
    /// The origins of an `unknown` (the identify verdict's).
    origins: Option<serde_json::Value>,
    /// The captures a `captured` lists.
    captures: Option<&'a [String]>,
    /// How a `decoding` decoder was started.
    via: Option<Via>,
    /// Why a `lost` decoder was stopped: `gone and silent`, `not seen again`,
    /// `unproven` or `relaunch failed`.
    why: Option<String>,
    /// The rows a `choosing` signal's `choose` tries.
    rows: Option<&'a [String]>,
}

impl Inner {
    /// Persist the loop's state as `job_context.watch`.
    fn watch_save(&self, r: &Run, w: &State) {
        let _ = self.context_merge(&r.id, w.context());
    }

    /// Publish one `watch` event on the job's event stream.
    fn watch_event(&self, r: &Run, state: &str, summary: String, e: Event) {
        let t = now();
        let payload = serde_json::json!({
            "protocol": "watch",
            "state": state,
            "signal": e.signal,
            "job": e.job.map(|j| j.0.clone()),
            "mode": e.mode,
            "source": e.source.map(|s| s.0.clone()),
            "placed": e.placed.map(Placed::as_str),
            "features": e.features,
            "allocations": e.allocations,
            "origins": e.origins,
            "captures": e.captures,
            "via": e.via,
            "why": e.why,
            "rows": e.rows,
            "summary": summary,
            "t_ns": t.0,
        });
        let prov = Provenance {
            span: Span {
                start_ns: t,
                end_ns: t,
            },
            ..r.prov.clone()
        };
        self.bus
            .publish(Frame::new(r.out.clone(), prov, Payload::Event(payload)));
        #[cfg(feature = "test-fixtures")]
        test_fault_at(state);
    }

    /// Whether `id` has a running record in this node.
    fn runs(&self, id: &JobId) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|e| e.record.state == JobState::Running && e.cancel.is_some())
    }

    /// Whether `id` ended `Failed`.
    fn failed(&self, id: &JobId) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|e| matches!(e.record.state, JobState::Failed { .. }))
    }

    /// Wait up to [`ADOPT_WAIT`] for `id` to run; `None` when cancelled first.
    async fn wait_running(&self, r: &Run, id: &JobId) -> Option<bool> {
        let deadline = tokio::time::Instant::now() + ADOPT_WAIT;
        loop {
            if self.runs(id) {
                return Some(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Some(false);
            }
            tokio::select! {
                _ = r.cancel.cancelled() => return None,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
    }

    /// Clear `id`'s owner: a decoder the watch keeps is an ordinary job, kept in the
    /// history when it stops.
    pub(super) fn disown(&self, id: &JobId) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(e) = jobs.get_mut(id)
            && e.record.owner.is_some()
        {
            e.record.owner = None;
            let _ = super::persist::persist(&self.jobs_dir, &e.record);
        }
    }

    /// Stop a job the watch started: gated on the watch's own source (the watch's lease
    /// is held meanwhile, so there is nothing to idle, and a `replace` holding the gate
    /// never waits on it), plainly on another source.
    async fn stop_started(&self, r: &Run, job: &JobId, source: &SourceId) {
        let _ = match *source == r.prov.source {
            true => self.stop_gated(job).await,
            false => self.stop(job).await,
        };
    }

    /// [`identify_band`] of `s` with the node's rows, the watch's band and its `modes`.
    fn identify_band(&self, r: &Run, s: &Signal) -> (u64, f32) {
        identify_band(
            &self.modes(),
            r.tuning.bandwidth_hz.0,
            s,
            r.seed.policy.modes.as_deref(),
        )
    }

    /// The rows of the policy's `modes` that fit `s`, with their widths: ready, its
    /// measured width within [`SPAN_WIDTH_FIT`] of the row's band ([`SPAN_FLOOR_FIT`] at
    /// the detector's floor, [`span_fits`]), and the row's band no wider than the
    /// watch's. Empty with `modes: null`.
    fn fitting_rows(&self, r: &Run, s: &Signal) -> Vec<(String, u64)> {
        let Some(list) = &r.seed.policy.modes else {
            return Vec::new();
        };
        let modes = self.modes();
        list.iter()
            .filter_map(|id| {
                let m = modes.get(id)?;
                let width = m.row.band.bandwidth_hz;
                (m.startable()
                    && span_fits(width, s.bandwidth_hz, r.seed.policy.width_floor_hz)
                    && width <= r.tuning.bandwidth_hz.0)
                    .then(|| (id.clone(), width))
            })
            .collect()
    }

    /// The ids of [`Inner::fitting_rows`].
    fn fit_ids(&self, r: &Run, s: &Signal) -> Vec<String> {
        self.fitting_rows(r, s)
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// Whether a running job other than the watch's own (itself, its identify run, its
    /// decoders) holds a band on the watch's source that overlaps `s`.
    fn held_elsewhere(&self, r: &Run, w: &State, s: &Signal) -> bool {
        self.leases
            .holders(&r.prov.source)
            .into_iter()
            .filter(|(j, _)| *j != r.id)
            .filter(|(j, _)| w.current.as_ref().is_none_or(|(c, _)| c != j))
            .filter(|(j, _)| !w.decoding.iter().any(|d| d.job == *j))
            .filter(|(j, _)| !self.owned_by(j, &r.id))
            .any(|(_, t)| s.overlaps(t.center_hz.0, t.bandwidth_hz.0))
    }

    /// Whether `job`'s record names `owner` as its owner.
    fn owned_by(&self, job: &JobId, owner: &JobId) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .get(job)
            .is_some_and(|e| e.record.owner.as_ref() == Some(owner))
    }

    /// Start an owned `identify` run on `t`'s centre and band inside the watch's
    /// capture, recorded as `current` first, restricted to `candidates` when given (in
    /// its `context.identify.candidates`), its rows' dwells scaled by the signal's try
    /// ([`dwell_scale`], in `context.identify.dwell_scale`). The launch runs in its own
    /// task, so a stop of the watch never drops it half way (as identify's own children).
    /// `Ok(None)`: cancelled.
    async fn start_identify(
        self: &Arc<Self>,
        r: &Run,
        w: &mut State,
        t: &Tracked,
        candidates: Option<&[String]>,
    ) -> Result<Option<JobId>, String> {
        let tuning = Tuning {
            center_hz: Hz(t.signal.center_hz),
            bandwidth_hz: Hz(t.band_hz),
        };
        let graph = JobKind::Identify
            .expand(
                &r.prov.source,
                tuning,
                Hz(0),
                None,
                None,
                None,
                &self.modes(),
            )
            .map_err(|e| e.to_string())?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: JobKind::Identify.name().to_string(),
            graph,
            source: r.prov.source.clone(),
            tuning,
            state: JobState::Running,
            context: {
                let mut identify = serde_json::json!({"dwell_scale": dwell_scale(t.tries)});
                if let Some(c) = candidates {
                    identify["candidates"] = c.into();
                }
                serde_json::json!({ "identify": identify })
            },
            created_ns: now(),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner: Some(r.id.clone()),
            audio_policy: None,
            stream: None,
            sinks: Vec::new(),
            handler: None,
            handler_named: false,
            needs: Vec::new(),
            group: None,
            replay: None,
            transient: false,
            held: None,
        };
        let id = rec.id.clone();
        w.current = Some((id.clone(), t.signal.center_hz));
        self.watch_save(r, w);
        let mut launch = self.launch_task(rec);
        tokio::select! {
            res = &mut launch => match res {
                Ok(Ok(())) => Ok(Some(id)),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("launch task: {e}")),
            },
            _ = r.cancel.cancelled() => {
                if self.gated(&r.id) {
                    let inner = self.clone();
                    tokio::spawn(async move {
                        if matches!(launch.await, Ok(Ok(()))) {
                            let _ = inner.stop(&id).await;
                        }
                    });
                } else if matches!(launch.await, Ok(Ok(()))) {
                    let _ = self.stop_gated(&id).await;
                }
                w.current = None;
                Ok(None)
            }
        }
    }

    /// Start `mode` on `source` at `tuning` as an ordinary job: tuned there unless a job
    /// holds the source (the elastic jobs there follow), expanded for the capture, with
    /// what `extra` carries (a choice's parameters, a hold from its start).
    async fn start_on(
        self: &Arc<Self>,
        source: &SourceId,
        mode: &str,
        tuning: Tuning,
        sinks: &[GraphNode],
        extra: Extra,
    ) -> Result<(JobId, SinkOutcome), String> {
        let port = self
            .ports
            .read()
            .unwrap()
            .get(source)
            .cloned()
            .ok_or_else(|| format!("unknown source {}", source.0))?;
        let kind = JobKind::from_name(mode).ok_or_else(|| format!("{mode} is not a job kind"))?;
        let (granted, tuned) = self
            .leases
            .tune_if_unheld_tuned(port.as_ref(), tuning)
            .await
            .map_err(|e| e.to_string())?;
        let bin_rate = self.bank_bin_rate(source, granted);
        // The policy's handler, as a start that names none gets it.
        let modes = self.modes();
        // A job running `fm_demod` on a plan channel of the station's region takes the
        // plan's deviation, as a direct start does, unless the start gives one.
        let deviation = extra.deviation_hz.or_else(|| {
            crate::ops::fm_plan_deviation(&kind, &modes, tuning.center_hz.0, &self.region())
        });
        let graph = self.plain_handler(&kind, &modes).and_then(|handler| {
            let graph = kind.expand_voiced(
                source,
                tuning,
                granted.bandwidth_hz,
                bin_rate,
                (deviation, extra.sideband, None),
                handler.as_deref(),
                &modes,
            )?;
            Ok((graph, handler))
        });
        let started = match graph {
            Ok((mut graph, handler)) => {
                let sink = match self.append_sink(&mut graph, sinks) {
                    Ok(false) => SinkOutcome::Off,
                    Ok(true) => SinkOutcome::On,
                    Err(why) => SinkOutcome::Refused(why),
                };
                // The row's expansion with the sinks, stored beside it: a re-expansion at
                // another capture rate appends them again.
                let sinks = match sink {
                    SinkOutcome::On => sinks.to_vec(),
                    _ => Vec::new(),
                };
                let rec = JobRecord {
                    format: JOB_RECORD_FORMAT,
                    id: JobId::new(),
                    kind: kind.name().to_string(),
                    graph,
                    source: source.clone(),
                    tuning,
                    state: JobState::Running,
                    context: serde_json::json!({}),
                    created_ns: now(),
                    input: None,
                    capture_rate_hz: Some(granted.bandwidth_hz.0),
                    elastic: false,
                    operator_graph: false,
                    owner: None,
                    audio_policy: None,
                    stream: None,
                    sinks,
                    handler,
                    handler_named: false,
                    needs: Vec::new(),
                    group: None,
                    replay: None,
                    transient: false,
                    // A new record: the hold is on it from its first sample.
                    held: extra.held,
                };
                let id = rec.id.clone();
                // In its own task, as identify's children: `launch`'s future runs this.
                match self.launch_task(rec).await {
                    Ok(Ok(())) => Ok((id, sink)),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(e) => Err(format!("launch task: {e}")),
                }
            }
            Err(e) => Err(e.to_string()),
        };
        if tuned {
            let except = started.as_ref().ok().map(|(id, _)| id.clone());
            let _ = self.relaunch_elastic(source, except.as_ref()).await;
        }
        if started.is_err() {
            self.leases.idle_if_unheld(port.as_ref()).await;
        }
        started
    }

    /// Start `mode` directly on the tracked signal `i` (the listed row nearest it) at the
    /// signal's centroid with the row's width and the policy's `sondehub` sink, kept as a
    /// decoder `via: modes` (event `decoding`). Under `decode_on: free` it goes to a free
    /// source as a proved decoder would ([`Inner::start_free`]), else (or when none
    /// takes it) on the watch's own source, inside its capture with no tune. The start
    /// on the watch's source runs in its own task, cancellable as identify's launches
    /// are. A start that fails counts as an unknown try of the signal (event `unknown`)
    /// and is the `Err`; `Ok(None)`: cancelled.
    async fn start_direct(
        self: &Arc<Self>,
        r: &Run,
        w: &mut State,
        i: usize,
        mode: &str,
    ) -> Result<Option<JobId>, String> {
        let signal = w.signals[i].signal.clone();
        let mut why = None;
        if r.seed.policy.free {
            match self.start_free(r, mode, &signal).await {
                Ok((job, source, sink)) => {
                    let placed = (source, Placed::Free, None);
                    self.keep_direct(r, w, i, mode, &job, placed, sink);
                    return Ok(Some(job));
                }
                Err(e) => why = Some(e),
            }
            if r.cancel.is_cancelled() {
                return Ok(None);
            }
        }
        let tuning = Tuning {
            center_hz: Hz(signal.center_hz),
            bandwidth_hz: Hz(self.row_width(mode, &signal)),
        };
        let (inner, source, m) = (self.clone(), r.prov.source.clone(), mode.to_string());
        let sinks = r.seed.policy.sinks.clone();
        let mut launch = tokio::spawn(async move {
            let extra = Extra::default();
            inner.start_on(&source, &m, tuning, &sinks, extra).await
        });
        let started = tokio::select! {
            res = &mut launch => res.unwrap_or_else(|e| Err(format!("launch task: {e}"))),
            _ = r.cancel.cancelled() => {
                if self.gated(&r.id) {
                    // The gate of the radio is not ours: finish and stop it from a task.
                    let inner = self.clone();
                    tokio::spawn(async move {
                        if let Ok(Ok((job, _))) = launch.await {
                            let _ = inner.stop(&job).await;
                        }
                    });
                } else if let Ok(Ok((job, _))) = launch.await {
                    let _ = self.stop_gated(&job).await;
                }
                return Ok(None);
            }
        };
        let (job, sink) = match started {
            Ok(s) => s,
            Err(e) => {
                count_try(&mut w.signals[i], now().0);
                let summary = format!("unknown at {}: {mode} did not start: {e}", signal.mhz());
                let ev = Event {
                    signal: Some(&signal),
                    mode: Some(mode),
                    ..Event::default()
                };
                self.watch_event(r, "unknown", summary, ev);
                return Err(e);
            }
        };
        let placed = (r.prov.source.clone(), Placed::Capture, why);
        self.keep_direct(r, w, i, mode, &job, placed, sink);
        Ok(Some(job))
    }

    /// Keep the directly started decoder `job` of the tracked signal `i`, placed on
    /// `(source, placed, why it fell back to the capture)`: listed in `decoding` `via:
    /// modes`, its events followed, and the event `decoding` published.
    #[allow(clippy::too_many_arguments)]
    fn keep_direct(
        &self,
        r: &Run,
        w: &mut State,
        i: usize,
        mode: &str,
        job: &JobId,
        (source, placed, why): (SourceId, Placed, Option<String>),
        sink: SinkOutcome,
    ) {
        let sink_error = sink_error_of(sink);
        let t = now().0;
        let tracked = &mut w.signals[i];
        tracked.handled = true;
        tracked.waiting = false;
        let gone_since_ns = (!tracked.present).then_some(t);
        let confirm_ns = r.seed.policy.confirm_ms.saturating_mul(1_000_000);
        let unproven_by_ns = (confirm_ns > 0).then(|| t.saturating_add(confirm_ns));
        let signal = tracked.signal.clone();
        let register = known_at(&w.known, &signal)
            .and_then(|k| k.allocation.as_ref())
            .map(|a| register_says(&serde_json::json!([a])))
            .unwrap_or_default();
        w.subs.insert(job.clone(), self.decoder_subs(job));
        w.decoding.push(Decoding {
            mode: mode.to_string(),
            job: job.clone(),
            source: source.clone(),
            signal: signal.clone(),
            since_ns: t,
            placed,
            via: Via::Modes,
            sink_error: sink_error.clone(),
            attached: Vec::new(),
            held: false,
            choose: None,
            last_event_ns: t,
            squelch_open: None,
            gone_since_ns,
            confirm_by_ns: None,
            unproven_by_ns,
        });
        let mut summary = format!(
            "decoding {mode} at {} on {} ({}, modes){register}",
            signal.mhz(),
            source.0,
            placed.as_str()
        );
        if let Some(why) = why {
            summary.push_str(&format!(": {why}"));
        }
        if let Some(why) = sink_error {
            summary.push_str(&format!("; sink refused: {why}"));
        }
        let e = Event {
            signal: Some(&signal),
            job: Some(job),
            mode: Some(mode),
            source: Some(&source),
            placed: Some(placed),
            via: Some(Via::Modes),
            ..Event::default()
        };
        self.watch_event(r, "decoding", summary, e);
    }

    /// The band a choosing watch classifies for the tracked signal `t`: its centre, as
    /// wide as the band its identify run would take or twice its measured width (its
    /// skirts and the rows a decoder could prove it by), within the watch's band and
    /// inside the ring ([`Inner::inside_ring`]).
    fn choice_band(&self, r: &Run, t: &Tracked) -> Tuning {
        let width = t
            .band_hz
            .max(t.signal.bandwidth_hz.saturating_mul(2))
            .min(r.tuning.bandwidth_hz.0);
        let center_hz = t.signal.center_hz;
        let width = self.inside_ring(&r.prov.source, center_hz, width, t.signal.bandwidth_hz);
        Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(width),
        }
    }

    /// `width` about `center_hz`, narrowed to what `source`'s ring holds around that
    /// centre ([`ring_clamped`]) but never under `floor_hz` (the signal's measured width):
    /// a wide signal near the capture's edge is still read and started.
    fn inside_ring(&self, source: &SourceId, center_hz: u64, width: u64, floor_hz: u64) -> u64 {
        let ring = self.rings.lock().unwrap().get(source).cloned();
        let span = ring.and_then(|ring| {
            let ring = ring.read().unwrap();
            Some((ring.center_hz()?.0, ring.sample_rate_hz().0))
        });
        match span {
            Some(span) => ring_clamped(width, floor_hz, center_hz, span),
            None => width,
        }
    }

    /// Wait for the start `launch` (in its own task), cancellable as identify's launches
    /// are: when the watch is stopped first, the start is finished and the job it started
    /// (`job` names it) stopped from a task, and the answer is `Ok(None)`.
    async fn launched<T: Send + 'static>(
        self: &Arc<Self>,
        r: &Run,
        mut launch: tokio::task::JoinHandle<Result<T, String>>,
        job: fn(&T) -> JobId,
    ) -> Result<Option<T>, String> {
        tokio::select! {
            res = &mut launch => res.unwrap_or_else(|e| Err(format!("launch task: {e}"))).map(Some),
            _ = r.cancel.cancelled() => {
                if self.gated(&r.id) {
                    // The gate of the radio is not ours: finish and stop it from a task.
                    let inner = self.clone();
                    tokio::spawn(async move {
                        if let Ok(Ok(t)) = launch.await {
                            let _ = inner.stop(&job(&t)).await;
                        }
                    });
                } else if let Ok(Ok(t)) = launch.await {
                    let _ = self.stop_gated(&job(&t)).await;
                }
                Ok(None)
            }
        }
    }

    /// Start the choice's `job` inside the watch's capture with the policy's sinks, under
    /// `held` from its first sample, its band kept inside the ring but never under
    /// `floor_hz` (the signal's measured width) unless the choice's own band is narrower.
    /// `Ok(None)`: cancelled.
    async fn start_chosen(
        self: &Arc<Self>,
        r: &Run,
        job: &ChoiceJob,
        held: Option<Hold>,
        floor_hz: u64,
    ) -> Result<Option<(JobId, SinkOutcome)>, String> {
        let (center, width) = (job.center_hz.0, job.bandwidth_hz.0);
        let width = self.inside_ring(&r.prov.source, center, width, floor_hz.min(width));
        let tuning = Tuning {
            center_hz: job.center_hz,
            bandwidth_hz: Hz(width),
        };
        let extra = Extra {
            deviation_hz: job
                .params
                .get("deviation_hz")
                .and_then(|v| v.as_u64())
                .map(Hz),
            sideband: job
                .params
                .get("sideband")
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            held,
        };
        let (inner, source, kind) = (self.clone(), r.prov.source.clone(), job.kind.clone());
        let sinks = r.seed.policy.sinks.clone();
        let start =
            tokio::spawn(
                async move { inner.start_on(&source, &kind, tuning, &sinks, extra).await },
            );
        self.launched(r, start, |(id, _)| id.clone()).await
    }

    /// Attach to `job`, a job of the row `mode` the watch keeps, what applies of the
    /// policy's `attach` and of `proved` (the riders its choice proved, the `ChoiceJob`'s
    /// `attach`; spec 49.3), in that order: a transcriber on its main audio (none for a
    /// job that gives no audio, a decoder row with no voice), then each rider the policy
    /// names whose row rides with `mode`, then each proved rider not named
    /// ([`Inner::ride`]). The jobs attached, in order, and a note for each that did not
    /// attach: `not attached: <why>` for the transcriber, `<row> did not attach: <why>`
    /// for a rider. Nothing more once the watch is stopped (an attach under way is then
    /// stopped from a task).
    async fn attach_to(
        self: &Arc<Self>,
        r: &Run,
        job: &JobId,
        mode: &str,
        proved: &[String],
    ) -> (Vec<JobId>, Vec<String>) {
        let mut out = (Vec::new(), Vec::new());
        let policy = &r.seed.policy.attach;
        if policy.contains(&JobKind::Transcribe) {
            let (id, why) = self.attach_transcriber(r, job).await;
            out.0.extend(id);
            out.1.extend(why.map(|why| format!("not attached: {why}")));
        }
        let named = (policy.iter())
            .filter(|k| **k != JobKind::Transcribe && self.rides_with(k, mode))
            .map(|k| k.name().to_string());
        let mut riders: Vec<String> = named.collect();
        for row in proved {
            if !riders.contains(row) {
                riders.push(row.clone());
            }
        }
        for row in riders {
            if r.cancel.is_cancelled() {
                break;
            }
            match self.ride(r, job, &row, &[]).await {
                Ride::Attached(id) => out.0.push(id),
                Ride::Refused(why) => out.1.push(why),
                Ride::Riding(_) | Ride::Cancelled => {}
            }
        }
        out
    }

    /// Attach a transcriber to `job`'s main audio: the attached job, or why it is not
    /// attached. Nothing when `job` gives no audio; nothing either when the watch is
    /// stopped meanwhile (the attached job is then stopped from a task).
    async fn attach_transcriber(
        self: &Arc<Self>,
        r: &Run,
        job: &JobId,
    ) -> (Option<JobId>, Option<String>) {
        let kind = JobKind::Transcribe;
        if self.audio_stream(job, None).is_err() {
            return (None, None);
        }
        if let Some(ready) = self.registry.readiness(kind.name())
            && !ready.ready
        {
            let why = ready.reason.unwrap_or_else(|| "not ready".into());
            return (None, Some(format!("{}: {why}", kind.name())));
        }
        let attach = attach_task(self, kind, job.clone());
        match self.launched(r, attach, JobId::clone).await {
            Ok(Some(id)) => (Some(id), None),
            Ok(None) => (None, None),
            Err(e) => (None, Some(e)),
        }
    }

    /// Whether the rider `kind` (a row that `rides`, whatever its state) rides with the
    /// row `mode` (by its `rides.with`, a variant row of a named demodulator included).
    fn rides_with(&self, kind: &JobKind, mode: &str) -> bool {
        let modes = self.modes();
        let (Some(rider), Some(demod)) = (crate::ops::rider_row(&modes, kind), modes.get(mode))
        else {
            return false;
        };
        crate::choice::rides_with(&modes, rider, demod)
    }

    /// Attach the rider row `row` to `job` ([`crate::ops::rider_attach`]), unless a running
    /// job of that row already rides on it: [`Ride::Riding`] names it (the watch's own
    /// when it is in `own`). A refusal or a failed launch is [`Ride::Refused`], `<row> did
    /// not attach: <why>`; the job runs on either way.
    async fn ride(self: &Arc<Self>, r: &Run, job: &JobId, row: &str, own: &[JobId]) -> Ride {
        let Some(kind) = JobKind::from_name(row) else {
            return Ride::Refused(format!("{row} did not attach: no row {row}"));
        };
        if let Some(by) = crate::ops::attached_to(&Node::handle(self), job, &kind) {
            return Ride::Riding(if own.contains(&by) { Ok(by) } else { Err(by) });
        }
        match self
            .launched(r, attach_task(self, kind, job.clone()), JobId::clone)
            .await
        {
            Ok(Some(id)) => Ride::Attached(id),
            Ok(None) => Ride::Cancelled,
            Err(e) => Ride::Refused(format!("{row} did not attach: {e}")),
        }
    }

    /// Start the watch's own `choose` on `band`: owned by the watch, transient, asked by
    /// the `watch` surface to the policy's step for it, handed the watch's step 1
    /// (`seed`, so it classifies nothing again), its verdict left in `Inner::verdicts`
    /// for [`settle`]. `Ok(None)`: cancelled.
    async fn start_choose(
        self: &Arc<Self>,
        r: &Run,
        band: Tuning,
        seed: StepOne,
    ) -> Result<Option<JobId>, String> {
        let (step, capped) = self
            .choice_policy
            .lock()
            .unwrap()
            .steps(Surface::Watch, None);
        let ask = ChooseAsk {
            surface: Surface::Watch,
            step,
            capped,
            start: false,
            transient: true,
            span: None,
            surface_note: None,
        };
        let rec = choose_record(r.prov.source.clone(), band, ask, Some(r.id.clone()));
        let id = rec.id.clone();
        self.choosing_seed(&id, seed);
        let launch = self.launch_task(rec);
        let child = id.clone();
        let start = tokio::spawn(async move {
            match launch.await {
                Ok(Ok(())) => Ok(child),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("launch task: {e}")),
            }
        });
        let started = self.launched(r, start, JobId::clone).await;
        // Taken as the child starts; one that never started leaves nothing behind.
        self.choosing_unseed(&id);
        started
    }

    /// `on_signal: choose`: begin step 1 for the tracked signal `i`: its
    /// [`Inner::choice_band`] over the newest `budget.slice_s` of the ring, classified and
    /// chosen as `classify` does, in a task of its own ([`classify_task`]), so the loop
    /// goes on following `detect` and its decoders while the ring is read and the
    /// register asked. Like an identify run, one at a time: [`chosen`] takes the answer.
    fn start_choice(self: &Arc<Self>, r: &Run, w: &mut State, i: usize) {
        let tracked = &mut w.signals[i];
        tracked.waiting = false;
        let band = self.choice_band(r, tracked);
        let slice_s = self.choice_policy.lock().unwrap().budget.slice_s;
        w.classifying = Some(Classifying {
            center_hz: tracked.signal.center_hz,
            band,
            task: classify_task(self, &r.prov.source, band, slice_s),
        });
    }

    /// Act on the step-1 `answer` for the tracked signal `i` (classified on `band`): start
    /// the answer's job inside the capture, kept `via: choose`. An answer that is an
    /// unproved demodulator with rows to try ([`rows_to_try`]) starts held ([`PROBATION`])
    /// under `probation`. The policy's `attach` kind is attached to it, then, while a
    /// decoder could still prove otherwise, the watch's own `choose` starts on the band
    /// ([`settle`] takes its verdict); one that cannot start leaves the first answer
    /// standing, its hold lifted. An answer with no job but rows to try keeps nothing and
    /// has them tried ([`Inner::start_choosing`]) when the policy's step for the watch, as
    /// step 1 read it ([`Classified`]), reaches step 2. A band nothing answers
    /// (`cannot_tell` with nothing to try, a read that fails) or a job that does not start
    /// counts as an unknown try (event `unknown`). Event `decoding`. `None`: cancelled.
    async fn start_answer(
        self: &Arc<Self>,
        r: &Run,
        w: &mut State,
        (i, band): (usize, Tuning),
        answer: Result<Classified, String>,
    ) -> Option<()> {
        let signal = w.signals[i].signal.clone();
        let (seed, rows) = match answer {
            Ok(Classified { step: s, widen, .. }) if s.choice.job.is_some() => {
                let rows = rows_to_try(&s.choice, widen);
                (s, rows)
            }
            Ok(Classified {
                step: s,
                climb,
                widen,
            }) if climb && !rows_to_try(&s.choice, widen).is_empty() => {
                let rows = rows_to_try(&s.choice, widen);
                return self.start_choosing(r, w, (i, band), s, rows).await;
            }
            Ok(Classified { step: s, .. }) => {
                let why = s.choice.reason.unwrap_or_else(|| "no job".into());
                self.choice_unknown(r, w, i, format!("can't tell: {why}"))
                    .await;
                return Some(());
            }
            Err(e) => {
                self.choice_unknown(r, w, i, format!("not classified: {e}"))
                    .await;
                return Some(());
            }
        };
        let choice = seed.choice.clone();
        let job = choice.job.clone()?;
        let demod = self
            .modes()
            .get(&job.kind)
            .is_some_and(|m| m.role() == Role::Demodulator);
        let unproved = choice.settled != Settled::Proved && !rows.is_empty();
        let held = (r.seed.policy.probation && demod && unproved).then(|| Hold {
            reason: PROBATION.into(),
            since_ns: now().0,
        });
        let (k, attach_notes, sink_error) =
            match self.keep_chosen(r, w, i, &job, held.clone()).await? {
                Ok(kept) => kept,
                Err(e) => {
                    let why = format!("{} did not start: {e}", job.kind);
                    self.choice_unknown(r, w, i, why).await;
                    return Some(());
                }
            };
        let id = w.decoding[k].job.clone();
        let mut notes = Vec::new();
        if unproved {
            match self.start_choose(r, band, seed).await {
                Ok(None) => return None,
                Ok(Some(c)) => w.decoding[k].choose = Some(c),
                Err(e) => {
                    // Nothing will settle it: the first answer stands.
                    if held.is_some() {
                        let _ = self.set_held(&id, None);
                        w.decoding[k].held = false;
                    }
                    notes.push(format!("choose did not start: {e}"));
                }
            }
        }
        let mut summary = chosen_summary(&job.kind, &signal, &r.prov.source, &choice);
        if w.decoding[k].held {
            summary.push_str(&format!(
                "; held on {PROBATION} while its choose tries {}",
                rows.join(", ")
            ));
        }
        notes.extend(kept_notes(attach_notes, sink_error));
        for n in notes {
            summary.push_str(&format!("; {n}"));
        }
        self.decoding_event(r, &w.decoding[k], summary);
        Some(())
    }

    /// Start the choice's `job` for the tracked signal `i` inside the capture under `held`
    /// ([`Inner::start_chosen`]), keep it `via: choose`, then attach what applies of the
    /// policy's `attach` and the job's proved riders to it ([`Inner::attach_to`]): its
    /// index in `decoding`, the notes of what did not attach and why its sink was
    /// refused. `Some(Err)`: it did not start; `None`: cancelled.
    async fn keep_chosen(
        self: &Arc<Self>,
        r: &Run,
        w: &mut State,
        i: usize,
        job: &ChoiceJob,
        held: Option<Hold>,
    ) -> Option<Result<(usize, Vec<String>, Option<String>), String>> {
        let signal = w.signals[i].signal.clone();
        let floor_hz = signal.bandwidth_hz;
        let (id, sink) = match self.start_chosen(r, job, held.clone(), floor_hz).await {
            Ok(None) => return None,
            Ok(Some(started)) => started,
            Err(e) => return Some(Err(e)),
        };
        let t = now().0;
        let tracked = &mut w.signals[i];
        tracked.handled = true;
        tracked.waiting = false;
        let gone_since_ns = (!tracked.present).then_some(t);
        let sink_error = sink_error_of(sink);
        w.subs.insert(id.clone(), self.decoder_subs(&id));
        // Kept before anything is attached: a stop of the watch now tears it all down.
        w.decoding.push(Decoding {
            mode: job.kind.clone(),
            job: id.clone(),
            source: r.prov.source.clone(),
            signal,
            since_ns: t,
            placed: Placed::Capture,
            via: Via::Choose,
            sink_error: sink_error.clone(),
            attached: Vec::new(),
            held: held.is_some(),
            choose: None,
            last_event_ns: t,
            squelch_open: None,
            gone_since_ns,
            confirm_by_ns: None,
            unproven_by_ns: None,
        });
        let k = w.decoding.len() - 1;
        let (attached, attach_notes) = self.attach_to(r, &id, &job.kind, &job.attach).await;
        w.decoding[k].attached = attached;
        if r.cancel.is_cancelled() {
            return None;
        }
        Some(Ok((k, attach_notes, sink_error)))
    }

    /// Event `decoding` for the kept job `d`, started by the choice.
    fn decoding_event(&self, r: &Run, d: &Decoding, summary: String) {
        let e = Event {
            signal: Some(&d.signal),
            job: Some(&d.job),
            mode: Some(&d.mode),
            source: Some(&r.prov.source),
            placed: Some(Placed::Capture),
            via: Some(Via::Choose),
            ..Event::default()
        };
        self.watch_event(r, "decoding", summary, e);
    }

    /// Step 1's answer `seed` for the tracked signal `i` has no job, but decoder rows could
    /// still prove it (`rows`, [`rows_to_try`]: `could_prove`, for `unknown` the rows its
    /// width fits, and under `widen` the rows set aside at the capture's edge): nothing is
    /// kept, and the watch's own `choose` starts on `band` to try them, handed the seed
    /// ([`Inner::start_choose`]; [`choosing_ended`] takes its verdict). The signal is handled while
    /// it runs. Event `choosing`, naming the rows (`rows`). A `choose` that does not start
    /// counts an unknown try. `None`: cancelled.
    async fn start_choosing(
        self: &Arc<Self>,
        r: &Run,
        w: &mut State,
        (i, band): (usize, Tuning),
        seed: StepOne,
        rows: Vec<String>,
    ) -> Option<()> {
        let reason = (seed.choice.reason.clone()).unwrap_or_else(|| "no job".into());
        let choose = match self.start_choose(r, band, seed).await {
            Ok(None) => return None,
            Ok(Some(c)) => c,
            Err(e) => {
                let why = format!("can't tell: {reason}; choose did not start: {e}");
                self.choice_unknown(r, w, i, why).await;
                return Some(());
            }
        };
        let tracked = &mut w.signals[i];
        tracked.handled = true;
        tracked.waiting = false;
        let signal = tracked.signal.clone();
        let summary = format!(
            "choosing at {}: can't tell ({reason}); its choose tries {}",
            signal.mhz(),
            rows.join(", ")
        );
        let e = Event {
            signal: Some(&signal),
            job: Some(&choose),
            source: Some(&r.prov.source),
            rows: Some(&rows),
            ..Event::default()
        };
        self.watch_event(r, "choosing", summary, e);
        w.choosing.push(ChoosingSignal {
            signal,
            choose,
            rows,
            reason,
            since_ns: now().0,
        });
        Some(())
    }

    /// The `choose` trying the tracked signal `i` proved `job` (`choice`): start it as
    /// step 1's job would have started, unheld (event `decoding`); one that does not start
    /// counts an unknown try. `None`: cancelled.
    async fn start_proved(
        self: &Arc<Self>,
        r: &Run,
        w: &mut State,
        i: usize,
        (choice, job): (&Choice, &ChoiceJob),
    ) -> Option<()> {
        let (k, attach_notes, sink_error) = match self.keep_chosen(r, w, i, job, None).await? {
            Ok(kept) => kept,
            Err(e) => {
                let why = format!("{} did not start: {e}", job.kind);
                self.choice_unknown(r, w, i, why).await;
                return Some(());
            }
        };
        let d = &w.decoding[k];
        let mut summary = chosen_summary(&d.mode, &d.signal, &r.prov.source, choice);
        for n in kept_notes(attach_notes, sink_error) {
            summary.push_str(&format!("; {n}"));
        }
        self.decoding_event(r, d, summary);
        Some(())
    }

    /// A choosing watch's tracked signal `i` got no job (`why`): `unknown` at once,
    /// counted as a try and retried after [`RETRY_MS`] as an identify's unknown, and
    /// captured under `on_unknown: capture`.
    async fn choice_unknown(self: &Arc<Self>, r: &Run, w: &mut State, i: usize, why: String) {
        let tracked = &mut w.signals[i];
        tracked.handled = false;
        count_try(tracked, now().0);
        let signal = tracked.signal.clone();
        let summary = format!("unknown at {}: {why}", signal.mhz());
        let e = Event {
            signal: Some(&signal),
            ..Event::default()
        };
        self.watch_event(r, "unknown", summary, e);
        capture::on_unknown(self, r, w, i).await;
    }

    /// `mode`'s row width, else the signal's own.
    fn row_width(&self, mode: &str, signal: &Signal) -> u64 {
        self.modes()
            .get(mode)
            .map_or(signal.bandwidth_hz, |m| m.row.band.bandwidth_hz)
    }

    /// Start `mode` on the signal's centre and the row's width on a free source other
    /// than the watch's (`prefer` first, then `source list` order), trying the next on a
    /// refusal (a `conflict`: not free after all). The job, its source and what came of
    /// the sink; else why none took it (`no free source`, or `no free source took it
    /// (<source>: <why>; ...)`).
    async fn start_free(
        self: &Arc<Self>,
        r: &Run,
        mode: &str,
        signal: &Signal,
    ) -> Result<(JobId, SourceId, SinkOutcome), String> {
        let width = self.row_width(mode, signal);
        let ports: Vec<Arc<dyn Port>> = {
            let mut v: Vec<_> = self.ports.read().unwrap().values().cloned().collect();
            v.sort_by(|a, b| a.id().0.cmp(&b.id().0));
            v
        };
        let mut free: Vec<SourceId> = self
            .leases
            .free_sources(&ports, signal.center_hz, width)
            .into_iter()
            .filter(|s| *s != r.prov.source)
            .collect();
        free.sort_by_key(|s| {
            r.seed
                .policy
                .prefer
                .iter()
                .position(|p| p == s)
                .unwrap_or(usize::MAX)
        });
        let tuning = Tuning {
            center_hz: Hz(signal.center_hz),
            bandwidth_hz: Hz(width),
        };
        let mut refused = Vec::new();
        for source in free {
            match self
                .start_on(
                    &source,
                    mode,
                    tuning,
                    &r.seed.policy.sinks,
                    Extra::default(),
                )
                .await
            {
                Ok((job, sink)) => return Ok((job, source, sink)),
                Err(e) => refused.push(format!("{}: {e}", source.0)),
            }
        }
        Err(match refused.is_empty() {
            true => "no free source".to_string(),
            false => format!("no free source took it ({})", refused.join("; ")),
        })
    }

    /// Place a decoder identify proved (`proved`, running `mode` inside the watch's
    /// capture) by the watch's policy: `capture` keeps it there; `free` starts `mode` on
    /// a free source ([`Inner::start_free`]), then stops `proved`; with none it stays in
    /// the capture. The decoder's job, source, placement and, when it fell back, why, with
    /// what came of giving it the sink.
    async fn place_decoder(
        self: &Arc<Self>,
        r: &Run,
        mode: &str,
        signal: &Signal,
        proved: &JobId,
    ) -> Placement {
        let kept = |why: Option<String>, sink: SinkOutcome| Placement {
            job: proved.clone(),
            source: r.prov.source.clone(),
            placed: Placed::Capture,
            why,
            sink,
        };
        if !r.seed.policy.free {
            let sink = self.sink_into(r, proved, &r.seed.policy.sinks).await;
            return kept(None, sink);
        }
        let why = match self.start_free(r, mode, signal).await {
            Ok((job, source, sink)) => {
                self.stop_started(r, proved, &r.prov.source).await;
                return Placement {
                    job,
                    source,
                    placed: Placed::Free,
                    why: None,
                    sink,
                };
            }
            Err(why) => why,
        };
        let sink = self.sink_into(r, proved, &r.seed.policy.sinks).await;
        kept(Some(why), sink)
    }

    /// Append `sinks` to `graph`, fed by the event output of its decoder
    /// ([`graph::append_sinks`]). `Ok(false)` when there is nothing to do (no sinks, or no
    /// node with an event output); `Err` with the reason, the graph left as it was, when
    /// the graph with them would not validate (the start and put of the watch check the
    /// sinks first, so this is the last line of defence: the decoder runs without the
    /// feed and the caller says so).
    ///
    /// A `sink_sondehub` whose params give no whole position (`lat`, `lon`, `alt_m`)
    /// needs the node's station for the rest: without one it is refused here, so the
    /// decoder runs without it and the `decoding` summary says why, rather than the stage
    /// idling unseen.
    fn append_sink(&self, graph: &mut Graph, sinks: &[GraphNode]) -> Result<bool, String> {
        for s in sinks.iter().filter(|s| s.kind == SONDEHUB_STAGE) {
            let has = |k: &str| s.params.get(k).is_some_and(serde_json::Value::is_number);
            if !(has("lat") && has("lon") && has("alt_m")) && self.station.lock().unwrap().is_none()
            {
                return Err(SINK_NO_STATION.into());
            }
        }
        graph::append_sinks(graph, sinks, &self.registry).map_err(|e| e.to_string())
    }

    /// Give the running decoder `job` the sinks `sinks` in place of those its record
    /// carries (`graph_put`'s restart under the same id), when they differ: an identify
    /// child that has none, or a re-adopted decoder kept under another policy. The
    /// restart is cancellable like identify's child launches: when the watch is stopped
    /// meanwhile (a `replace` holds the gate of the radio and waits for the watch), the
    /// launch is finished and the decoder stopped by a task. A launch that fails puts the
    /// graph without sinks back ([`SinkOutcome::Refused`]); when that fails too the
    /// decoder is [`SinkOutcome::Lost`]. An operator's graph (and a decoder kept with a
    /// sink before the sinks were stored beside its graph) runs as stored: `Off`.
    async fn sink_into(self: &Arc<Self>, r: &Run, job: &JobId, sinks: &[GraphNode]) -> SinkOutcome {
        #[cfg(feature = "test-fixtures")]
        if !sink_hold(&r.cancel).await {
            return SinkOutcome::Cancelled;
        }
        // The decoder's restart lock before its record is read, held through both
        // relaunches (the order: restart lock, then the source's gate, which the stop
        // and the launch take inside): an operator's `graph_put` of it waits its turn.
        // Given up when the watch is stopped: a capture move holds the lock while it
        // relaunches the watch in place, and waits for this run to end.
        let _restart = tokio::select! {
            g = self.restart_guard(job) => g,
            _ = r.cancel.cancelled() => return SinkOutcome::Cancelled,
        };
        let Some(rec) = self.jobs.lock().unwrap().get(job).map(|e| e.record.clone()) else {
            return SinkOutcome::Off;
        };
        if rec.operator_graph {
            return SinkOutcome::Off;
        }
        let base = graph::strip_sinks(&rec.graph, &rec.sinks);
        let mut graph = base.clone();
        let (wanted, refused) = match self.append_sink(&mut graph, sinks) {
            Ok(true) => (sinks.to_vec(), None),
            Ok(false) => (Vec::new(), None),
            Err(why) => (Vec::new(), Some(why)),
        };
        let on = !wanted.is_empty();
        let outcome = move |refused: Option<String>| match refused {
            Some(why) => SinkOutcome::Refused(why),
            None if on => SinkOutcome::On,
            None => SinkOutcome::Off,
        };
        if wanted == rec.sinks {
            return outcome(refused);
        }
        // Stopping an owned job forgets its record: it is the watch's ordinary decoder
        // from here on (the caller disowns it too).
        self.disown(job);
        // Still the row's expansion at the decoder's capture rate, the sinks beside it.
        let rate = Some(rec.capture_rate_hz);
        let error = match self.relaunch(r, job, graph, wanted, rate, true).await {
            Ok(true) => return outcome(refused),
            Ok(false) => return SinkOutcome::Cancelled,
            Err(e) => format!("sink restart failed ({e})"),
        };
        // The stop happened and the new graph would not launch: the decoder without sinks.
        match self.relaunch(r, job, base, Vec::new(), rate, false).await {
            Ok(true) => SinkOutcome::Refused(error),
            Ok(false) => SinkOutcome::Cancelled,
            Err(e) => SinkOutcome::Lost(format!("{error}; relaunch without it failed ({e})")),
        }
    }

    /// Store `graph` as `job`'s, expanded for `capture_rate_hz` with `sinks` (stopping it
    /// first when `stop`, the restart that changes them) and launch it, cancellable.
    /// `Ok(false)` when the watch was cancelled meanwhile (a task then finishes the
    /// launch and stops the job).
    async fn relaunch(
        self: &Arc<Self>,
        r: &Run,
        job: &JobId,
        graph: Graph,
        sinks: Vec<GraphNode>,
        capture_rate_hz: Option<Option<u64>>,
        stop: bool,
    ) -> Result<bool, String> {
        let rec = self
            .store_graph(
                job,
                graph,
                capture_rate_hz,
                stop.then_some(true),
                Some(sinks),
            )
            .await
            .map_err(|e| e.to_string())?;
        #[cfg(feature = "test-fixtures")]
        if sink_launch_fails() {
            return Err("test: sink launch refused".into());
        }
        let mut launch = self.launch_task(rec);
        tokio::select! {
            res = &mut launch => match res {
                Ok(Ok(())) => Ok(true),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("launch task: {e}")),
            },
            _ = r.cancel.cancelled() => {
                let (inner, job) = (self.clone(), job.clone());
                if self.gated(&r.id) {
                    // The gate of the radio is not ours: finish and stop it from a task.
                    tokio::spawn(async move {
                        if matches!(launch.await, Ok(Ok(()))) {
                            let _ = inner.stop(&job).await;
                        }
                    });
                } else if matches!(launch.await, Ok(Ok(()))) {
                    let _ = self.stop_gated(&job).await;
                }
                Ok(false)
            }
        }
    }

    /// Subscribe to `job`'s event outputs, for its silence.
    fn decoder_subs(&self, job: &JobId) -> Vec<Receiver<Frame>> {
        let jobs = self.jobs.lock().unwrap();
        jobs.get(job)
            .map(|e| e.outputs.clone())
            .unwrap_or_default()
            .iter()
            .filter(|o| o.kind == StreamKind::Event)
            .filter_map(|o| self.bus.subscribe(&o.id))
            .collect()
    }

    /// Whether the watch's run is ending for a `graph_put` restart (set before its stop),
    /// after which the next run re-adopts its decoders.
    fn restarting(&self, id: &JobId) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|e| e.restarting)
    }
}

/// The `watch` job's controller, one of the job's supervised tasks beside its `fft` and
/// `detect` stages: (1) stop a stale identify a run before a restart left and re-adopt
/// the persisted decoders that still run; (2) follow `detect`'s `signal` events, queueing
/// each new one that no other job's band overlaps; (3) identify the queued signals one at
/// a time, best score first, and place each proof; (4) retry unknown signals, stop lost
/// decoders. See the module docs for the exits.
pub(super) async fn watch_loop(inner: Arc<Inner>, mut r: Run) -> Result<(), StageError> {
    let mut w = State {
        signals: Vec::new(),
        decoding: Vec::new(),
        choosing: Vec::new(),
        max_decoders: r.seed.policy.max_decoders,
        cooled: std::mem::take(&mut r.seed.cooled),
        known: Vec::new(),
        register: serde_json::Value::Null,
        current: None,
        classifying: None,
        subs: HashMap::new(),
        pending: None,
        capturing: Vec::new(),
        captured: Vec::new(),
        guard: Some(Guard {
            inner: inner.clone(),
            port: r.port.clone(),
        }),
    };
    let result = run(&inner, &mut r, &mut w).await;
    w.guard = None;
    // A step 1 still reading the ring has started nothing: it is dropped.
    if let Some(c) = w.classifying.take() {
        c.task.abort();
    }
    // At the node's shutdown everything is left as it is: the records resume, the next
    // run re-adopts the decoders and stops the identify run as stale. A `graph_put`
    // restart keeps the decoders for the next run. A fault of the watch's port leaves
    // the decoders to fail on their own (those in the capture fail with the same fault),
    // for the node's recovery of the source to relaunch and the next run to re-adopt;
    // stopping them would mark them `Stopped`, which recovery never relaunches. Any other
    // end (`job_stop`, a `replace`, a failure of the loop or of a stage) tears down all
    // the run started.
    let shutdown = inner.shutdown.is_cancelled();
    let port_faulted = r.port.fault().is_some();
    let teardown = !shutdown && !inner.restarting(&r.id) && !port_faulted;
    let (current, pending, captures) = match shutdown {
        true => (None, None, Vec::new()),
        false => (
            w.current.take().map(|(j, _)| j),
            w.pending.take(),
            std::mem::take(&mut w.capturing)
                .into_iter()
                .map(|(j, _)| j)
                .collect(),
        ),
    };
    // A signal's `choose` that keeps nothing never outlives the run (at the node's
    // shutdown it ends with the node, and a transient job never resumes).
    let choosing = std::mem::take(&mut w.choosing)
        .into_iter()
        .filter(|_| !shutdown)
        .map(|t| (t.choose, r.prov.source.clone()));
    let kept = match teardown {
        true => std::mem::take(&mut w.decoding),
        false => Vec::new(),
    };
    let decoders = choosing
        .chain(kept.into_iter().flat_map(Decoding::teardown))
        .collect();
    release(&inner, &r.prov.source, current, pending, decoders, captures).await;
    if teardown {
        w.signals.clear();
        w.cooled.clear();
    }
    inner.watch_save(&r, &w);
    result
}

/// The loop proper; returns on cancellation or a fault.
async fn run(inner: &Arc<Inner>, r: &mut Run, w: &mut State) -> Result<(), StageError> {
    let cancelled = || Err(StageError::Cancelled);
    if let Some(stale) = r.seed.stale.take() {
        match inner.wait_running(r, &stale).await {
            None => return cancelled(),
            Some(true) => {
                let _ = inner.stop_gated(&stale).await;
            }
            Some(false) => {}
        }
    }
    // A span watch's resumed channel in a quiet spell is held as a kept one is (spec
    // 9.5); any other watch confirms within the detector's `gone_ms`.
    let policy = &r.seed.policy;
    let confirm_ms = match policy.modes.is_some() {
        true => policy.gone_ms.max(policy.hold_ms),
        false => policy.gone_ms,
    };
    let confirm_ns = confirm_ms.saturating_mul(1_000_000);
    for mut d in std::mem::take(&mut r.seed.adopt) {
        match inner.wait_running(r, &d.job).await {
            None => return cancelled(),
            Some(false) => continue,
            Some(true) => {}
        }
        // Kept under another policy's sinks (a `graph_put` of the watch changed them):
        // restarted with this run's.
        match inner.sink_into(r, &d.job, &r.seed.policy.sinks).await {
            SinkOutcome::Cancelled => return cancelled(),
            SinkOutcome::Lost(why) => {
                inner.stop_started(r, &d.job, &d.source).await;
                let summary = format!("lost {} at {}: {why}", d.mode, d.signal.mhz());
                let e = Event {
                    signal: Some(&d.signal),
                    job: Some(&d.job),
                    mode: Some(&d.mode),
                    source: Some(&d.source),
                    placed: Some(d.placed),
                    why: Some(WHY_RELAUNCH.into()),
                    ..Event::default()
                };
                inner.watch_event(r, "lost", summary, e);
                continue;
            }
            SinkOutcome::Refused(why) => d.sink_error = Some(why),
            SinkOutcome::Off | SinkOutcome::On => d.sink_error = None,
        }
        let t = now().0;
        d.last_event_ns = t;
        d.confirm_by_ns = Some(t.saturating_add(confirm_ns));
        w.subs.insert(d.job.clone(), inner.decoder_subs(&d.job));
        w.decoding.push(d);
    }
    if r.seed.policy.modes.is_some() {
        match known_channels(inner, r).await {
            None => return cancelled(),
            Some((known, register)) => (w.known, w.register) = (known, register),
        }
        if r.seed.policy.seed_known && seed(inner, r, w).await.is_none() {
            return cancelled();
        }
    }
    inner.watch_save(r, w);
    loop {
        tokio::select! {
            _ = r.cancel.cancelled() => return cancelled(),
            _ = tokio::time::sleep(POLL) => {}
        }
        if test_fail_now() {
            return Err(StageError::Failed("watch: test failure".into()));
        }
        if let Some(f) = r.port.fault() {
            return Err(StageError::Failed(source_fault_reason(r.port.id(), &f)));
        }
        let mut changed = w.prune_cooled(now().0);
        changed |= signals(inner, r, w);
        changed |= verdict(inner, r, w).await;
        if r.cancel.is_cancelled() {
            return cancelled();
        }
        changed |= decoders(inner, r, w).await;
        changed |= settle(inner, r, w).await;
        match choosing_ended(inner, r, w).await {
            None => return cancelled(),
            Some(c) => changed |= c,
        }
        match chosen(inner, r, w).await {
            None => return cancelled(),
            Some(c) => changed |= c,
        }
        if r.cancel.is_cancelled() {
            return cancelled();
        }
        changed |= capture::ended(inner, r, w);
        if w.current.is_none() && w.classifying.is_none() {
            sweep(inner, r, w).await;
            match next(inner, r, w).await {
                Next::Idle => {}
                Next::Started => changed = true,
                Next::Cancelled => return cancelled(),
            }
        }
        if changed {
            inner.watch_save(r, w);
        }
    }
}

/// A span watch's known channels (spec 4.3) and what the register query asked and
/// answered (`{"providers": <providers giving assignments>, "assignments": <centres
/// answered>}`; an unreachable register answers nothing, which this cannot tell from
/// nothing licensed): each listed ready row's `band.centers_hz` inside the band
/// (`from: row`), then each transmitter [`band_assignments`] finds in the band (the
/// station's region, else [`DEFAULT_REGION`]; centres within half the narrowest listed
/// row's width are one) whose decoded emission width fits a listed row by
/// [`WIDTH_FIT`] (`from: register`, the row whose width lies nearest). `None`:
/// cancelled.
async fn known_channels(inner: &Arc<Inner>, r: &Run) -> Option<(Vec<Known>, serde_json::Value)> {
    let modes = inner.modes();
    let band = r.tuning;
    let rows: Vec<(String, u64, Vec<u64>)> = r
        .seed
        .policy
        .modes
        .iter()
        .flatten()
        .filter_map(|id| {
            let m = modes.get(id)?;
            let width = m.row.band.bandwidth_hz;
            (m.startable() && width <= band.bandwidth_hz.0)
                .then(|| (id.clone(), width, m.row.band.centers_hz.clone()))
        })
        .collect();
    let inside = |hz: u64| hz.abs_diff(band.center_hz.0) <= band.bandwidth_hz.0 / 2;
    let mut known: Vec<Known> = rows
        .iter()
        .flat_map(|(id, width, centres)| {
            centres.iter().filter(|c| inside(**c)).map(|c| Known {
                center_hz: *c,
                bandwidth_hz: *width,
                mode: id.clone(),
                from: KnownFrom::Row,
                licensee: None,
                site: None,
                distance_km: None,
                sites: 0,
                allocation: None,
            })
        })
        .collect();
    let providers = inner.knowledge.read().unwrap().clone();
    let asked = providers.iter().filter(|p| p.gives_assignments()).count();
    let Some(narrowest) = rows.iter().map(|(_, w, _)| *w).min() else {
        let register = serde_json::json!({"providers": asked, "assignments": 0});
        return Some((known, register));
    };
    let region = inner
        .station
        .lock()
        .unwrap()
        .as_ref()
        .map_or_else(|| DEFAULT_REGION.to_string(), |s| s.region.clone());
    let lookup = band_assignments(
        &providers,
        &region,
        band.center_hz,
        band.bandwidth_hz,
        Hz(narrowest / 2),
    );
    let found = tokio::select! {
        a = lookup => a,
        _ = r.cancel.cancelled() => return None,
    };
    for b in &found {
        let a = &b.allocation;
        let (Some(center), Some(emission)) = (a.center_hz, a.emission_decoded.as_ref()) else {
            continue;
        };
        let licensed = emission.bandwidth_hz.0;
        let row = rows
            .iter()
            .filter(|(_, w, _)| fits_within(WIDTH_FIT, *w, licensed))
            .min_by(|x, y| {
                let off = |w: u64| (w as f64 / licensed as f64).ln().abs();
                off(x.1).total_cmp(&off(y.1))
            });
        let Some((id, width, _)) = row else {
            continue;
        };
        known.push(Known {
            center_hz: center.0,
            bandwidth_hz: *width,
            mode: id.clone(),
            from: KnownFrom::Register,
            licensee: a.licensee.clone(),
            site: a.site.clone(),
            distance_km: a.distance_km,
            sites: b.sites,
            allocation: Some(a.clone()),
        });
    }
    let register = serde_json::json!({"providers": asked, "assignments": found.len()});
    Some((known, register))
}

/// `seed_known`: start a decoder at once on each known channel up to the cap, at the
/// known centre with its row ([`Inner::start_direct`] on a tracked signal there that
/// the detector has not seen), unless a decoder (an adopted one, an earlier seed)
/// already covers it, its channel is cooled or another job holds it. Each must be
/// confirmed by a `new` on its channel within `hold_ms` or is `lost` `not seen again`.
/// `None`: cancelled.
async fn seed(inner: &Arc<Inner>, r: &Run, w: &mut State) -> Option<()> {
    let hold_ns = r.seed.policy.hold_ms.saturating_mul(1_000_000);
    for k in w.known.clone() {
        if w.full() {
            break;
        }
        let t = now().0;
        let signal = Signal {
            center_hz: k.center_hz,
            bandwidth_hz: k.bandwidth_hz,
            snr_db: 0.0,
            first_ns: t,
            last_ns: t,
        };
        let decoded = w
            .decoding
            .iter()
            .any(|d| d.signal.overlaps(signal.center_hz, signal.bandwidth_hz));
        if decoded
            || cooled_overlaps(w, signal.center_hz, signal.bandwidth_hz, t)
            || inner.held_elsewhere(r, w, &signal)
        {
            continue;
        }
        w.signals.push(Tracked {
            signal,
            present: false,
            tries: 0,
            next_retry_ns: None,
            handled: false,
            band_hz: k.bandwidth_hz,
            score: 0.0,
            features: None,
            allocations: None,
            learned: None,
            captured: false,
            captures: Vec::new(),
            fits: vec![k.mode.clone()],
            waiting: false,
        });
        let i = w.signals.len() - 1;
        match inner.start_direct(r, w, i, &k.mode).await {
            Ok(None) => return None,
            Ok(Some(job)) => {
                if let Some(d) = w.decoding.iter_mut().find(|d| d.job == job) {
                    d.confirm_by_ns = Some(d.since_ns.saturating_add(hold_ns));
                }
            }
            // Its `unknown` is published; nothing on the air to retry.
            Err(_) => {
                w.signals.remove(i);
            }
        }
    }
    Some(())
}

/// Take `detect`'s announcements since the last tick into the state; whether it changed.
fn signals(inner: &Arc<Inner>, r: &mut Run, w: &mut State) -> bool {
    let mut changed = false;
    loop {
        let v = match r.rx.try_recv() {
            Ok(f) => match f.payload {
                Payload::Event(v) if v["protocol"] == "signal" => v,
                _ => continue,
            },
            Err(TryRecvError::Lagged(_)) => continue,
            Err(_) => return changed,
        };
        let Some(s) = Signal::of(&v) else { continue };
        let half = r.tuning.bandwidth_hz.0 / 2;
        let inside = s.center_hz.abs_diff(r.tuning.center_hz.0) <= half;
        if !inside {
            continue;
        }
        changed = true;
        let new = v["state"] == "new";
        let t = now().0;
        // A decoder the watch keeps is on it.
        if let Some(d) = w
            .decoding
            .iter_mut()
            .find(|d| d.signal.overlaps(s.center_hz, s.bandwidth_hz))
        {
            match new {
                true => {
                    // The decoder stays where it runs (a seeded one on its known centre).
                    d.signal = s.clone();
                    d.gone_since_ns = None;
                    // A seeded decoder's `confirm_ms` counts from its signal's first `new`.
                    if d.confirm_by_ns.take().is_some() && d.unproven_by_ns.is_some() {
                        let confirm_ns = r.seed.policy.confirm_ms.saturating_mul(1_000_000);
                        d.unproven_by_ns = Some(t.saturating_add(confirm_ns));
                    }
                }
                false => d.gone_since_ns = Some(t),
            }
        }
        let tracked = nearest(&w.signals, s.center_hz, s.bandwidth_hz);
        match (new, tracked) {
            (true, Some(i)) => {
                w.signals[i].fits = inner.fit_ids(r, &s);
                w.signals[i].signal = s;
                w.signals[i].present = true;
            }
            (true, None) => {
                let decoded = w
                    .decoding
                    .iter()
                    .any(|d| d.signal.overlaps(s.center_hz, s.bandwidth_hz));
                let choosing = w
                    .choosing
                    .iter()
                    .any(|t| t.signal.overlaps(s.center_hz, s.bandwidth_hz));
                if decoded || choosing || inner.held_elsewhere(r, w, &s) {
                    continue;
                }
                let (band_hz, score) = inner.identify_band(r, &s);
                let fits = inner.fit_ids(r, &s);
                w.signals.push(Tracked {
                    signal: s,
                    present: true,
                    tries: 0,
                    next_retry_ns: None,
                    handled: false,
                    band_hz,
                    score,
                    features: None,
                    allocations: None,
                    learned: None,
                    captured: false,
                    captures: Vec::new(),
                    fits,
                    waiting: false,
                });
                capture::seen_before(w);
            }
            (false, Some(i)) => {
                let on = w
                    .current
                    .as_ref()
                    .is_some_and(|(_, c)| w.signals[i].signal.overlaps(*c, 1));
                let decoded = w.decoding.iter().any(|d| {
                    d.signal.overlaps(
                        w.signals[i].signal.center_hz,
                        w.signals[i].signal.bandwidth_hz,
                    )
                });
                let capturing = w.capturing.iter().any(|(_, c)| {
                    c.overlaps(
                        w.signals[i].signal.center_hz,
                        w.signals[i].signal.bandwidth_hz,
                    )
                });
                // A signal being tried is let go by [`choosing_ended`], which stops its `choose`.
                let choosing = w.choosing.iter().any(|t| {
                    t.signal.overlaps(
                        w.signals[i].signal.center_hz,
                        w.signals[i].signal.bandwidth_hz,
                    )
                });
                match on || decoded || capturing || choosing {
                    true => w.signals[i].present = false,
                    false => {
                        w.signals.remove(i);
                    }
                }
            }
            (false, None) => {}
        }
    }
}

/// When the identify run in progress has ended, act on its verdict: place a proof's
/// decoder and publish `decoding`; publish `unknown` and schedule a retry otherwise.
/// Whether the state changed.
async fn verdict(inner: &Arc<Inner>, r: &Run, w: &mut State) -> bool {
    let Some((id, center)) = w.current.clone() else {
        return false;
    };
    if inner.runs(&id) {
        return false;
    }
    let verdict = inner.verdicts.lock().unwrap().remove(&id);
    w.current = None;
    let Some(i) = nearest(&w.signals, center, 1) else {
        // Its signal is gone from the list: a proof's decoder is not kept.
        if let Some(job) = verdict
            .as_ref()
            .and_then(|v| v["verdict"]["job"].as_str())
            .map(|s| JobId(s.into()))
        {
            inner.stop_started(r, &job, &r.prov.source).await;
        }
        return true;
    };
    let v = verdict.unwrap_or(serde_json::Value::Null);
    let proved = v["verdict"]["mode"]
        .as_str()
        .zip(v["verdict"]["job"].as_str())
        .map(|(m, j)| (m.to_string(), JobId(j.into())));
    if let Some((mode, job)) = proved
        && inner.runs(&job)
    {
        let signal = w.signals[i].signal.clone();
        // Neither `current` nor kept while it is placed: the guard stops it on a panic.
        w.pending = Some(job.clone());
        let Placement {
            job,
            source,
            placed,
            why,
            sink,
        } = inner.place_decoder(r, &mode, &signal, &job).await;
        inner.disown(&job);
        w.signals[i].handled = true;
        let sink_error = match sink {
            // Stopping: the loop's exit path stops what is pending.
            SinkOutcome::Cancelled => return true,
            SinkOutcome::Lost(why) => {
                // Not listed as decoding: it is gone (left alone until its signal returns).
                w.pending = None;
                inner.stop_started(r, &job, &source).await;
                let summary = format!("lost {mode} at {}: {why}", signal.mhz());
                let e = Event {
                    signal: Some(&signal),
                    job: Some(&job),
                    mode: Some(&mode),
                    source: Some(&source),
                    placed: Some(placed),
                    why: Some(WHY_RELAUNCH.into()),
                    ..Event::default()
                };
                inner.watch_event(r, "lost", summary, e);
                return true;
            }
            SinkOutcome::Refused(why) => Some(why),
            SinkOutcome::Off | SinkOutcome::On => None,
        };
        let t = now().0;
        // A signal that went while identify ran sends no further `gone`: count its
        // silence from now, so the decoder is still dropped after the hold.
        let gone_since_ns = (!w.signals[i].present).then_some(t);
        w.subs.insert(job.clone(), inner.decoder_subs(&job));
        w.decoding.push(Decoding {
            mode: mode.clone(),
            job: job.clone(),
            source: source.clone(),
            signal: signal.clone(),
            since_ns: t,
            placed,
            via: Via::Identify,
            sink_error: sink_error.clone(),
            attached: Vec::new(),
            held: false,
            choose: None,
            last_event_ns: t,
            squelch_open: None,
            gone_since_ns,
            confirm_by_ns: None,
            unproven_by_ns: None,
        });
        w.pending = None;
        let mut summary = format!(
            "decoding {mode} at {} on {} ({})",
            signal.mhz(),
            source.0,
            placed.as_str()
        );
        if let Some(why) = why {
            summary.push_str(&format!(": {why}"));
        }
        if let Some(why) = sink_error {
            summary.push_str(&format!("; sink refused: {why}"));
        }
        let e = Event {
            signal: Some(&signal),
            job: Some(&job),
            mode: Some(&mode),
            source: Some(&source),
            placed: Some(placed),
            via: Some(Via::Identify),
            ..Event::default()
        };
        inner.watch_event(r, "decoding", summary, e);
        return true;
    }
    let tracked = &mut w.signals[i];
    count_try(tracked, now().0);
    let label = v["verdict"]["label"].as_str().unwrap_or("unknown");
    let features = v["verdict"]["features"].clone();
    let allocations = v["verdict"]["allocations"].clone();
    let learned = Some(v["verdict"]["learned"].clone()).filter(|l| l.is_object());
    let mut summary = match &learned {
        // A signal an accepted signature knows: named, and not captured. Its retries go
        // on, so a real decoder may still prove it.
        Some(l) => format!(
            "learned {} at {}:{}",
            l["name"].as_str().unwrap_or_default(),
            tracked.signal.mhz(),
            measured(&features),
        ),
        None => format!(
            "unknown at {}: {label}{}{}",
            tracked.signal.mhz(),
            measured(&features),
            register_words(&v["verdict"]),
        ),
    };
    summary.push_str(&format!(
        " (identify {} of {})",
        tracked.tries,
        RETRY_MAX + 1
    ));
    if v.is_null() {
        summary.push_str("; identify ended without a verdict");
    }
    tracked.features = Some(features.clone());
    tracked.allocations = Some(allocations.clone());
    tracked.learned = learned;
    let origins = v["verdict"]["origins"].clone();
    let signal = tracked.signal.clone();
    let e = Event {
        signal: Some(&signal),
        job: Some(&id),
        features: Some(features),
        allocations: Some(allocations),
        origins: Some(origins),
        ..Event::default()
    };
    inner.watch_event(r, "unknown", summary, e);
    capture::on_unknown(inner, r, w, i).await;
    true
}

/// When the choosing watch's step 1 has answered, act on it for its signal
/// ([`Inner::start_answer`]); a signal no longer tracked is let go. Whether the state
/// changed (`Some(false)`: nothing ended yet); `None`: cancelled.
async fn chosen(inner: &Arc<Inner>, r: &Run, w: &mut State) -> Option<bool> {
    if !w.classifying.as_ref().is_some_and(|c| c.task.is_finished()) {
        return Some(false);
    }
    let c = w.classifying.take()?;
    let answer = c
        .task
        .await
        .unwrap_or_else(|e| Err(format!("classify task: {e}")));
    let Some(i) = nearest(&w.signals, c.center_hz, 1) else {
        return Some(true);
    };
    inner.start_answer(r, w, (i, c.band), answer).await?;
    Some(true)
}

/// Take the verdict of each kept job's `choose` child that has ended (the child leaves it
/// in `Inner::verdicts`, as an owned identify does): a proof of riders of the job's row
/// (a `wfm_demod` with `rds_decode` attached) attaches them and lifts its hold, the job
/// kept ([`ride_on_kept`]); only a proof of another row replaces the job ([`replace`]);
/// anything else lifts its hold and the first answer stands ([`lift`]): the job's own
/// row, an unproved guess at another, `cannot_tell`, or no verdict at all (a node
/// restart: a transient job never resumes). A held job whose
/// child is past step 2 with no proof ([`Inner::choose_past_two`]) is lifted at once
/// while the child goes on to step 3, whose proof still replaces it. Whether the state
/// changed.
async fn settle(inner: &Arc<Inner>, r: &Run, w: &mut State) -> bool {
    if choices_held() {
        return false;
    }
    let mut changed = false;
    for k in 0..w.decoding.len() {
        let d = &w.decoding[k];
        let past = d
            .held
            .then(|| d.choose.as_ref().and_then(|c| inner.choose_past_two(c)));
        if let Some(Some(ran)) = past {
            let why = match ran {
                None => "its choose's step 2 proved nothing (step 3 goes on)".to_string(),
                Some(reason) => {
                    format!("its choose's step 2 did not run ({reason}); step 3 goes on")
                }
            };
            lift_with(inner, r, w, k, why);
            changed = true;
        }
    }
    let ended: Vec<JobId> = w
        .decoding
        .iter()
        .filter_map(|d| d.choose.clone())
        .filter(|c| !inner.runs(c))
        .collect();
    for c in ended {
        let verdict = inner.verdicts.lock().unwrap().remove(&c);
        let Some(k) = w
            .decoding
            .iter()
            .position(|d| d.choose.as_ref() == Some(&c))
        else {
            continue;
        };
        w.decoding[k].choose = None;
        changed = true;
        #[allow(unused_mut)]
        let mut choice: Option<Choice> = verdict.and_then(|v| serde_json::from_value(v).ok());
        #[cfg(feature = "test-fixtures")]
        test_rename_verdict(&mut choice);
        match choice.as_ref().and_then(|c| c.job.as_ref().map(|j| (c, j))) {
            Some((c, job))
                if c.settled == Settled::Proved && rides_on(inner, job, &w.decoding[k].mode) =>
            {
                if ride_on_kept(inner, r, w, k, (c, job)).await.is_none() {
                    return true;
                }
            }
            Some((c, job)) if c.settled == Settled::Proved && job.kind != w.decoding[k].mode => {
                if replace(inner, r, w, k, (c, job)).await.is_none() {
                    return true;
                }
            }
            _ => lift(inner, r, w, k, choice.as_ref()),
        }
    }
    changed
}

/// The `choose` of the kept job `k` proved no other row (`choice`, `None` when it ended
/// without a verdict): the job stands and its hold is lifted, so it joins the mix and
/// its transcriber publishes what it kept. Event `decoding`, only when a hold was lifted.
fn lift(inner: &Inner, r: &Run, w: &mut State, k: usize, choice: Option<&Choice>) {
    let d = &w.decoding[k];
    if !d.held {
        return;
    }
    let why = match choice {
        None => "its choose ended without a verdict".to_string(),
        Some(c) => match (&c.job, &c.reason) {
            (None, reason) => format!(
                "its choose could not tell ({})",
                reason.as_deref().unwrap_or("no reason given")
            ),
            (Some(j), _) if j.kind == d.mode => {
                format!(
                    "the choice stands ({} at step {})",
                    c.settled.name(),
                    c.step
                )
            }
            (Some(j), _) => format!(
                "its choose proved nothing ({} at step {} is {})",
                j.kind,
                c.step,
                c.settled.name()
            ),
        },
    };
    lift_with(inner, r, w, k, why);
}

/// Lift the hold of the kept job `k` for `why`, so it joins the mix and its transcriber
/// publishes what it kept. Event `decoding`.
fn lift_with(inner: &Inner, r: &Run, w: &mut State, k: usize, why: String) {
    let d = &mut w.decoding[k];
    let _ = inner.set_held(&d.job, None);
    d.held = false;
    let summary = format!(
        "decoding {} at {} on {}: {why}; the first answer stands and the hold is lifted",
        d.mode,
        d.signal.mhz(),
        d.source.0
    );
    kept_event(inner, r, d, summary);
}

/// Whether every row of the proved `job`'s `attach` is a rider of the kept job's row
/// `mode` ([`Inner::rides_with`]): its proof confirms the kept job (spec 49.3), so it
/// is kept and the riders attached to it rather than replaced. `false` for a job with
/// nothing to attach.
fn rides_on(inner: &Inner, job: &ChoiceJob, mode: &str) -> bool {
    !job.attach.is_empty()
        && (job.attach.iter())
            .all(|row| JobKind::from_name(row).is_some_and(|k| inner.rides_with(&k, mode)))
}

/// The `choose` of the kept job `k` proved riders of its row (`choice`, its `job`'s
/// `attach`, [`rides_on`]): the job is not replaced and keeps its id. Each rider is
/// attached to it ([`Inner::ride`]): one of that row already riding is left as it is
/// (`<row> already rides on it (job <id>)` when it is not the watch's own), and one that
/// does not attach is noted (`<row> did not attach: <why>`) while the job runs on. Its
/// hold is lifted whatever came of the attach: the proof confirms the demodulator. Event
/// `decoding`, `its choose proved <rows> riding on it (<settled> at step <n>)` then the
/// notes. `None`: cancelled.
async fn ride_on_kept(
    inner: &Arc<Inner>,
    r: &Run,
    w: &mut State,
    k: usize,
    (choice, job): (&Choice, &ChoiceJob),
) -> Option<()> {
    let (id, own) = (w.decoding[k].job.clone(), w.decoding[k].attached.clone());
    let mut why = format!(
        "its choose proved {} riding on it ({} at step {})",
        job.attach.join(", "),
        choice.settled.name(),
        choice.step
    );
    for row in &job.attach {
        let note = match inner.ride(r, &id, row, &own).await {
            Ride::Cancelled => return None,
            Ride::Attached(a) => {
                w.decoding[k].attached.push(a);
                format!("{row} attached")
            }
            Ride::Riding(Ok(_)) => format!("{row} already rides on it"),
            Ride::Riding(Err(by)) => format!("{row} already rides on it (job {})", by.0),
            Ride::Refused(why) => why,
        };
        why.push_str(&format!("; {note}"));
    }
    if r.cancel.is_cancelled() {
        return None;
    }
    if w.decoding[k].held {
        lift_with(inner, r, w, k, why);
    } else {
        let d = &w.decoding[k];
        let summary = format!(
            "decoding {} at {} on {}: {why}",
            d.mode,
            d.signal.mhz(),
            d.source.0
        );
        kept_event(inner, r, d, summary);
    }
    Some(())
}

/// Event `decoding` for the kept job `d`, started by the choice, with `summary`.
fn kept_event(inner: &Inner, r: &Run, d: &Decoding, summary: String) {
    let e = Event {
        signal: Some(&d.signal),
        job: Some(&d.job),
        mode: Some(&d.mode),
        source: Some(&d.source),
        placed: Some(d.placed),
        via: Some(Via::Choose),
        ..Event::default()
    };
    inner.watch_event(r, "decoding", summary, e);
}

/// The `choose` of the kept job `k` settled on another row (`choice`, its `job`): stop
/// the job's attached jobs first, then the job (still held: what the transcriber kept is
/// dropped, and the mix never had it), then start the verdict's job unheld in its place,
/// with what applies of the policy's `attach` and the verdict's proved riders attached
/// ([`Inner::attach_to`]). A job that does not start drops the entry and counts an
/// unknown try of its signal. Event `decoding` (or `unknown`). `None`: cancelled.
async fn replace(
    inner: &Arc<Inner>,
    r: &Run,
    w: &mut State,
    k: usize,
    (choice, job): (&Choice, &ChoiceJob),
) -> Option<()> {
    let old = w.decoding[k].clone();
    for a in &old.attached {
        inner.stop_started(r, a, &old.source).await;
    }
    inner.stop_started(r, &old.job, &old.source).await;
    w.subs.remove(&old.job);
    w.decoding[k].attached.clear();
    let floor_hz = old.signal.bandwidth_hz;
    let (id, sink) = match inner.start_chosen(r, job, None, floor_hz).await {
        Ok(Some(started)) => started,
        Ok(None) => {
            w.decoding.remove(k);
            return None;
        }
        Err(e) => {
            w.decoding.remove(k);
            if let Some(i) = nearest(&w.signals, old.signal.center_hz, old.signal.bandwidth_hz) {
                let why = format!("{} did not start in place of {}: {e}", job.kind, old.mode);
                inner.choice_unknown(r, w, i, why).await;
            }
            return Some(());
        }
    };
    let t = now().0;
    let sink_error = sink_error_of(sink);
    w.subs.insert(id.clone(), inner.decoder_subs(&id));
    let d = &mut w.decoding[k];
    d.mode = job.kind.clone();
    d.job = id.clone();
    d.held = false;
    d.since_ns = t;
    d.last_event_ns = t;
    d.squelch_open = None;
    d.unproven_by_ns = None;
    d.sink_error = sink_error.clone();
    let (attached, attach_notes) = inner.attach_to(r, &id, &job.kind, &job.attach).await;
    w.decoding[k].attached = attached;
    if r.cancel.is_cancelled() {
        return None;
    }
    let d = &w.decoding[k];
    let mut summary = format!(
        "decoding {} at {} on {} (capture, choice: {} at step {}) in place of {}",
        d.mode,
        d.signal.mhz(),
        d.source.0,
        choice.settled.name(),
        choice.step,
        old.mode
    );
    for n in kept_notes(attach_notes, sink_error) {
        summary.push_str(&format!("; {n}"));
    }
    let e = Event {
        signal: Some(&d.signal),
        job: Some(&d.job),
        mode: Some(&d.mode),
        source: Some(&d.source),
        placed: Some(d.placed),
        via: Some(Via::Choose),
        ..Event::default()
    };
    inner.watch_event(r, "decoding", summary, e);
    Some(())
}

/// The `decoding` summary of `kind` started by the choice `choice` for `signal`.
fn chosen_summary(kind: &str, signal: &Signal, source: &SourceId, choice: &Choice) -> String {
    format!(
        "decoding {kind} at {} on {} (capture, choice: {} at step {})",
        signal.mhz(),
        source.0,
        choice.settled.name(),
        choice.step,
    )
}

/// The notes of a kept job that runs without some of its attached jobs (`attach_notes`,
/// [`Inner::attach_to`]'s) or without its sink.
fn kept_notes(attach_notes: Vec<String>, sink_error: Option<String>) -> Vec<String> {
    let sink = sink_error.map(|why| format!("sink refused: {why}"));
    attach_notes.into_iter().chain(sink).collect()
}

/// Take the verdict of each `choose` the watch runs on a signal it keeps nothing for
/// ([`Inner::start_choosing`]), once it has ended: a proof starts the proved job inside the
/// capture, kept `via: choose` with the policy's `attach` ([`Inner::start_proved`]);
/// anything else (`cannot_tell`, an unproved guess, no verdict at all) is the unknown try
/// step 1 would have made (event `unknown`, counted and retried after [`RETRY_MS`]). A
/// row listed for `unknown` is never started on a guess. A signal that has gone stops its
/// `choose` and is let go. Whether the state changed; `None`: cancelled.
async fn choosing_ended(inner: &Arc<Inner>, r: &Run, w: &mut State) -> Option<bool> {
    let mut changed = false;
    let present = |w: &State, t: &ChoosingSignal| {
        nearest(&w.signals, t.signal.center_hz, t.signal.bandwidth_hz)
            .is_some_and(|i| w.signals[i].present)
    };
    let gone: Vec<JobId> = (w.choosing.iter())
        .filter(|t| !present(w, t))
        .map(|t| t.choose.clone())
        .collect();
    for c in gone {
        let Some(k) = w.choosing.iter().position(|t| t.choose == c) else {
            continue;
        };
        let t = w.choosing.remove(k);
        inner.stop_started(r, &t.choose, &r.prov.source).await;
        inner.verdicts.lock().unwrap().remove(&t.choose);
        if let Some(i) = nearest(&w.signals, t.signal.center_hz, t.signal.bandwidth_hz)
            && !w.signals[i].present
        {
            w.signals.remove(i);
        }
        changed = true;
    }
    if choices_held() {
        return Some(changed);
    }
    let ended: Vec<JobId> = (w.choosing.iter())
        .map(|t| t.choose.clone())
        .filter(|c| !inner.runs(c))
        .collect();
    for c in ended {
        let Some(k) = w.choosing.iter().position(|t| t.choose == c) else {
            continue;
        };
        let t = w.choosing.remove(k);
        changed = true;
        let verdict = inner.verdicts.lock().unwrap().remove(&c);
        #[allow(unused_mut)]
        let mut choice: Option<Choice> = verdict.and_then(|v| serde_json::from_value(v).ok());
        #[cfg(feature = "test-fixtures")]
        test_rename_verdict(&mut choice);
        let Some(i) = nearest(&w.signals, t.signal.center_hz, t.signal.bandwidth_hz) else {
            continue;
        };
        match choice.as_ref().and_then(|c| c.job.as_ref().map(|j| (c, j))) {
            Some((c, job)) if c.settled == Settled::Proved => {
                if r.cancel.is_cancelled() {
                    return None;
                }
                inner.start_proved(r, w, i, (c, job)).await?;
            }
            _ => {
                let what = match &choice {
                    None => "its choose ended without a verdict".to_string(),
                    Some(c) => format!(
                        "its choose proved none of {} (steps run: {})",
                        t.rows.join(", "),
                        (c.steps_run.iter().map(u8::to_string))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                let why = format!("can't tell: {}; {what}", t.reason);
                inner.choice_unknown(r, w, i, why).await;
            }
        }
    }
    Some(changed)
}

/// `<n>` kHz from `hz`: whole kHz bare, else to one decimal.
fn khz(hz: u64) -> String {
    match hz % 1_000 {
        0 => format!("{}", hz / 1_000),
        _ => format!("{:.1}", hz as f64 / 1e3),
    }
}

/// ` <bw> kHz[, <baud> Bd][, duty <d>]` from a verdict's layer-1 `features`, or nothing
/// without a record.
pub(super) fn measured(features: &serde_json::Value) -> String {
    let Some(bw) = features["bandwidth_hz"].as_u64() else {
        return String::new();
    };
    let mut out = format!(" {} kHz", khz(bw));
    if let Some(baud) = features["symbol_rate_baud"].as_u64() {
        out.push_str(&format!(", {baud} Bd"));
    }
    if let Some(duty) = features["duty"].as_f64() {
        out.push_str(&format!(", duty {duty:.2}"));
    }
    out
}

/// A designator's decoded modulation as one word: `FM`, `AM`, `PM`, else its letter.
fn modulation_word(e: &serde_json::Value) -> String {
    let name = e["modulation_name"].as_str().unwrap_or_default();
    let words = [("Frequency", "FM"), ("Amplitude", "AM"), ("Phase", "PM")];
    match words.iter().find(|(start, _)| name.starts_with(start)) {
        Some((_, w)) => (*w).to_string(),
        None => e["modulation"].as_str().unwrap_or("?").to_string(),
    }
}

/// A designator's decoded information type as one word: `data`, `telephony`,
/// `telegraphy`, else its letter.
fn info_word(e: &serde_json::Value) -> String {
    let info = e["info"].as_str().unwrap_or("?");
    match info {
        "D" => "data",
        "E" => "telephony",
        "A" | "B" => "telegraphy",
        other => other,
    }
    .to_string()
}

/// The register's words for an identify verdict `v`, as Identify's job summary and the
/// watch both word them: `; <the register verdict's summary>` with a register answer
/// (`register agrees: ...`, `register unreachable: ...`, ...); `; register unreachable:
/// <reason>` for an unreachable answer carrying no verdict; else [`register_says`] from
/// the verdict's `allocations` (a verdict kept before the register answer).
pub(super) fn register_words(v: &serde_json::Value) -> String {
    let register = &v["register"];
    if let Some(s) = register["verdict"]["summary"].as_str() {
        return format!("; {s}");
    }
    if register["status"]["state"] == "unreachable" {
        let reason = register["status"]["reason"]
            .as_str()
            .unwrap_or("no reason given");
        return format!("; register unreachable: {reason}");
    }
    register_says(&v["allocations"])
}

/// `; register says <EMISSION> (<bw> kHz <mod> <info>) <licensee>, <site>, <d> km` for
/// the nearest `tx` assignment of `allocations` (an identify verdict's, nearest
/// first): without a decoded designator `; register says <licensee>, <site>, <d> km`;
/// nothing when there is no transmitter.
pub(super) fn register_says(allocations: &serde_json::Value) -> String {
    let nearest = allocations
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["role"] == "tx");
    let Some(a) = nearest else {
        return String::new();
    };
    let mut where_: Vec<String> = ["licensee", "site"]
        .iter()
        .filter_map(|k| a[*k].as_str().map(str::to_string))
        .collect();
    if let Some(d) = a["distance_km"].as_f64() {
        where_.push(format!("{d:.0} km"));
    }
    let mut out = String::from("; register says");
    let e = &a["emission_decoded"];
    if let (Some(code), Some(bw)) = (a["emission"].as_str(), e["bandwidth_hz"].as_u64()) {
        out.push_str(&format!(
            " {code} ({} kHz {} {})",
            khz(bw),
            modulation_word(e),
            info_word(e)
        ));
    }
    if !where_.is_empty() {
        out.push(' ');
        out.push_str(&where_.join(", "));
    }
    out
}

/// Note the decoders' events; drop those whose job no longer runs (the operator stopped
/// it; a span watch cools its channel [`WHY_STOPPED`], or [`WHY_FAILED`] counting a try
/// when it ended `Failed`); stop (`lost`) those whose signal
/// has gone and that stayed silent for the policy's `hold_ms`, re-adopted ones whose
/// signal did not reappear in time, and direct starts that published nothing within
/// `confirm_ms` while their signal was present (`unproven`). A demodulator row's
/// liveness is its squelch: it is silent from the moment its last `squelch` event shut it
/// (an open squelch keeps it alive; its squelch opening is what proves it under
/// `confirm_ms`, so one whose squelch never opens while its signal is present can still
/// be `unproven`), and a row that publishes nothing (`protocol: none`) is alive while its
/// signal is present and never `unproven`. Stopping one stops
/// its `choose` child and its attached job first ([`Decoding::teardown`]). A span watch
/// cools each lost one's channel for the policy's `cooldown_ms` ([`free_channel`]); any
/// other watch forgets its signal. Each frees a slot for [`next`]. Whether the state
/// changed.
async fn decoders(inner: &Arc<Inner>, r: &Run, w: &mut State) -> bool {
    let t = now().0;
    let modes = inner.modes();
    for d in &mut w.decoding {
        // Only the row's own events are its signal: a frame row's `timing` and forwarded
        // `bits` flow on noise too. A mode no longer loaded counts every event.
        let row = modes.get(&d.mode);
        let protocol = row.map(|m| m.row.protocol.clone());
        let demod = row.is_some_and(|m| m.role() == Role::Demodulator);
        let counts = |v: &serde_json::Value| {
            protocol
                .as_deref()
                .is_none_or(|p| v["protocol"].as_str() == Some(p))
        };
        for rx in w.subs.get_mut(&d.job).into_iter().flatten() {
            loop {
                match rx.try_recv() {
                    Ok(f) => {
                        if let Payload::Event(v) = &f.payload
                            && counts(v)
                        {
                            if demod && v["protocol"] == "squelch" {
                                d.squelch_open = Some(v["open"] == true);
                            } else {
                                d.last_event_ns = t;
                                d.unproven_by_ns = None;
                            }
                        }
                    }
                    Err(TryRecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        }
        let quiet_row = protocol.as_deref() == Some("none");
        if demod && (d.squelch_open == Some(true) || quiet_row && d.gone_since_ns.is_none()) {
            d.last_event_ns = t;
        }
        if demod && (d.squelch_open == Some(true) || quiet_row) {
            d.unproven_by_ns = None;
        }
    }
    let silence_ns = r.seed.policy.hold_ms.saturating_mul(1_000_000);
    let until_ns = t.saturating_add(r.seed.policy.cooldown_ms.saturating_mul(1_000_000));
    let span = r.seed.policy.modes.is_some();
    let mut changed = false;
    let mut kept = Vec::with_capacity(w.decoding.len());
    for d in std::mem::take(&mut w.decoding) {
        // A decoder that died of the watch's port fault stays listed: the loop is about
        // to fail on the same fault, and the next run re-adopts it once recovered.
        if !inner.runs(&d.job) && r.port.fault().is_some() {
            kept.push(d);
            continue;
        }
        if !inner.runs(&d.job) {
            // Stopped by the operator (or ended on its own): its slot is free. A span
            // watch's channel rests for the cooldown before the watch starts it again;
            // any other watch leaves its signal alone until it goes and comes back. A
            // decoder that failed counts a try, so one that keeps failing stops being
            // restarted after [`RETRY_MAX`]; an operator stop counts none. Its `choose`
            // child goes with it (its attached job ends with its input).
            w.subs.remove(&d.job);
            if let Some(c) = &d.choose {
                inner.stop_started(r, c, &r.prov.source).await;
                inner.verdicts.lock().unwrap().remove(c);
            }
            if span {
                match inner.failed(&d.job) {
                    true => free_channel(inner, w, &d, until_ns, WHY_FAILED, Some(t)),
                    false => free_channel(inner, w, &d, until_ns, WHY_STOPPED, None),
                }
            }
            changed = true;
            continue;
        }
        let unconfirmed = d.confirm_by_ns.is_some_and(|by| t >= by);
        let unproven = d.gone_since_ns.is_none() && d.unproven_by_ns.is_some_and(|by| t >= by);
        let silent = d
            .gone_since_ns
            .is_some_and(|_| t.saturating_sub(d.last_event_ns.max(d.since_ns)) >= silence_ns);
        let why = match (unconfirmed, unproven, silent) {
            (true, _, _) => WHY_NOT_SEEN,
            (false, true, _) => WHY_UNPROVEN,
            (false, false, true) => WHY_SILENT,
            (false, false, false) => {
                kept.push(d);
                continue;
            }
        };
        for (job, on) in d.clone().teardown() {
            inner.stop_started(r, &job, &on).await;
            inner.verdicts.lock().unwrap().remove(&job);
        }
        w.subs.remove(&d.job);
        match span {
            true => free_channel(inner, w, &d, until_ns, why, Some(t)),
            false => w
                .signals
                .retain(|x| !x.signal.overlaps(d.signal.center_hz, d.signal.bandwidth_hz)),
        }
        let summary = format!("lost {} at {}: {why}", d.mode, d.signal.mhz());
        let e = Event {
            signal: Some(&d.signal),
            job: Some(&d.job),
            mode: Some(&d.mode),
            source: Some(&d.source),
            placed: Some(d.placed),
            why: Some(why.to_string()),
            ..Event::default()
        };
        inner.watch_event(r, "lost", summary, e);
        changed = true;
    }
    w.decoding = kept;
    changed
}

/// A span watch's decoder `d` no longer runs: cool its channel (its centre, its row's
/// width) until `until_ns` for `why`, forget the tracked signals on it that have gone,
/// and put those still present back in the queue, started afresh once the cooldown
/// passes. After a loss the watch raised or a failure (`lost_at`, its time), each
/// requeued signal
/// counts a try ([`count_try`]): past [`RETRY_MAX`] it is left alone until it goes and
/// comes back, so a carrier no decoder proves does not churn.
fn free_channel(
    inner: &Inner,
    w: &mut State,
    d: &Decoding,
    until_ns: u64,
    why: &str,
    lost_at: Option<u64>,
) {
    w.cooled.push(Cooled {
        center_hz: d.signal.center_hz,
        bandwidth_hz: inner.row_width(&d.mode, &d.signal),
        until_ns,
        why: why.to_string(),
    });
    let (center_hz, bandwidth_hz) = (d.signal.center_hz, d.signal.bandwidth_hz);
    w.signals
        .retain(|x| x.present || !x.signal.overlaps(center_hz, bandwidth_hz));
    for x in &mut w.signals {
        if x.signal.overlaps(center_hz, bandwidth_hz) {
            x.handled = false;
            x.next_retry_ns = None;
            x.waiting = false;
            if let Some(t) = lost_at {
                count_try(x, t);
            }
        }
    }
}

/// With no identify in progress, stop any running job this watch owns that it does not
/// keep (a stale identify run or its child resumed after a restart; a kept job's `choose`
/// child is kept with it, as is the `choose` trying a signal with nothing kept).
async fn sweep(inner: &Arc<Inner>, r: &Run, w: &State) {
    let strays: Vec<JobId> = inner
        .jobs
        .lock()
        .unwrap()
        .values()
        .filter(|e| e.record.owner.as_ref() == Some(&r.id))
        .filter(|e| e.record.state == JobState::Running && e.cancel.is_some())
        .filter(|e| !w.decoding.iter().any(|d| d.job == e.record.id))
        .filter(|e| {
            !w.decoding
                .iter()
                .any(|d| d.choose.as_ref() == Some(&e.record.id))
        })
        .filter(|e| !w.capturing.iter().any(|(j, _)| *j == e.record.id))
        .filter(|e| !w.choosing.iter().any(|t| t.choose == e.record.id))
        .map(|e| e.record.id.clone())
        .collect();
    for id in strays {
        let _ = inner.stop_gated(&id).await;
        // A stale capture child's end is nobody's to report.
        inner.captured.lock().unwrap().remove(&id);
    }
}

enum Next {
    Idle,
    Started,
    Cancelled,
}

/// Act on the queued signal that is due and ranks highest ([`rank`]), unless another
/// job's band now overlaps it (it is then dropped). A signal on a cooled channel is not
/// due until its cooldown passes. With `modes: null`, start an identify run on it (event
/// `identifying`), or under `on_signal: choose` the node's choice for it
/// ([`Inner::start_choice`]). With `modes`, by the listed rows its width fits ([`SPAN_WIDTH_FIT`]):
/// none, it is `unknown` at once (a try, retried and captured as an identify's unknown);
/// else the fitting row whose width lies nearest the signal's starts directly
/// ([`Inner::start_direct`]), and rows tied for nearest (one channel width) run an
/// identify restricted to them. A signal that would take a decoder while all
/// `max_decoders` slots are kept waits instead ([`wait`]) and takes a slot when one
/// frees and it ranks highest; nothing is pre-empted.
async fn next(inner: &Arc<Inner>, r: &Run, w: &mut State) -> Next {
    let t = now().0;
    let mut marked = false;
    loop {
        let full = w.full();
        let due = w
            .signals
            .iter()
            .enumerate()
            .filter(|(_, x)| x.present && !x.handled && !(full && x.waiting))
            .filter(|(_, x)| x.next_retry_ns.is_none_or(|n| t >= n))
            .filter(|(_, x)| !cooled_overlaps(w, x.signal.center_hz, x.signal.bandwidth_hz, t))
            .max_by(|a, b| rank(a.1, b.1, &w.known).then(b.0.cmp(&a.0)))
            .map(|(i, _)| i);
        let Some(i) = due else {
            return match marked {
                true => Next::Started,
                false => Next::Idle,
            };
        };
        if inner.held_elsewhere(r, w, &w.signals[i].signal) {
            w.signals.remove(i);
            continue;
        }
        let mut candidates = None;
        if r.seed.policy.modes.is_some() {
            let signal = &w.signals[i].signal;
            let fits = inner.fitting_rows(r, signal);
            let nearest = nearest_fits(&fits, signal.bandwidth_hz);
            w.signals[i].fits = fits.into_iter().map(|(id, _)| id).collect();
            if nearest.is_empty() {
                no_fit(inner, r, w, i, t).await;
                return Next::Started;
            }
            if full {
                wait(inner, r, w, i);
                marked = true;
                continue;
            }
            if let [mode] = nearest.as_slice() {
                return match inner.start_direct(r, w, i, mode).await {
                    Ok(None) => Next::Cancelled,
                    Ok(Some(_)) | Err(_) => Next::Started,
                };
            }
            candidates = Some(nearest);
        } else if full {
            wait(inner, r, w, i);
            marked = true;
            continue;
        } else if r.seed.policy.choose {
            inner.start_choice(r, w, i);
            return Next::Started;
        }
        w.signals[i].waiting = false;
        let tracked = w.signals[i].clone();
        match inner
            .start_identify(r, w, &tracked, candidates.as_deref())
            .await
        {
            Ok(None) => return Next::Cancelled,
            Ok(Some(id)) => {
                let summary = format!("identifying {}", tracked.signal.mhz());
                let e = Event {
                    signal: Some(&tracked.signal),
                    job: Some(&id),
                    ..Event::default()
                };
                inner.watch_event(r, "identifying", summary, e);
                return Next::Started;
            }
            Err(e) => {
                // Not startable now (a conflict, a fault): counted as an unknown try.
                w.current = None;
                let tracked = &mut w.signals[i];
                count_try(tracked, t);
                let signal = tracked.signal.clone();
                // No identify ran, so no register answer: `allocations` stays null.
                let summary = format!("unknown at {}: identify did not start: {e}", signal.mhz());
                let ev = Event {
                    signal: Some(&signal),
                    ..Event::default()
                };
                inner.watch_event(r, "unknown", summary, ev);
                return Next::Started;
            }
        }
    }
}

/// Every slot is kept: the tracked signal `i` waits for one (event `waiting` the first
/// time, summary `waiting at <MHz>: <n> of <cap> decoding`, then `, <t> choosing` while
/// `t` signals are being tried, each holding a slot).
fn wait(inner: &Inner, r: &Run, w: &mut State, i: usize) {
    let tracked = &mut w.signals[i];
    if tracked.waiting {
        return;
    }
    tracked.waiting = true;
    let signal = tracked.signal.clone();
    let mut summary = format!(
        "waiting at {}: {} of {} decoding",
        signal.mhz(),
        w.decoding.len(),
        w.max_decoders
    );
    // A signal being tried holds the slot its proof would take.
    if !w.choosing.is_empty() {
        summary.push_str(&format!(", {} choosing", w.choosing.len()));
    }
    let e = Event {
        signal: Some(&signal),
        ..Event::default()
    };
    inner.watch_event(r, "waiting", summary, e);
}

/// Count a try of `tracked` that ended without a decoder (an unknown verdict, a start
/// that failed, no listed row fitting): past [`RETRY_MAX`] it is left alone (`handled`,
/// no retry) until it goes and comes back, else tried again [`RETRY_MS`] after `now_ns`.
fn count_try(tracked: &mut Tracked, now_ns: u64) {
    tracked.tries += 1;
    match tracked.tries > RETRY_MAX {
        true => {
            tracked.handled = true;
            tracked.next_retry_ns = None;
        }
        false => tracked.next_retry_ns = Some(now_ns.saturating_add(retry_ms() * 1_000_000)),
    }
}

/// The tracked signal `i` fits none of the policy's `modes`: `unknown` at once (no
/// identify run, so no register answer or origins), counted as a try and retried after
/// [`RETRY_MS`] as an identify's unknown, and captured under `on_unknown: capture`.
async fn no_fit(inner: &Arc<Inner>, r: &Run, w: &mut State, i: usize, t: u64) {
    let tracked = &mut w.signals[i];
    count_try(tracked, t);
    let signal = tracked.signal.clone();
    let summary = format!(
        "unknown at {}: no listed mode fits {} kHz",
        signal.mhz(),
        khz(signal.bandwidth_hz)
    );
    let e = Event {
        signal: Some(&signal),
        ..Event::default()
    };
    inner.watch_event(r, "unknown", summary, e);
    capture::on_unknown(inner, r, w, i).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(center_hz: u64, bandwidth_hz: u64) -> Tracked {
        Tracked {
            signal: Signal {
                center_hz,
                bandwidth_hz,
                snr_db: 20.0,
                first_ns: 0,
                last_ns: 0,
            },
            present: true,
            tries: 0,
            next_retry_ns: None,
            handled: false,
            band_hz: 12_500,
            score: 0.0,
            features: None,
            allocations: None,
            learned: None,
            captured: false,
            captures: Vec::new(),
            fits: Vec::new(),
            waiting: false,
        }
    }

    /// A choice band keeps inside the ring: narrowed to twice the distance to the nearer
    /// edge, never under the floor, untouched in the middle.
    #[test]
    fn a_choice_band_keeps_inside_the_ring() {
        // A 2.4 MHz ring about 100 MHz: 98.8 to 101.2 MHz.
        let ring = (100_000_000, 2_400_000);
        assert_eq!(ring_clamped(400_000, 180_000, 100_000_000, ring), 400_000);
        // A WFM signal 120 kHz under the top edge: 240 kHz of room.
        assert_eq!(ring_clamped(400_000, 180_000, 101_080_000, ring), 240_000);
        // Nearer still: the measured width stands.
        assert_eq!(ring_clamped(400_000, 180_000, 101_150_000, ring), 180_000);
        // The low edge too, and a centre outside the ring keeps only the floor.
        assert_eq!(ring_clamped(400_000, 180_000, 98_900_000, ring), 200_000);
        assert_eq!(ring_clamped(400_000, 180_000, 102_000_000, ring), 180_000);
    }

    /// The width an identify run takes comes only from rows in range for the signal's
    /// centre: the 8 MHz `dvbt_decode` (174-862 MHz) sets it at 500 MHz, not at 1.3 GHz.
    /// A wide row marked `identify.when_named` sets no identify band of a watch with
    /// default modes; unmarked it would.
    #[test]
    fn a_row_marked_when_named_sets_no_identify_band() {
        let dir = tempfile::tempdir().unwrap();
        let modes_dir = dir.path().join("modes");
        std::fs::create_dir_all(&modes_dir).unwrap();
        let (_, text) = ModeRegistry::embedded()
            .into_iter()
            .find(|(f, _)| f.ends_with("dvbt_decode.json"))
            .unwrap();
        let mut seen = Vec::new();
        for marked in [false, true] {
            let mut row: serde_json::Value = serde_json::from_str(text).unwrap();
            row["id"] = "wide_decode".into();
            row["band"] = serde_json::json!({
                "bandwidth_hz": 9_000_000, "centers_hz": [], "min_hz": null, "range_hz": null
            });
            row["identify"]["when_named"] = marked.into();
            std::fs::write(modes_dir.join("wide.json"), row.to_string()).unwrap();
            let mut modes = ModeRegistry::load_with(
                &crate::stage::StageRegistry::new(),
                Some(&modes_dir),
                None,
            );
            modes.set_program("wide_decode", Some("/stand-in".into()), None);
            let signal = Signal {
                center_hz: 1_300_000_000,
                bandwidth_hz: 9_000_000,
                ..tracked(0, 0).signal
            };
            seen.push(identify_band(&modes, 20_000_000, &signal, None));
        }
        assert_eq!(seen[0].0, 9_000_000, "{seen:?}");
        assert!(seen[0].1 > 0.0, "{seen:?}");
        assert_eq!(seen[1], (9_000_000, 0.0), "{seen:?}");
    }

    #[test]
    fn identify_band_ignores_an_out_of_range_wide_row() {
        let mut modes = ModeRegistry::load(&crate::stage::StageRegistry::new(), None);
        modes.set_program("dvbt_decode", Some("/stand-in".into()), None);
        let signal = |center_hz| Signal {
            center_hz,
            bandwidth_hz: 7_000_000,
            ..tracked(0, 0).signal
        };
        let (band_hz, score) = identify_band(&modes, 20_000_000, &signal(500_000_000), None);
        assert_eq!(band_hz, 8_000_000);
        assert!(score > 0.0, "{score}");
        assert_eq!(
            identify_band(&modes, 20_000_000, &signal(1_300_000_000), None),
            (7_000_000, 0.0)
        );
    }

    #[test]
    fn a_watch_naming_a_row_marked_when_named_identifies_on_its_band() {
        // dvbs_ip_decode is tried only when named (`identify.when_named`) and has no
        // ranges: with its handler ready, a watch whose `modes` name it identifies a 7 MHz
        // signal on the row's 10 MHz; one with the default modes, or naming another row,
        // still passes it over and takes the signal's own width.
        let mut modes = ModeRegistry::load(&crate::stage::StageRegistry::new(), None);
        modes.set_handler_program("dvbs_ip_decode", "onair", Some("/stand-in".into()), None);
        assert!(modes.get("dvbs_ip_decode").unwrap().startable());
        let signal = Signal {
            center_hz: 1_300_000_000,
            bandwidth_hz: 7_000_000,
            ..tracked(0, 0).signal
        };
        let named = ["dvbs_ip_decode".to_string()];
        let (band_hz, score) = identify_band(&modes, 20_000_000, &signal, Some(&named));
        assert_eq!(band_hz, 10_000_000);
        assert!(score > 0.0, "{score}");
        assert_eq!(
            identify_band(&modes, 20_000_000, &signal, None),
            (7_000_000, 0.0)
        );
        let other = ["dvbt_decode".to_string()];
        assert_eq!(
            identify_band(&modes, 20_000_000, &signal, Some(&other)),
            (7_000_000, 0.0)
        );
    }

    #[test]
    fn a_span_fit_is_tolerant_and_prefers_the_nearest_width() {
        // A strong 12.5 kHz page reads 22 kHz: it fits now, not by identify's rule.
        assert!(fits_within(SPAN_WIDTH_FIT, 12_500, 22_000));
        assert!(!width_fits(12_500, 22_000));
        // Bounds inclusive; a 60 kHz signal fits no 12.5 or 25 kHz row.
        assert!(fits_within(SPAN_WIDTH_FIT, 12_500, 25_000));
        assert!(fits_within(SPAN_WIDTH_FIT, 12_500, 6_250));
        assert!(!fits_within(SPAN_WIDTH_FIT, 12_500, 60_000));
        assert!(!fits_within(SPAN_WIDTH_FIT, 25_000, 60_000));
        let row = |id: &str, w: u64| (id.to_string(), w);
        // 20 kHz: 25 kHz (ratio 0.8) is nearer than 12.5 kHz (ratio 1.6).
        let fits = [row("acars", 12_500), row("vdl2", 25_000)];
        assert_eq!(nearest_fits(&fits, 20_000), ["vdl2"]);
        assert_eq!(nearest_fits(&fits, 14_000), ["acars"]);
        // One channel width: a tie, for the restricted identify.
        let fits = [row("pocsag_decode", 12_500), row("aprs_decode", 12_500)];
        assert_eq!(
            nearest_fits(&fits, 22_000),
            ["pocsag_decode", "aprs_decode"]
        );
        assert!(nearest_fits(&[], 22_000).is_empty());
    }

    #[test]
    fn a_reading_at_the_detectors_floor_fits_a_row_a_little_more_than_twice_it() {
        // pagers_au: width_hz [6000, 25000]; a clean pager reads at the floor, which
        // the plain fit (half of 12.5 kHz is 6.25 kHz) misses.
        assert!(!fits_within(SPAN_WIDTH_FIT, 12_500, 6_000));
        assert!(span_fits(12_500, 6_000, 6_000));
        assert!(
            span_fits(12_500, 6_200, 6_000),
            "within a hair of the floor"
        );
        let row = |id: &str, w: u64| (id.to_string(), w);
        let listed = [row("pocsag_decode", 12_500)];
        let fits: Vec<_> = listed
            .iter()
            .filter(|(_, w)| span_fits(*w, 6_000, 6_000))
            .cloned()
            .collect();
        assert_eq!(nearest_fits(&fits, 6_000), ["pocsag_decode"]);
        // A 50 kHz row stays out of reach, and off the floor the plain fit holds.
        assert!(!span_fits(50_000, 6_000, 6_000));
        assert!(!span_fits(12_500, 6_000, 2_000));
        assert!(!span_fits(12_500, 5_000, 4_000));
        assert!(span_fits(12_500, 22_000, 6_000));
    }

    #[test]
    fn a_slot_goes_to_a_known_channel_then_score_then_snr() {
        let (mut a, mut b) = (tracked(148_600_000, 12_500), tracked(148_700_000, 12_500));
        a.score = 2.0;
        b.signal.snr_db = 40.0;
        assert_eq!(rank(&a, &b, &[]), Ordering::Greater, "score before SNR");
        b.score = 2.0;
        assert_eq!(rank(&a, &b, &[]), Ordering::Less, "then SNR");
        let known = [Known {
            center_hz: 148_600_000,
            bandwidth_hz: 12_500,
            mode: "pocsag_decode".into(),
            from: KnownFrom::Row,
            licensee: None,
            site: None,
            distance_km: None,
            sites: 0,
            allocation: None,
        }];
        assert_eq!(rank(&a, &b, &known), Ordering::Greater, "known first");
    }

    #[test]
    fn a_cooled_channel_blocks_only_overlapping_signals_until_it_passes() {
        let mut w = State {
            signals: Vec::new(),
            decoding: Vec::new(),
            choosing: Vec::new(),
            max_decoders: 1,
            cooled: vec![Cooled {
                center_hz: 148_600_000,
                bandwidth_hz: 12_500,
                until_ns: 100,
                why: WHY_UNPROVEN.into(),
            }],
            known: Vec::new(),
            register: serde_json::Value::Null,
            current: None,
            classifying: None,
            subs: HashMap::new(),
            pending: None,
            capturing: Vec::new(),
            captured: Vec::new(),
            guard: None,
        };
        assert!(cooled_overlaps(&w, 148_605_000, 13_000, 99));
        assert!(
            !cooled_overlaps(&w, 148_625_000, 13_000, 99),
            "a channel away"
        );
        assert!(!cooled_overlaps(&w, 148_605_000, 13_000, 100), "passed");
        assert!(!w.prune_cooled(99));
        assert!(w.prune_cooled(100));
        assert!(w.cooled.is_empty());
    }

    #[test]
    fn a_verdict_goes_to_the_nearest_overlapping_track() {
        // Seen live 2026-11-05: a 22.5 kHz track at 404.0254 MHz also covers the 7 kHz
        // track at 404.0285 MHz, and took every one of its verdicts ("identify 9 of 4").
        let signals = vec![tracked(404_025_377, 22_500), tracked(404_028_540, 7_031)];
        assert_eq!(nearest(&signals, 404_028_540, 1), Some(1));
        assert_eq!(nearest(&signals, 404_025_400, 1), Some(0));
        assert_eq!(nearest(&signals, 404_100_000, 1), None);
    }

    /// A kept job's teardown stops its `choose` child, then every job attached to it in
    /// order, then the job.
    #[test]
    fn a_teardown_stops_each_attached_job_before_the_job() {
        let v = serde_json::json!({
            "mode": "wfm_demod", "job": "demod", "source": "rx",
            "signal": {"center_hz": 1, "bandwidth_hz": 1, "snr_db": 1.0, "first_ns": 0,
                "last_ns": 0},
            "since_ns": 0, "placed": "capture", "via": "choose",
            "attached": ["scribe", "rider"], "choose": "child"
        });
        let d: Decoding = serde_json::from_value(v).unwrap();
        let order: Vec<String> = d.teardown().into_iter().map(|(j, _)| j.0).collect();
        assert_eq!(order, ["child", "scribe", "rider", "demod"]);
    }

    /// A decoder persisted before plan 49 with one `attached` job reads as a list of it;
    /// one with none is empty and persists without the key.
    #[test]
    fn a_persisted_attached_job_reads_as_a_list() {
        let mut v = serde_json::json!({
            "mode": "fm_demod", "job": "demod", "source": "rx",
            "signal": {"center_hz": 1, "bandwidth_hz": 1, "snr_db": 1.0, "first_ns": 0,
                "last_ns": 0},
            "since_ns": 0, "placed": "capture", "via": "choose", "attached": "scribe"
        });
        let d: Decoding = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(d.attached, [JobId("scribe".into())]);
        assert_eq!(
            serde_json::to_value(&d).unwrap()["attached"],
            serde_json::json!(["scribe"])
        );
        v.as_object_mut().unwrap().remove("attached");
        let d: Decoding = serde_json::from_value(v).unwrap();
        assert!(d.attached.is_empty());
        assert!(serde_json::to_value(&d).unwrap().get("attached").is_none());
    }

    /// `attach` keeps its entries once each in order; the start refuses an entry that is
    /// neither `transcribe` nor a row that rides, naming it.
    #[test]
    fn attach_takes_transcribe_and_riders_once_each() {
        let modes = ModeRegistry::load(&crate::stage::StageRegistry::new(), None);
        let p = serde_json::json!({"attach": ["transcribe", "rds_decode", "transcribe"]});
        check_attach(&modes, &p).unwrap();
        let g = Graph {
            id: crate::graph::GraphId("watch".into()),
            nodes: vec![GraphNode {
                id: crate::graph::NodeId("detect".into()),
                kind: DETECT_STAGE.into(),
                params: p,
            }],
            edges: vec![],
        };
        let kinds: Vec<String> = Policy::of(&g)
            .attach
            .iter()
            .map(|k| k.name().into())
            .collect();
        assert_eq!(kinds, ["transcribe", "rds_decode"]);
        for (bad, i) in [
            ("packets", 1),
            ("fm_demod", 1),
            ("dmr_decode", 1),
            ("no_such_row", 1),
        ] {
            let p = serde_json::json!({"attach": ["transcribe", bad]});
            let e = check_attach(&modes, &p).unwrap_err();
            assert_eq!(
                e,
                format!(
                    "attach[{i}]: {bad} is not attached by a watch (only transcribe and rows that ride with a demodulator)"
                )
            );
        }
    }
}
