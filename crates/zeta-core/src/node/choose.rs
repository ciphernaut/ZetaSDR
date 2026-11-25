//! The `choose` job (design 5.3): a controller that climbs the proof ladder for its band
//! as far as its context's `choose.step` allows (the choice policy's step for the surface
//! that asked, [`crate::choice_policy::ChoicePolicy::steps`]), taking no lease.
//!
//! Its selection is the job's band over the newest `budget.slice_s` seconds of the span it
//! was asked (the page's box), else of the source's ring (what the ring holds, when less; a
//! span the ring no longer holds is a `cannot_tell`). Step 1 classifies it at once, as
//! `classify` does, and chooses ([`choose_now`]); a watch's own `choose` skips it, taking
//! the watch's classification of the signal and the samples it read ([`StepOne`]), so the
//! band is classified once. Step 1 sets aside a decoder row whose own band reaches past the
//! capture. Step 2 writes the same slice into the burst store as a trial capture (pinned
//! while the job runs): its band is the trial band, the smallest holding every
//! `could_prove` row's own band ([`trial_band`]), and with the node's band cutter the slice
//! is cut to it at once ([`keep_for_trials`]), so a wide capture's slice is small. It
//! replays the slice through each `could_prove` row at the row's own band on the measured
//! centre ([`replay_trials`]), at most `budget.concurrent` trials at once across every
//! `choose` of the node ([`Inner::trial_slot`]), each judged by its row's identify rule
//! within `budget.trial_s` in all; the first proof settles the job. A slice under 1 s, or
//! one the trial quota cannot hold even shortened to 1 s beside the slices other chooses
//! have pinned, is not tried, and `not_tried` says why. Step 3 tries the rows still in
//! `could_prove` (under `widen`, then those step 1 set aside) on the live band, one at a
//! time in score order within what is left of `budget.trial_s` ([`Inner::step_three`]),
//! each holding one of the node's trial slots. Step 3's trials hold every slot but one
//! between them (with one slot, that one), and a waiting replay is served before a waiting
//! step-3 trial ([`TrialClass`]), so a new signal's replay does not queue behind another's
//! dwell. Each step-3 trial runs as an ordinary job the `choose` owns, started only where a
//! running capture already holds the row's band (lease setting `share`), or after growing a
//! held capture to hold it, every holder keeping its band (`widen`). A trial's lease gives
//! way to any foreground request that changes the capture.
//!
//! The job's event stream carries `{"protocol": "choice", "step", "choice"}` when step
//! 1 answers and whenever the answer changes, then `{"protocol": "verdict", "choice"}`;
//! `context.choose` keeps the ask, `choice` (the latest), `slice`, `trials` and
//! `verdict`. With `start` the verdict's job is started on the job's source (`started`,
//! or `start_error`); a step-3 proof's job is kept running as that job instead. The job
//! ends `stopped`, a `cannot_tell` included: a source with no ring answers
//! `cannot_tell`, its reason that the source is not capturing.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use num_complex::Complex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::OwnedMutexGuard;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::identify::{Attempt, NotStarted, ProofFrom, TrialLease, Trier};
use super::launch::{AfterGate, Ending, StageRunning, supervise};
use super::persist::now;
use super::{Inner, Node, NodeError, REPLAY_SOURCE, source_fault_reason};
use crate::choice::{
    Alternative, Choice, ChoiceJob, NOT_HELD_LEAD, NotTried, Settled, capture_holds,
    catalogue_entry, choose_now, inside_capture, job_band, job_for, trial_band, trial_bands,
};
use crate::choice_policy::{Budget, LeaseSetting, Surface};
use crate::classify::{Classification, Measurement};
use crate::cut::BandCutter;
use crate::graph::{Graph, GraphId};
use crate::job::JobRecord;
use crate::lease::Uncovered;
use crate::ops::{JobKind, Op, OpError, OpResult, identify_dwell_ms};
use crate::port::{Port, Tuning};
use crate::ring::RingError;
use crate::stage::StageError;
use crate::stream::{Frame, Payload};
use crate::types::{Hz, JobId, Provenance, Selection, SourceId, Span, StreamId, StreamKind};

/// Step 2's note when the ring holds under a second of the band once its oldest
/// [`RING_MARGIN_NS`] are set aside.
pub const RING_SHORT: &str =
    "not tried: the ring holds under 1 s once its oldest 100 ms are set aside";
/// Step 2's note when the policy's `slice_s` is under the second step 2 needs.
pub const POLICY_SLICE_SHORT: &str = "not tried: the policy's slice_s is under 1 s";
/// Step 3's note of a row no running capture holds the band of (and, under `widen`, no
/// held capture could grow to).
pub const NOT_COVERED: &str = "not tried: no running capture covers it";
/// Step 3's note of a row whose trial gave way to a foreground request.
pub const GAVE_WAY: &str = "not tried: a foreground request took the capture";
/// Step 2's note when the trial quota cannot hold a second of the slice.
pub const OVER_QUOTA: &str = "not tried: the slice exceeds the trial quota";
/// The note of a row the trial budget left no time for.
pub const OVER_BUDGET: &str = "not tried: budget";
/// The trial steps' note when layer 1 measured nothing to size a proved job by.
pub const UNMEASURED: &str = "not tried: layer 1 did not measure the band";
/// Step 2's note when the span asked is itself under the second step 2 needs.
pub const SPAN_SHORT: &str = "not tried: the selection's span is under 1 s";
/// The `cannot_tell` reason of a span asked that the ring no longer holds.
pub const SPAN_GONE: &str = "the ring no longer holds the span";
/// Step 2's note when the cut of its slice failed and took the samples with it.
pub const UNCUT: &str = "not tried: the slice could not be cut";
/// The shortest slice step 2 replays.
const MIN_SLICE_NS: u64 = 1_000_000_000;
/// How far after a ring's oldest sample a selection of all it holds starts: the next
/// block written may evict that much before the selection is read (a ring of 1 s
/// written in 100 ms blocks).
const RING_MARGIN_NS: u64 = 100_000_000;
/// Bytes one sample of a slice takes in the burst store (`cf32`).
const SAMPLE_BYTES: u64 = 8;

/// What the node's `choose` jobs share (`Inner::choosing`).
#[derive(Default)]
pub(super) struct Choosing {
    /// The trials running now across every `choose`, by class, and the replays waiting
    /// for a slot ([`Inner::trial_slot`]).
    slots: std::sync::Mutex<Slots>,
    /// The most trials ever running at once (tests).
    peak: AtomicU32,
    /// The most step-3 trials ever running at once (tests).
    live_peak: AtomicU32,
    /// Woken whenever a trial ends or a waiting replay's wait ends, for the trials waiting
    /// for a slot.
    freed: tokio::sync::Notify,
    /// The step 1 a watch hands its `choose` child ([`StepOne`]), by the child's id, until
    /// the child takes it as it starts.
    seeds: std::sync::Mutex<HashMap<JobId, StepOne>>,
    /// How many bands a `choose` classified itself (tests).
    classified: AtomicU64,
    /// The owned chooses now past step 2 with no proof, on to step 3 ([`PastTwo`]): their
    /// watch lifts its probation then, as only a step-3 proof can still replace the job.
    /// The value is why step 2 did not run (`None`: it ran).
    past_two: std::sync::Mutex<HashMap<JobId, Option<String>>>,
}

/// An owned `choose` listed as past step 2 with no proof while it lives
/// ([`Inner::choose_past_two`]); dropped however the job ends.
struct PastTwo {
    inner: Arc<Inner>,
    id: JobId,
}

impl Drop for PastTwo {
    fn drop(&mut self) {
        self.inner
            .choosing
            .past_two
            .lock()
            .unwrap()
            .remove(&self.id);
    }
}

/// A `choose`'s step 1: the selection, the span's length in ns, its classification,
/// what of the samples it read step 2 keeps ([`Kept`]) and the answer. A watch hands its
/// own to its `choose` child, which then classifies nothing again.
pub(super) struct StepOne {
    pub sel: Selection,
    pub held_ns: u64,
    pub classified: Classification,
    /// `None` when no slice will be written (nothing in `could_prove`, or a proof).
    pub kept: Option<Kept>,
    pub choice: Choice,
}

/// The samples step 2 writes as its slice: the trial band ([`trial_band`], the record's
/// band), cut to it when the node has a cutter (`rate` the lowest that holds it, the file
/// centred `file_center` on it), else the ring's block at its rate and centre.
pub(super) struct Kept {
    pub samples: Vec<Complex<f32>>,
    pub rate: Hz,
    pub file_center: Hz,
    pub band: Tuning,
}

/// How much wider than a union of several rows' bands the slice is cut, so what lies
/// past the union does not fold into a row's band when the cut's rate is the band's own.
const UNION_GUARD: f64 = 1.25;

impl StepOne {
    /// Step 1 for `sel` (`held_ns` long), classified `classified` from `samples` of a
    /// ring at `rate` centred `ring_center`: chosen with the ring as the capture
    /// ([`choose_now`]), and, when the `choose` may `climb` to step 2, the samples kept
    /// for it ([`keep_for_trials`]). Nothing is kept, and nothing cut, where step 2 will
    /// write no slice: a proof, an empty `could_prove`, a selection under 1 s, or a
    /// policy `slice_s` under 1 s.
    pub(super) async fn chosen(
        node: &Node,
        (sel, held_ns, climb): (Selection, u64, bool),
        classified: Classification,
        (samples, rate, ring_center): (Vec<Complex<f32>>, Hz, Hz),
    ) -> StepOne {
        let modes = node.inner.modes();
        let region = node.inner.region();
        let capture = Some((ring_center, rate));
        let choice = choose_now(&modes, modes.weights(), &classified, &sel, &region, capture);
        let slice_s = node.inner.choice_policy.lock().unwrap().budget.slice_s;
        let climb = climb && held_ns >= MIN_SLICE_NS && slice_s >= 1.0;
        let kept = match &classified.measurement {
            Some(m) if climb && choice.settled != Settled::Proved => {
                match trial_band(&modes, &choice, m, &sel, &region) {
                    Some(band) => {
                        // A union of bands that differ: cut with a guard.
                        let bands = trial_bands(&modes, &choice, m, &sel, &region);
                        let union = bands.iter().any(|b| *b != band);
                        let block = (samples, rate, ring_center);
                        keep_for_trials(node.cutter(), (band, union), block).await
                    }
                    None => None,
                }
            }
            _ => None,
        };
        StepOne {
            sel,
            held_ns,
            classified,
            kept,
            choice,
        }
    }
}

/// What step 2 keeps of a ring block (`samples` at `rate`, centred `ring_center`) for
/// rows tried inside the trial `band` ([`trial_band`]; `union` when it is the union of
/// bands that differ): with a `cutter` the block is cut to the band on a blocking thread
/// and dropped (a union cut [`UNION_GUARD`] times as wide, when the capture holds that),
/// else kept whole. A cut that fails keeps the block whole; `None` when the cut panicked
/// and took the block with it.
async fn keep_for_trials(
    cutter: Option<Arc<dyn BandCutter>>,
    (band, union): (Tuning, bool),
    (samples, rate, ring_center): (Vec<Complex<f32>>, Hz, Hz),
) -> Option<Kept> {
    // A union's whole-Hz centre may leave it a hertz past the capture: brought back in.
    let band = inside_capture(band, (ring_center, rate));
    let whole = |samples| Kept {
        samples,
        rate,
        file_center: ring_center,
        band,
    };
    let Some(cutter) = cutter else {
        return Some(whole(samples));
    };
    let guarded = Tuning {
        bandwidth_hz: Hz((band.bandwidth_hz.0 as f64 * UNION_GUARD).ceil() as u64),
        ..band
    };
    let cut_to = match union {
        true if capture_holds((ring_center, rate), guarded) => guarded,
        _ => band,
    };
    let offset = cut_to.center_hz.0 as i64 - ring_center.0 as i64;
    let cut = tokio::task::spawn_blocking(move || {
        match cutter.cut(&samples, rate, offset, cut_to.bandwidth_hz) {
            Some(c) => Ok(c),
            None => Err(samples),
        }
    })
    .await;
    match cut {
        Ok(Ok((samples, rate))) => Some(Kept {
            samples,
            rate,
            file_center: band.center_hz,
            band,
        }),
        Ok(Err(samples)) => Some(whole(samples)),
        // The cut panicked: the block went with it.
        Err(_) => None,
    }
}

/// What a trial slot is for ([`Inner::trial_slot`]). Both classes share the policy's
/// `budget.concurrent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TrialClass {
    /// A step-2 replay of a trial slice: served before any waiting `Live` trial, as it
    /// holds its slot about its slice's length.
    Replay,
    /// A step-3 trial on the live band, holding its slot for its dwell: at most
    /// [`live_limit`] at once, so one slot is kept for replays.
    Live,
}

/// The most step-3 trials that may run at once under `concurrent` slots: every slot but
/// one, and with one slot, that one.
fn live_limit(concurrent: u32) -> u32 {
    concurrent.saturating_sub(1).max(1)
}

/// The node's trial slots in use, by class, and the replays waiting for one.
#[derive(Default)]
struct Slots {
    replay: u32,
    live: u32,
    replays_waiting: u32,
}

impl Slots {
    /// Whether a trial of `class` may take a slot now, under `concurrent` slots: any
    /// free slot for a replay; for a step-3 trial, a free slot under [`live_limit`] while
    /// no replay waits.
    fn admits(&self, class: TrialClass, concurrent: u32) -> bool {
        let limit = concurrent.max(1);
        let free = self.replay + self.live < limit;
        match class {
            TrialClass::Replay => free,
            TrialClass::Live => free && self.live < live_limit(limit) && self.replays_waiting == 0,
        }
    }

    /// The count of running trials of `class`.
    fn running(&mut self, class: TrialClass) -> &mut u32 {
        match class {
            TrialClass::Replay => &mut self.replay,
            TrialClass::Live => &mut self.live,
        }
    }
}

/// One running trial's slot ([`Inner::trial_slot`]), given back on drop.
struct TrialSlot {
    inner: Arc<Inner>,
    class: TrialClass,
}

impl Drop for TrialSlot {
    fn drop(&mut self) {
        let c = &self.inner.choosing;
        *c.slots.lock().unwrap().running(self.class) -= 1;
        c.freed.notify_waiters();
    }
}

/// A replay counted as waiting for a slot ([`Slots::replays_waiting`]) while it lives,
/// however its wait ends.
struct ReplayWaiting<'a> {
    choosing: &'a Choosing,
}

impl Drop for ReplayWaiting<'_> {
    fn drop(&mut self) {
        self.choosing.slots.lock().unwrap().replays_waiting -= 1;
        // A step-3 trial held back only by this replay may go now.
        self.choosing.freed.notify_waiters();
    }
}

#[cfg(feature = "test-fixtures")]
impl Node {
    /// Tests only: the most trials (step 2's replays and step 3's live jobs) that ever
    /// ran at once across the node's chooses.
    pub fn test_trial_peak(&self) -> u32 {
        self.inner.choosing.peak.load(Ordering::SeqCst)
    }

    /// Tests only: the most step-3 trials that ever ran at once across the node's
    /// chooses (a [`Node::test_live_trial_slot`] counted too).
    pub fn test_live_trial_peak(&self) -> u32 {
        self.inner.choosing.live_peak.load(Ordering::SeqCst)
    }

    /// Tests only: how many step-2 replays wait for a trial slot now.
    pub fn test_replays_waiting(&self) -> u32 {
        self.inner.choosing.slots.lock().unwrap().replays_waiting
    }

    /// Tests only: a step-3 trial's slot, as a dwell would hold it, waited for up to
    /// `within`; `None` when none came free. Held until the answer is dropped.
    pub async fn test_live_trial_slot(&self, within: Duration) -> Option<TestTrialSlot> {
        let cancel = CancellationToken::new();
        let deadline = Instant::now() + within;
        let slot = self
            .inner
            .trial_slot(TrialClass::Live, &cancel, deadline)
            .await;
        slot.map(|slot| TestTrialSlot { _slot: slot })
    }

    /// Tests only: how many bands the node's chooses classified themselves (a watch's
    /// child takes the watch's step 1 instead).
    pub fn test_choose_classifications(&self) -> u64 {
        self.inner.choosing.classified.load(Ordering::SeqCst)
    }
}

/// Tests only: a trial slot [`Node::test_live_trial_slot`] holds, given back on drop.
#[cfg(feature = "test-fixtures")]
pub struct TestTrialSlot {
    _slot: TrialSlot,
}

/// What a `choose` was asked, resolved at `job_start`: the first members of its
/// `context.choose`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChooseAsk {
    /// Who asked: `ChooseParams.surface`, else the transport's.
    pub surface: Surface,
    /// The step the job climbs to: the policy's for the surface and the ask.
    pub step: u8,
    /// `not tried: capped at step <n> for <surface>` when the ask was cut.
    pub capped: Option<String>,
    /// Start the verdict's job when the job ends.
    pub start: bool,
    /// A short-lived ask: the job is `JobRecord.transient`.
    pub transient: bool,
    /// The selection's span asked (`ChooseParams.span`): the job's selection is its
    /// newest `budget.slice_s`; `None`, the ring's newest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    /// Why the surface asked was not taken (the agent transport asks as `agent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_note: Option<String>,
}

impl Node {
    /// A `Node` over `inner` for a controller job's own task, so the job applies what a
    /// caller of the node would: it shares the node, and its drop shuts nothing down.
    pub(super) fn handle(inner: &Arc<Inner>) -> Node {
        Node {
            inner: inner.clone(),
            load_errors: Vec::new(),
            job_load_errors: 0,
            owner: false,
        }
    }

    /// Start a `choose` on `source` over `tuning` as `ask` says: a record with no
    /// graph and `ask` in its context, launched with no lease ([`Inner::launch_choose`]).
    pub(crate) async fn choose_start(
        &self,
        source: SourceId,
        tuning: Tuning,
        ask: ChooseAsk,
    ) -> Result<JobId, NodeError> {
        let rec = choose_record(source, tuning, ask, None);
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
    }
}

/// The record of a `choose` on `source` over `tuning` asked `ask`: no graph, `ask` in its
/// context, owned by `owner` (a watch, which then takes its verdict from
/// `Inner::verdicts` as it takes an owned identify's).
pub(super) fn choose_record(
    source: SourceId,
    tuning: Tuning,
    ask: ChooseAsk,
    owner: Option<JobId>,
) -> JobRecord {
    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![],
        edges: vec![],
    };
    JobRecord {
        context: serde_json::json!({ "choose": ask }),
        transient: ask.transient,
        owner,
        ..super::running_record(JobKind::Choose.name(), source, tuning, graph, None, false)
    }
}

/// What a running `choose` job needs.
struct Run {
    id: JobId,
    source: SourceId,
    tuning: Tuning,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
    ask: ChooseAsk,
    /// Whether a controller owns the job: its verdict is then also kept for the owner in
    /// `Inner::verdicts`, since an owned record is forgotten as it stops.
    owned: bool,
    /// `context.choose` as last written: the ask, then what the run adds.
    ctx: serde_json::Map<String, Value>,
}

impl Inner {
    /// Launch a `choose` job: no lease (its trials replay the ring, holding no radio),
    /// then [`choose_loop`] under [`supervise`] with one `Event` output stream. `gate`
    /// and `after` are a radio launch's; the steps here take neither.
    pub(super) async fn launch_choose(
        self: &Arc<Self>,
        rec: JobRecord,
        port: Arc<dyn Port>,
        _gate: Option<&OwnedMutexGuard<()>>,
        _after: &mut AfterGate,
    ) -> Result<(), NodeError> {
        // A record whose ask does not read (none of this build writes one) asks as a
        // client asking for no step.
        let ask = serde_json::from_value::<ChooseAsk>(rec.context["choose"].clone())
            .unwrap_or_else(|_| {
                let surface = Surface::Client;
                let policy = self.choice_policy.lock().unwrap().clone();
                let (step, capped) = policy.steps(surface, None);
                ChooseAsk {
                    surface,
                    step,
                    capped,
                    start: false,
                    transient: false,
                    span: None,
                    surface_note: None,
                }
            });
        let ctx = match serde_json::to_value(&ask) {
            Ok(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        let (id, source, tuning) = (rec.id.clone(), rec.source.clone(), rec.tuning);
        let owned = rec.owner.is_some();
        let token = self.shutdown.child_token();
        let (prov, run, ended) = self.record_running(rec, &token)?;
        let out = self.bus.register(StreamKind::Event, prov.clone());
        let r = Run {
            id: id.clone(),
            source,
            tuning,
            prov,
            out: out.clone(),
            cancel: token.clone(),
            ask,
            owned,
            ctx,
        };
        // A watch's step 1, taken now: the watch drops one never taken once this returns.
        let seed = self.choosing.seeds.lock().unwrap().remove(&id);
        let mut set = JoinSet::new();
        let (running, body) = (
            StageRunning::new(self.stages.clone()),
            choose_loop(self.clone(), r, port, seed),
        );
        set.spawn(async move {
            let _running = running;
            body.await
        });
        // No port: there is no lease to release nor a radio to idle.
        let h = tokio::spawn(supervise(
            self.clone(),
            id.clone(),
            (run, ended),
            None,
            set,
            token,
            Ending::streams(vec![out.clone()]),
        ));
        if let Some(e) = self.jobs.lock().unwrap().get_mut(&id)
            && e.run == run
        {
            e.done = Some(h);
            e.outputs = vec![super::JobOutput::new(StreamKind::Event, out)];
        }
        Ok(())
    }

    /// Publish `payload` on the job's event stream, stamped now.
    fn choose_event(&self, r: &Run, mut payload: Value) {
        let t = now();
        payload["t_ns"] = t.0.into();
        let prov = Provenance {
            span: Span {
                start_ns: t,
                end_ns: t,
            },
            ..r.prov.clone()
        };
        self.bus
            .publish(Frame::new(r.out.clone(), prov, Payload::Event(payload)));
    }

    /// Write `context.choose` as the run holds it. A failed merge (the job was removed
    /// under the run) only loses the context copy: the run goes on.
    fn choose_context(&self, r: &Run) {
        let patch = serde_json::json!({ "choose": Value::Object(r.ctx.clone()) });
        let _ = self.context_merge(&r.id, patch);
    }

    /// The answer is now `c`: `context.choose.choice`, and one `choice` event.
    fn publish_choice(&self, r: &mut Run, c: &Choice) {
        let choice = serde_json::to_value(c).expect("a choice serialises");
        r.ctx.insert("choice".into(), choice.clone());
        self.choose_context(r);
        let payload = serde_json::json!({
            "protocol": "choice",
            "step": c.step,
            "choice": choice,
            "summary": summary(c),
        });
        self.choose_event(r, payload);
    }

    /// The job's final answer `c`: `context.choose.choice` and `verdict`, then one
    /// `verdict` event.
    fn choose_verdict(&self, r: &mut Run, c: &Choice) {
        let choice = serde_json::to_value(c).expect("a choice serialises");
        r.ctx.insert("choice".into(), choice.clone());
        r.ctx.insert("verdict".into(), choice.clone());
        self.choose_context(r);
        if r.owned {
            self.verdicts
                .lock()
                .unwrap()
                .insert(r.id.clone(), choice.clone());
        }
        let payload = serde_json::json!({
            "protocol": "verdict",
            "choice": choice,
            "summary": summary(c),
        });
        self.choose_event(r, payload);
    }

    /// The job's selection: its band over the newest `slice_s` seconds of its asked
    /// span the ring holds, else of the ring ([`Inner::selection_of`]).
    fn job_selection(&self, r: &Run, slice_s: f32) -> Result<(Selection, u64), String> {
        let asked = r.ask.span.as_ref().map(|s| (s.start_ns.0, s.end_ns.0));
        self.selection_of(&r.source, r.tuning, slice_s, asked)
    }

    /// The band `band` of `source` over the newest `slice_s` seconds of its ring, or the
    /// newest contiguous run when that is shorter, with the span's length in ns. `None`
    /// while the ring holds nothing. What a watch's step 1 classifies.
    pub(super) fn newest_selection(
        &self,
        source: &SourceId,
        band: Tuning,
        slice_s: f32,
    ) -> Option<(Selection, u64)> {
        self.selection_of(source, band, slice_s, None).ok()
    }

    /// The band `band` of `source` over the newest `slice_s` seconds of `asked`
    /// (`(start_ns, end_ns)`; `None`, all the ring holds) inside the ring's newest
    /// contiguous run ([`slice_span`]), with the span's length in ns. Why there is none:
    /// the source is not capturing, or the ring no longer holds the span asked
    /// ([`SPAN_GONE`]).
    fn selection_of(
        &self,
        source: &SourceId,
        band: Tuning,
        slice_s: f32,
        asked: Option<(u64, u64)>,
    ) -> Result<(Selection, u64), String> {
        let not_capturing = || format!("source {} is not capturing", source.0);
        let ring = self
            .rings
            .lock()
            .unwrap()
            .get(source)
            .cloned()
            .ok_or_else(not_capturing)?;
        let ring = ring.read().unwrap();
        let (start, latest) = ring
            .latest_run_start()
            .zip(ring.latest())
            .ok_or_else(not_capturing)?;
        let (from, to) = slice_span(start.0, latest.0, slice_s, asked).ok_or_else(|| {
            if asked.is_some() {
                SPAN_GONE.to_string()
            } else {
                not_capturing()
            }
        })?;
        let sel = Selection {
            source: source.clone(),
            span: Span {
                start_ns: crate::types::Timestamp(from),
                end_ns: crate::types::Timestamp(to),
            },
            center_hz: band.center_hz,
            bandwidth_hz: band.bandwidth_hz,
        };
        Ok((sel, to - from))
    }

    /// Write the `kept` samples of the selection `sel` as a trial capture of their band
    /// ([`Kept`]: cut to it, or the ring's block), shortened to the newest the trial quota
    /// holds beside the trial slices other chooses have pinned or reserved, pinned for the
    /// job.
    /// Its bytes are reserved first ([`BurstStore::reserve_trial`], a short lock), then
    /// written on a blocking thread, so no lock is held and no runtime worker waits
    /// across the write. The `not tried: ...` reason when it cannot: the quota holds
    /// under 1 s of it ([`OVER_QUOTA`]), or the write fails.
    ///
    /// [`BurstStore::reserve_trial`]: crate::bursts::BurstStore::reserve_trial
    async fn write_slice(&self, mut sel: Selection, kept: Kept) -> Result<TrialSlice, String> {
        let Kept {
            mut samples,
            rate,
            file_center,
            band,
        } = kept;
        let reservation = loop {
            let fits =
                usize::try_from(self.bursts.trial_room() / SAMPLE_BYTES).unwrap_or(usize::MAX);
            if samples.len() > fits {
                if (fits as u64) < rate.0 {
                    return Err(OVER_QUOTA.into());
                }
                let cut = samples.len() - fits;
                samples.drain(..cut);
                sel.span.start_ns.0 += cut as u64 * 1_000_000_000 / rate.0.max(1);
            }
            if let Some(r) = self
                .bursts
                .reserve_trial(samples.len() as u64 * SAMPLE_BYTES)
            {
                break r;
            }
            // Another slice reserved since the room was read: size it again.
        };
        let store = self.bursts.clone();
        let unwritten =
            |e: &dyn std::fmt::Display| format!("not tried: the slice could not be written: {e}");
        // The pin's guard is made on the blocking thread: a job cancelled meanwhile
        // drops it with the task's output, unpinning the slice.
        tokio::task::spawn_blocking(move || {
            let rec = crate::ops::write_ring_slice(
                &store,
                &sel,
                band,
                (&samples, rate, file_center),
                Some(reservation),
            )
            .map_err(|e| unwritten(&e))?;
            Ok(TrialSlice {
                _pinned: Pinned {
                    store,
                    id: rec.id.clone(),
                },
                id: rec.id,
                sel,
            })
        })
        .await
        .map_err(|e| unwritten(&e))?
    }

    /// Step 2: replay the trial `slice` through `choice.could_prove`
    /// ([`replay_trials`]) until `deadline`. The answer: `choice` revised by the first
    /// proof (step 2, `proved`, the old answer its runner-up), or `choice` with
    /// [`OVER_BUDGET`] for the rows the budget left untried. `Cancelled` when the job is.
    async fn step_two(
        self: &Arc<Self>,
        r: &mut Run,
        choice: &Choice,
        m: &Measurement,
        slice: &TrialSlice,
        (budget, deadline): (Budget, Instant),
    ) -> Result<Choice, StageError> {
        let mut next = choice.clone();
        let sel = &slice.sel;
        let job = (&r.id, &r.cancel);
        // Each row is replayed at its own band, as step 3 and the proved job run it.
        let (modes, region) = (self.modes(), self.region());
        let rows: Vec<(String, Option<Tuning>)> = choice
            .could_prove
            .iter()
            .map(|row| {
                let band = modes
                    .get(row)
                    .map(|r| job_band(&job_for(r, m, sel, &region)));
                (row.clone(), band)
            })
            .collect();
        let replays = replay_trials(self, job, &slice.id, &rows, (budget, deadline)).await;
        self.add_trials(r, &replays.trials);
        if replays.trials.iter().any(|t| t.job.is_some()) {
            next.steps_run = with_step(&next.steps_run, 2);
        }
        if r.cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        let proved = replays
            .proved
            .and_then(|(row, evidence)| modes.get(&row).map(|m| (m, evidence)));
        if let Some((mode, evidence)) = proved {
            let job = job_for(mode, m, sel, &self.region());
            let band = Tuning {
                center_hz: job.center_hz,
                bandwidth_hz: job.bandwidth_hz,
            };
            self.supersede_by(&mode.row.id.0, band, now().0);
            return Ok(proved_at(&next, (2, &mode.row.id.0), job, vec![evidence]));
        }
        next.not_tried
            .extend(replays.unstarted.into_iter().map(|row| NotTried {
                row: Some(row),
                reason: OVER_BUDGET.into(),
            }));
        Ok(next)
    }

    /// Hand the `choose` `id` (a watch's child, about to launch) the watch's step 1.
    pub(super) fn choosing_seed(&self, id: &JobId, seed: StepOne) {
        self.choosing.seeds.lock().unwrap().insert(id.clone(), seed);
    }

    /// Whether the owned `choose` `id` is past step 2 with no proof, trying step 3:
    /// `None` when it is not; `Some(None)` when step 2 ran; `Some(Some(reason))` when it
    /// did not run (the `not_tried` reason, without its `not tried: ` lead).
    pub(super) fn choose_past_two(&self, id: &JobId) -> Option<Option<String>> {
        self.choosing.past_two.lock().unwrap().get(id).cloned()
    }

    /// Drop a step 1 handed to the `choose` `id` that it has not taken (it never started).
    pub(super) fn choosing_unseed(&self, id: &JobId) {
        self.choosing.seeds.lock().unwrap().remove(id);
    }

    /// A slot for one trial of `class`, waited for while the node's chooses already run
    /// the current policy's `budget.concurrent` between them ([`Slots::admits`]): a
    /// step-3 trial also while [`live_limit`] step-3 trials run, or while a replay waits.
    /// `None` when `cancel` fires or `deadline` passes first.
    async fn trial_slot(
        self: &Arc<Self>,
        class: TrialClass,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Option<TrialSlot> {
        let c = &self.choosing;
        // Counted from the first refusal to the end of the wait, however it ends.
        let mut waiting = None;
        loop {
            let freed = c.freed.notified();
            tokio::pin!(freed);
            // Registered before the count is read: a slot given back meanwhile wakes it.
            freed.as_mut().enable();
            {
                let concurrent = self.choice_policy.lock().unwrap().budget.concurrent;
                let mut slots = c.slots.lock().unwrap();
                if slots.admits(class, concurrent) {
                    *slots.running(class) += 1;
                    c.peak
                        .fetch_max(slots.replay + slots.live, Ordering::SeqCst);
                    if class == TrialClass::Live {
                        c.live_peak.fetch_max(slots.live, Ordering::SeqCst);
                    }
                    drop(slots);
                    drop(waiting);
                    return Some(TrialSlot {
                        inner: self.clone(),
                        class,
                    });
                }
                if class == TrialClass::Replay && waiting.is_none() {
                    slots.replays_waiting += 1;
                    waiting = Some(ReplayWaiting { choosing: c });
                }
            }
            tokio::select! {
                _ = &mut freed => {}
                _ = cancel.cancelled() => return None,
                _ = tokio::time::sleep_until(deadline) => return None,
            }
        }
    }

    /// Append `trials` to `context.choose.trials` and write the context.
    fn add_trials(&self, r: &mut Run, trials: &[Trial]) {
        let all = r
            .ctx
            .entry("trials")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(all) = all {
            all.extend(
                trials
                    .iter()
                    .map(|t| serde_json::to_value(t).expect("a trial serialises")),
            );
        }
        self.choose_context(r);
    }

    /// Step 3: try each of `choice.could_prove` (in its order, the score's), and under
    /// [`LeaseSetting::Widen`] then each row step 1 set aside as one the capture does not
    /// hold ([`NOT_HELD_LEAD`]), on the live band, one at a time, until `deadline`: the
    /// row's job for the measurement `m` in `sel` ([`job_for`]), started on `port` as an
    /// ordinary job the `choose` owns ([`Inner::trial_row`]) only where a running capture
    /// already holds its band untuned, and judged by the row's identify rule for its dwell
    /// (cut to what is left of the budget, its start included). Under
    /// [`LeaseSetting::Widen`] a band outside the running capture first grows it
    /// ([`widen_for`]; `widened the capture to <centre>/<width> Hz for a trial` in the
    /// answer's evidence when the capture moved); a row still uncovered is [`NOT_COVERED`],
    /// one the budget leaves no time for [`OVER_BUDGET`], one whose trial a foreground
    /// request took the capture from [`GAVE_WAY`] (its job stopped, the next row tried).
    /// Every trial is in `context.choose.trials` (`step: 3`). The answer: `choice` revised
    /// by the first proof (step 3, `proved`, the old answer its runner-up) with, when
    /// `keep`, the proved job still running in its guard; else `choice` with the notes.
    /// Every other trial job is stopped. `Cancelled` when the job is.
    async fn step_three(
        self: &Arc<Self>,
        r: &mut Run,
        (port, lease): (&Arc<dyn Port>, LeaseSetting),
        choice: &Choice,
        (m, sel): (&Measurement, &Selection),
        (deadline, keep): (Instant, bool),
    ) -> Result<(Choice, Option<TrialGuard>), StageError> {
        let modes = self.modes();
        let region = self.region();
        let id = r.id.clone();
        let mut next = choice.clone();
        let mut widened = Vec::new();
        let budget_left = |c: &Choice, row: &str| {
            !c.not_tried
                .iter()
                .any(|n| n.row.as_deref() == Some(row) && n.reason == OVER_BUDGET)
        };
        let note = |c: &mut Choice, row: &str, reason: &str| {
            c.not_tried.push(NotTried {
                row: Some(row.to_string()),
                reason: reason.into(),
            })
        };
        // Under `widen` a row whose band the capture did not hold at step 1 is tried
        // too, after the listed ones: growing the capture may hold it. Its step-1 note
        // gives way to what step 3 makes of it once it is reached; a row never reached
        // keeps it.
        let set_aside: Vec<String> = match lease {
            LeaseSetting::Widen => choice
                .not_tried
                .iter()
                .filter(|n| n.reason.starts_with(NOT_HELD_LEAD))
                .filter_map(|n| n.row.clone())
                .collect(),
            _ => Vec::new(),
        };
        for row in choice.could_prove.iter().chain(&set_aside) {
            // Step 2 found the budget spent before this row.
            if !budget_left(choice, row) {
                continue;
            }
            if set_aside.contains(row) {
                next.not_tried.retain(|n| {
                    !(n.reason.starts_with(NOT_HELD_LEAD) && n.row.as_deref() == Some(row))
                });
            }
            let trial = |job: Option<&JobId>, outcome, evidence| Trial {
                step: 3,
                row: row.clone(),
                job: job.map(|j| j.0.clone()),
                outcome,
                evidence,
            };
            let Some(mode) = modes.get(row) else {
                self.add_trials(r, &[trial(None, "unavailable", format!("no row {row}"))]);
                continue;
            };
            if Instant::now() >= deadline {
                note(&mut next, row, OVER_BUDGET);
                continue;
            }
            // Held for the trial: the node's trials share `budget.concurrent`.
            let Some(_slot) = self.trial_slot(TrialClass::Live, &r.cancel, deadline).await else {
                if r.cancel.is_cancelled() {
                    return Err(StageError::Cancelled);
                }
                note(&mut next, row, OVER_BUDGET);
                continue;
            };
            let job = job_for(mode, m, sel, &region);
            let band = Tuning {
                center_hz: job.center_hz,
                bandwidth_hz: job.bandwidth_hz,
            };
            // The row's own cancellation: the job's, or its lease giving way.
            let yielded = r.cancel.child_token();
            let trier = Trier {
                id: &id,
                port,
                band: r.tuning,
                owner: Some(id.clone()),
                cancel: &yielded,
                lease: TrialLease::Share,
            };
            let mut guard = TrialGuard {
                inner: self.clone(),
                job: None,
            };
            let dwell_ms = identify_dwell_ms(mode.row.identify.dwell_ms, 1);
            let at = (band, dwell_ms, Some(deadline));
            let mut attempt = self.trial_row(&trier, mode, at, &mut guard.job).await;
            let outside = matches!(&attempt, Err(NotStarted::Uncovered(Uncovered::Outside)));
            if lease == LeaseSetting::Widen && outside && Instant::now() < deadline {
                let before = port.current();
                if let Some(got) = widen_for(self, port.as_ref(), band, &r.cancel).await {
                    if Some(got) != before {
                        widened.push(format!(
                            "widened the capture to {}/{} Hz for a trial",
                            got.center_hz.0, got.bandwidth_hz.0
                        ));
                    }
                    attempt = self.trial_row(&trier, mode, at, &mut guard.job).await;
                }
            }
            if r.cancel.is_cancelled() {
                guard.stop().await;
                return Err(StageError::Cancelled);
            }
            let started = guard.job.clone();
            if started.is_some() {
                next.steps_run = with_step(&next.steps_run, 3);
            }
            // A foreground request changed the capture: the trial's lease went.
            if yielded.is_cancelled() {
                guard.stop().await;
                if started.is_some() {
                    let t = trial(started.as_ref(), "stopped", GAVE_WAY.into());
                    self.add_trials(r, &[t]);
                }
                note(&mut next, row, GAVE_WAY);
                continue;
            }
            match attempt {
                Err(NotStarted::Uncovered(_)) => note(&mut next, row, NOT_COVERED),
                Err(NotStarted::Failed(e)) => {
                    self.add_trials(r, &[trial(None, "unavailable", e)]);
                }
                Ok(Attempt::Cancelled) => {
                    guard.stop().await;
                    return Err(StageError::Cancelled);
                }
                Ok(Attempt::NoFrame(why)) => {
                    guard.stop().await;
                    self.add_trials(r, &[trial(started.as_ref(), "no_frame", why)]);
                }
                Ok(Attempt::Proved { evidence, .. }) => {
                    let t = trial(started.as_ref(), "proved", evidence.clone());
                    self.add_trials(r, &[t]);
                    if !keep {
                        guard.stop().await;
                    }
                    self.supersede_by(row, band, now().0);
                    let lines = widened.into_iter().chain([evidence]).collect();
                    // The notes of the rows before it stand.
                    let proved = proved_at(&next, (3, row), job, lines);
                    return Ok((proved, keep.then_some(guard)));
                }
            }
        }
        next.evidence.extend(widened);
        Ok((next, None))
    }
}

/// Grow `port`'s held capture to hold `band` as well, the smallest band holding both
/// (every holder keeps its band, restarting on the new capture:
/// [`Node::source_widen_held`], which never tunes an unheld port and moves nothing once
/// `cancel` has fired), in its own task (see [`start_task`]: the holders' relaunch
/// launches jobs). The capture granted, or `None` when the band is inside the capture
/// already, the port cannot capture both at once, the job is stopped or the widen is
/// refused.
async fn widen_for(
    inner: &Arc<Inner>,
    port: &dyn Port,
    band: Tuning,
    cancel: &CancellationToken,
) -> Option<Tuning> {
    let cur = port.current().filter(|_| !cancel.is_cancelled())?;
    let edges = |t: Tuning| {
        let half = t.bandwidth_hz.0 / 2;
        (
            t.center_hz.0.saturating_sub(half),
            t.center_hz.0.saturating_add(half),
        )
    };
    let ((bl, bh), (cl, ch)) = (edges(band), edges(cur));
    let (lo, hi) = (bl.min(cl), bh.max(ch));
    if (lo, hi) == (cl, ch) || hi - lo > port.capabilities().max_bandwidth_hz.value().0 {
        return None;
    }
    let want = Tuning {
        center_hz: Hz(lo + (hi - lo) / 2),
        bandwidth_hz: Hz(hi - lo),
    };
    // Awaited whole: a widen under way is never dropped half way through its restarts.
    widen_task(inner, port.id(), want, cancel).await.ok()?.ok()
}

/// [`Node::source_widen_held`] of `source` to `want` for a trial stopped by `cancel`,
/// in its own task (see [`start_task`]).
fn widen_task(
    inner: &Arc<Inner>,
    source: &SourceId,
    want: Tuning,
    cancel: &CancellationToken,
) -> JoinHandle<Result<Tuning, NodeError>> {
    let (node, source, cancel) = (Node::handle(inner), source.clone(), cancel.clone());
    tokio::spawn(async move { node.source_widen_held(&source, want, &cancel).await })
}

/// The `choose` job's body: (1) the watch's step 1 its child was handed ([`StepOne`]),
/// else [`step_one`] (none is a `cannot_tell`), the policy's cap noted; (2) when the
/// job's step reaches 2 and step 1 proved nothing, the same samples written as the trial
/// slice ([`Inner::write_slice`], pinned until just before the verdict) and
/// [`Inner::step_two`] over it, unless the policy's slice is under 1 s
/// ([`POLICY_SLICE_SHORT`]), the span asked is ([`SPAN_SHORT`]), the selection is
/// ([`RING_SHORT`]) or layer 1 measured nothing ([`UNMEASURED`]); (3) when the job's step
/// reaches 3 and nothing proved yet, [`Inner::step_three`] on the live band with what is
/// left of the trials' budget (an owned job is listed past step 2 meanwhile,
/// [`Inner::choose_past_two`]); then the verdict, and with `start` the verdict's job: a
/// step-3 proof's own job, kept running, its lease made an ordinary one and the job
/// disowned (unless its lease gave way first: then it is stopped and the job started
/// afresh), else one started.
async fn choose_loop(
    inner: Arc<Inner>,
    mut r: Run,
    port: Arc<dyn Port>,
    seed: Option<StepOne>,
) -> Result<(), StageError> {
    if let Some(f) = port.fault() {
        return Err(StageError::Failed(source_fault_reason(port.id(), &f)));
    }
    let node = Node::handle(&inner);
    let (budget, lease) = {
        let policy = inner.choice_policy.lock().unwrap();
        (policy.budget, policy.lease)
    };
    let capped = r
        .ask
        .capped
        .clone()
        .map(|reason| NotTried { row: None, reason });
    // A watch's child takes the watch's step 1; any other classifies its selection.
    let seed = match seed {
        Some(s) => s,
        None => match step_one(&inner, &r, &node, budget.slice_s).await? {
            Ok(s) => s,
            Err(reason) => {
                let c = cannot_tell(reason, capped.into_iter().collect());
                inner.publish_choice(&mut r, &c);
                inner.choose_verdict(&mut r, &c);
                return Ok(());
            }
        },
    };
    let StepOne {
        sel,
        held_ns,
        classified,
        mut kept,
        mut choice,
    } = seed;
    choice.not_tried.extend(capped);
    let step_two = r.ask.step >= 2 && choice.settled != Settled::Proved;
    let note = |c: &mut Choice, reason: String| c.not_tried.push(NotTried { row: None, reason });
    // The trial slice lives until the verdict, pinned, for every trial step.
    let mut slice = None;
    let asked_short = r
        .ask
        .span
        .as_ref()
        .is_some_and(|s| s.end_ns.0.saturating_sub(s.start_ns.0) < MIN_SLICE_NS);
    if step_two && budget.slice_s < 1.0 {
        note(&mut choice, POLICY_SLICE_SHORT.into());
    } else if step_two && asked_short {
        note(&mut choice, SPAN_SHORT.into());
    } else if step_two && held_ns < MIN_SLICE_NS {
        note(&mut choice, RING_SHORT.into());
    } else if step_two && classified.measurement.is_none() {
        note(&mut choice, UNMEASURED.into());
    } else if step_two && !choice.could_prove.is_empty() {
        let written = match kept.take() {
            Some(kept) => inner.write_slice(sel.clone(), kept).await,
            None => Err(UNCUT.into()),
        };
        match written {
            Ok(s) => {
                r.ctx.insert("slice".into(), s.id.clone().into());
                slice = Some(s);
            }
            Err(reason) => note(&mut choice, reason),
        }
    }
    // Written or not, the samples go now, not after the step-3 dwells.
    drop(kept);
    inner.publish_choice(&mut r, &choice);
    // Both trial steps share the budget's seconds.
    let deadline = Instant::now() + Duration::from_secs(budget.trial_s.into());
    if let (Some(s), Some(m)) = (&slice, &classified.measurement) {
        let revised = inner
            .step_two(&mut r, &choice, m, s, (budget, deadline))
            .await?;
        // The steps run alone are no new answer: the verdict carries them.
        let changed = Choice {
            steps_run: choice.steps_run.clone(),
            ..revised.clone()
        } != choice;
        choice = revised;
        if changed {
            inner.publish_choice(&mut r, &choice);
        }
    }
    // The proved step-3 job, kept to be the started one.
    let mut kept = None;
    if r.ask.step >= 3
        && choice.settled != Settled::Proved
        && let Some(m) = &classified.measurement
    {
        // Step 2 proved nothing: an owner (a watch) may stop waiting on it now.
        let _past_two = r.owned.then(|| {
            let why = (!choice.steps_run.contains(&2)).then(|| step_two_reason(&choice));
            inner
                .choosing
                .past_two
                .lock()
                .unwrap()
                .insert(r.id.clone(), why);
            PastTwo {
                inner: inner.clone(),
                id: r.id.clone(),
            }
        });
        let on = (&port, lease);
        let at = (deadline, r.ask.start);
        let (revised, k) = inner.step_three(&mut r, on, &choice, (m, &sel), at).await?;
        kept = k;
        // The steps run alone are no new answer: the verdict carries them.
        let changed = Choice {
            steps_run: choice.steps_run.clone(),
            ..revised.clone()
        } != choice;
        choice = revised;
        if changed {
            inner.publish_choice(&mut r, &choice);
        }
    }
    // A proof at step 2 or 3 leaves its slice a training record.
    if let Some(s) = &slice
        && choice.settled == Settled::Proved
        && (2..=3).contains(&choice.step)
        && let Err(e) = inner
            .bursts
            .annotate(&s.id, training_record(&classified, &choice))
    {
        eprintln!("zeta: choose {}: training record: {e}", r.id.0);
    }
    // Unpinned before the verdict's job starts and the verdict is published.
    drop(slice);
    let kept = match kept.as_mut().and_then(|k| k.job.take()) {
        Some(job) => {
            // Under the gate: no request is between its decision and its tune.
            let gate = inner.leases.gate_guard(&r.source).await;
            let settled = inner.leases.settle_trial(&r.source, &job, &gate);
            drop(gate);
            if !settled {
                let _ = inner.stop(&job).await;
            }
            settled.then_some(job)
        }
        None => None,
    };
    if let Some(job) = kept {
        // An ordinary job from now on, kept in the history when it stops.
        inner.disown(&job);
        r.ctx.insert("started".into(), job.0.into());
    } else if r.ask.start
        && let Some(job) = &choice.job
    {
        let started = start_task(&inner, &r.source, job).await;
        let (key, v) = match started {
            Ok(Ok(OpResult::Job(id))) => ("started", id.0.into()),
            Ok(Ok(other)) => (
                "start_error",
                format!("job_start answered {other:?}").into(),
            ),
            Ok(Err(e)) => ("start_error", e.to_string().into()),
            Err(e) => ("start_error", format!("start task: {e}").into()),
        };
        r.ctx.insert(key.into(), v);
    }
    inner.choose_verdict(&mut r, &choice);
    Ok(())
}

/// Step 1 of the `choose` `r`: its selection ([`Inner::job_selection`]) classified and
/// chosen ([`choose_now`]); one the ring moved past before it was read (a block evicted
/// its oldest samples in between) is taken again once, from the ring as it is then. Why
/// there is none: the source is not capturing, the ring no longer holds the span asked,
/// or the classify's error. `Cancelled` when the job is.
async fn step_one(
    inner: &Arc<Inner>,
    r: &Run,
    node: &Node,
    slice_s: f32,
) -> Result<Result<StepOne, String>, StageError> {
    inner.choosing.classified.fetch_add(1, Ordering::SeqCst);
    let mut last = None;
    for _ in 0..2 {
        let (sel, held_ns) = match inner.job_selection(r, slice_s) {
            Ok(s) => s,
            Err(why) => return Ok(Err(why)),
        };
        let c = tokio::select! {
            c = crate::ops::classify_samples(node, &sel) => c,
            _ = r.cancel.cancelled() => return Err(StageError::Cancelled),
        };
        match c {
            Ok((classified, samples, rate, ring_center)) => {
                node.supersede_proved(&classified.tried, r.tuning);
                let block = (samples, rate, ring_center);
                let at = (sel, held_ns, r.ask.step >= 2);
                let one = StepOne::chosen(node, at, classified, block);
                return Ok(Ok(tokio::select! {
                    one = one => one,
                    _ = r.cancel.cancelled() => return Err(StageError::Cancelled),
                }));
            }
            Err(e) => {
                let too_old = matches!(
                    &e,
                    OpError::Ring {
                        detail: RingError::TooOld { .. }
                    }
                );
                last = Some(e.to_string());
                if !too_old {
                    break;
                }
            }
        }
    }
    Ok(Err(last.unwrap_or_default()))
}

/// The training record of a slice a trial settled: each source's guess (layer 1's
/// label, the catalogue's, a learned signature's, layer 3's) and the answer.
fn training_record(c: &Classification, choice: &Choice) -> Value {
    let layer1 = c.measurement.iter().map(|m| ("layer1", &m.label));
    // A label's source is read from its layer and, in layer 2, from the evidence
    // convention of the catalogue and the name prefix of the learned signatures.
    let others = c.labels.iter().filter_map(|l| match l.layer {
        1 => None,
        3 => Some(("layer3", l)),
        2 if catalogue_entry(l).is_some() => Some(("catalogue", l)),
        2 if l.name.starts_with(crate::learned::LEARNED) => Some(("learned", l)),
        _ => Some(("layer2", l)),
    });
    let sources: Vec<Value> = layer1
        .chain(others)
        .map(|(source, l)| {
            serde_json::json!({"source": source, "guess": l.name, "confidence": l.confidence})
        })
        .collect();
    serde_json::json!({
        "sources": sources,
        "settled": {"row": choice.row, "step": choice.step, "settled": choice.settled.name()},
    })
}

/// A choice with no job, for `reason`, at step 1.
fn cannot_tell(reason: String, not_tried: Vec<NotTried>) -> Choice {
    Choice {
        job: None,
        row: None,
        step: 1,
        settled: Settled::CannotTell,
        evidence: vec![],
        runner_up: None,
        could_prove: vec![],
        not_tried,
        reason: Some(reason),
        steps_run: vec![1],
    }
}

/// `prev` settled at `step` by a decode of `row` (`evidence` what the trial noted, its
/// `decode <row>: ...` line last): `job`, `proved` at `step` with layer 1's line and
/// `evidence`, `prev`'s answer its runner-up, `row` out of `could_prove`.
fn proved_at(
    prev: &Choice,
    (step, row): (u8, &str),
    job: ChoiceJob,
    evidence: Vec<String>,
) -> Choice {
    let layer1 = prev
        .evidence
        .iter()
        .filter(|e| e.starts_with("layer 1: "))
        .cloned();
    let runner_up = prev
        .job
        .clone()
        .zip(prev.row.clone())
        .map(|(job, row)| Alternative {
            job,
            row,
            evidence: prev.evidence.clone(),
        });
    Choice {
        job: Some(job),
        row: Some(row.to_string()),
        step,
        settled: Settled::Proved,
        evidence: layer1.chain(evidence).collect(),
        runner_up,
        could_prove: prev
            .could_prove
            .iter()
            .filter(|r| *r != row)
            .cloned()
            .collect(),
        not_tried: prev.not_tried.clone(),
        reason: None,
        steps_run: with_step(&prev.steps_run, step),
    }
}

/// Why step 2 did not run, from the notes in `c`: the first note of no row (the policy,
/// the span, the ring, layer 1, the slice), else the budget, else nothing to replay.
fn step_two_reason(c: &Choice) -> String {
    let lead = |s: &str| s.strip_prefix("not tried: ").unwrap_or(s).to_string();
    if let Some(n) = c.not_tried.iter().find(|n| n.row.is_none()) {
        return lead(&n.reason);
    }
    if c.could_prove.is_empty() {
        return "no decoder row fits to replay".into();
    }
    lead(OVER_BUDGET)
}

/// `steps` with `step` added, in order, once.
fn with_step(steps: &[u8], step: u8) -> Vec<u8> {
    let mut out = steps.to_vec();
    if !out.contains(&step) {
        out.push(step);
        out.sort_unstable();
    }
    out
}

/// A choice's one line: `<kind> at <MHz> MHz, <width> Hz (<settled>, step <n>)`, or
/// `cannot tell: <reason>`.
fn summary(c: &Choice) -> String {
    match &c.job {
        Some(j) => format!(
            "{} at {:.4} MHz, {} Hz ({}, step {})",
            j.kind,
            j.center_hz.0 as f64 / 1e6,
            j.bandwidth_hz.0,
            c.settled.name(),
            c.step
        ),
        None => format!("cannot tell: {}", c.reason.as_deref().unwrap_or_default()),
    }
}

/// A `job_start` of `job` on `source` (its kind, centre and width, the `deviation_hz`
/// and `sideband` of its parameters), applied as a caller of the node would, in its own
/// task: a concrete handle keeps the job's loop out of the start's future type.
fn start_task(
    inner: &Arc<Inner>,
    source: &SourceId,
    job: &ChoiceJob,
) -> JoinHandle<Result<OpResult, OpError>> {
    let op = serde_json::json!({
        "op": "job_start", "kind": job.kind, "source": source.0,
        "center_hz": job.center_hz.0, "bandwidth_hz": job.bandwidth_hz.0,
        "deviation_hz": job.params.get("deviation_hz"), "sideband": job.params.get("sideband"),
        "elastic": false, "replace": [],
    });
    let node = Node::handle(inner);
    tokio::spawn(async move {
        let op: Op = serde_json::from_value(op).map_err(|e| OpError::Other {
            message: format!("the choice's job does not start: {e}"),
        })?;
        crate::ops::apply(&node, op, "choose").await
    })
}

/// A `capture_decode` of the capture `slice` through `row` at `band`, owned by the
/// `choose` job `owner` (so the replay is the job's to stop, and goes once it ends), in its
/// own task (see [`start_task`]).
fn replay_task(
    inner: &Arc<Inner>,
    owner: &JobId,
    slice: &str,
    (row, band): (&str, Tuning),
) -> JoinHandle<Result<JobId, NodeError>> {
    let (node, slice, row) = (Node::handle(inner), slice.to_string(), row.to_string());
    let owner = Some(owner.clone());
    tokio::spawn(async move {
        node.capture_decode_owned(&slice, &row, None, false, owner, Some(band))
            .await
    })
}

/// One trial of step 2 or 3, as `context.choose.trials` lists it.
#[derive(Clone, Debug, Serialize)]
struct Trial {
    /// 2 for a replay of the slice, 3 for a job on the live band.
    step: u8,
    row: String,
    /// The trial's job (a replay, or a job on the live band); `null` when it did not
    /// start.
    job: Option<String>,
    /// `proved`, `no_frame`, `unavailable` (the job did not start) or `stopped`
    /// (another trial settled the job, or the job ended first).
    outcome: &'static str,
    evidence: String,
}

/// What the step-2 trials came to.
#[derive(Default)]
struct Replays {
    trials: Vec<Trial>,
    /// The first proof: its row and its `decode <row>: ...` line.
    proved: Option<(String, String)>,
    /// The rows the budget left no time for (none once a trial proved).
    unstarted: Vec<String>,
}

/// The step-2 trial engine: replay the capture `slice` through each of `rows` (each at its
/// own band; `None` for a row the registry no longer has) in order ([`replay_trial`]), each
/// replay owned by the job `owner`, at most `budget.concurrent` at once (and each waiting
/// for a node-wide trial slot), each spawned under `cancel` and judged until `deadline`
/// (the end of the trials' `budget.trial_s`). The first proof stops the trials still
/// running and starts no more; a row not started by the deadline is `unstarted`.
async fn replay_trials(
    inner: &Arc<Inner>,
    (owner, cancel): (&JobId, &CancellationToken),
    slice: &str,
    rows: &[(String, Option<Tuning>)],
    (budget, deadline): (Budget, Instant),
) -> Replays {
    let settle = cancel.child_token();
    let concurrent = usize::try_from(budget.concurrent)
        .unwrap_or(usize::MAX)
        .max(1);
    let mut queue = rows.iter();
    let mut set = JoinSet::new();
    let mut out = Replays::default();
    // The rows whose replay found no trial slot before the deadline.
    let mut slotless = Vec::new();
    loop {
        while !settle.is_cancelled() && set.len() < concurrent && Instant::now() < deadline {
            let Some(row) = queue.next() else { break };
            let at = (inner.clone(), owner.clone(), slice.to_string());
            set.spawn(replay_trial(at, row.clone(), settle.clone(), deadline));
        }
        let Some(done) = set.join_next().await else {
            break;
        };
        // A trial that panicked records nothing; its guard stopped its replay.
        let Ok(done) = done else { continue };
        let (trial, proof) = match done {
            Ok(t) => t,
            Err(row) => {
                slotless.push(row);
                continue;
            }
        };
        if let Some(evidence) = proof.filter(|_| out.proved.is_none()) {
            out.proved = Some((trial.row.clone(), evidence));
            settle.cancel();
        }
        out.trials.push(trial);
    }
    if out.proved.is_none() && !cancel.is_cancelled() {
        let left = queue.map(|(row, _)| row.clone());
        out.unstarted = slotless.into_iter().chain(left).collect();
    }
    out
}

/// Replay `slice` through `row` at its band, the replay owned by `owner` ([`replay_task`]),
/// once a node-wide trial slot is free ([`Inner::trial_slot`]), and wait for the row's
/// proof on the replay's events ([`Inner::await_proof`] on the replay feed, subscribed
/// before the replay starts) until `deadline`, or until `settle` is cancelled; the replay
/// is stopped either way. The trial and, for a proof, its `decode <row>: ...` line; the row
/// itself when no slot came free before the deadline (or `settle`).
async fn replay_trial(
    (inner, owner, slice): (Arc<Inner>, JobId, String),
    (row, band): (String, Option<Tuning>),
    settle: CancellationToken,
    deadline: Instant,
) -> Result<(Trial, Option<String>), String> {
    let trial = |job: Option<&JobId>, outcome, evidence| Trial {
        step: 2,
        row: row.clone(),
        job: job.map(|j| j.0.clone()),
        outcome,
        evidence,
    };
    let modes = inner.modes();
    let (Some(mode), Some(band)) = (modes.get(&row), band) else {
        return Ok((trial(None, "unavailable", format!("no row {row}")), None));
    };
    let Some(_slot) = inner
        .trial_slot(TrialClass::Replay, &settle, deadline)
        .await
    else {
        return Err(row);
    };
    let feed = inner
        .bus
        .feed_of(&SourceId(REPLAY_SOURCE.into()))
        .and_then(|f| inner.bus.subscribe(&f));
    let Some(feed) = feed else {
        let why = "the replay feed is not open".to_string();
        return Ok((trial(None, "unavailable", why), None));
    };
    let job = match replay_task(&inner, &owner, &slice, (&row, band)).await {
        Ok(Ok(job)) => job,
        Ok(Err(e)) => return Ok((trial(None, "unavailable", e.to_string()), None)),
        Err(e) => {
            return Ok((
                trial(None, "unavailable", format!("replay task: {e}")),
                None,
            ));
        }
    };
    let mut guard = TrialGuard {
        inner: inner.clone(),
        job: Some(job.clone()),
    };
    let dwell_ms = u64::try_from(
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    let attempt = inner
        .await_proof(&settle, mode, &job, ProofFrom::Replay(feed), dwell_ms)
        .await;
    guard.stop().await;
    Ok(match attempt {
        Attempt::Proved { evidence, .. } => {
            let t = trial(Some(&job), "proved", evidence.clone());
            (t, Some(evidence))
        }
        Attempt::NoFrame(why) => (trial(Some(&job), "no_frame", why), None),
        Attempt::Cancelled => {
            let why = "the job settled or ended first".to_string();
            (trial(Some(&job), "stopped", why), None)
        }
    })
}

/// A trial's job (a replay, or a job on the live band): stopped by [`TrialGuard::stop`],
/// or, on any other exit of the trial (a panic, an abort), from a task the drop spawns.
struct TrialGuard {
    inner: Arc<Inner>,
    job: Option<JobId>,
}

impl TrialGuard {
    async fn stop(&mut self) {
        if let Some(job) = self.job.take() {
            let _ = self.inner.stop(&job).await;
        }
    }
}

impl Drop for TrialGuard {
    fn drop(&mut self) {
        let Some(job) = self.job.take() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let inner = self.inner.clone();
        rt.spawn(async move {
            let _ = inner.stop(&job).await;
        });
    }
}

/// A trial capture pinned while the job's ladder runs (stored pinned by
/// [`crate::bursts::BurstStore::write_trial`]): unpinned on drop, however the job ends.
struct Pinned {
    store: Arc<crate::bursts::BurstStore>,
    id: String,
}

impl Drop for Pinned {
    fn drop(&mut self) {
        let _ = self.store.unpin(&self.id);
    }
}

/// The ring slice the trial steps replay: its capture, the selection it holds (the
/// job's, shortened to the trial quota) and the pin that keeps it while the job runs.
struct TrialSlice {
    id: String,
    sel: Selection,
    _pinned: Pinned,
}

/// The newest `slice_s` seconds of the span `asked` (`None`: all of it) in a ring whose
/// newest contiguous run starts at `run_start_ns` and ends at `latest_ns`:
/// `(from_ns, to_ns)`. A run shorter than that starts [`RING_MARGIN_NS`] after its
/// oldest sample, which the next block written may evict before the selection is read;
/// `None` when nothing is left (the run holds none of the span).
fn slice_span(
    run_start_ns: u64,
    latest_ns: u64,
    slice_s: f32,
    asked: Option<(u64, u64)>,
) -> Option<(u64, u64)> {
    let want_ns = (f64::from(slice_s) * 1e9) as u64;
    let oldest = run_start_ns.saturating_add(RING_MARGIN_NS);
    let (lo, hi) = asked.unwrap_or((0, latest_ns));
    let to = hi.min(latest_ns);
    let from = to.saturating_sub(want_ns).max(oldest).max(lo);
    (to > from).then_some((from, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(layer: u8, name: &str, evidence: &str) -> crate::classify::Label {
        crate::classify::Label {
            layer,
            name: name.into(),
            confidence: 0.5,
            evidence: evidence.into(),
        }
    }

    /// Each layer's labels keep their source; a layer-2 label of neither convention is
    /// kept as `layer2`, never dropped; the answer is the choice's row, step and settled.
    #[test]
    fn a_training_record_names_each_source_and_the_answer() {
        let c = Classification {
            labels: vec![
                label(2, "MPT 1327", "catalogue mpt: 1327"),
                label(2, "learned pager", "learned abc: centre"),
                label(2, "other", "something else"),
                label(3, "dmr", ""),
            ],
            ..Classification::default()
        };
        let choice = Choice {
            job: None,
            row: Some("dmr_decode".into()),
            step: 3,
            settled: Settled::Proved,
            evidence: vec![],
            runner_up: None,
            could_prove: vec![],
            not_tried: vec![],
            reason: None,
            steps_run: vec![1, 3],
        };
        let t = training_record(&c, &choice);
        let got: Vec<(&str, &str)> = t["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["source"].as_str().unwrap(), s["guess"].as_str().unwrap()))
            .collect();
        assert_eq!(
            got,
            [
                ("catalogue", "MPT 1327"),
                ("learned", "learned pager"),
                ("layer2", "other"),
                ("layer3", "dmr")
            ]
        );
        assert_eq!(
            t["settled"],
            serde_json::json!({"row": "dmr_decode", "step": 3, "settled": "proved"})
        );
    }

    /// A step-3 trial takes every slot but one, and none while a replay waits; a replay
    /// takes any free slot; with one slot, either takes it.
    #[test]
    fn step_three_leaves_a_slot_for_replays_and_waits_behind_them() {
        let slots = |replay, live, replays_waiting| Slots {
            replay,
            live,
            replays_waiting,
        };
        let (r, l) = (TrialClass::Replay, TrialClass::Live);
        assert!(slots(0, 1, 0).admits(l, 3));
        assert!(!slots(0, 2, 0).admits(l, 3));
        assert!(slots(0, 2, 0).admits(r, 3));
        assert!(!slots(1, 2, 0).admits(r, 3));
        assert!(!slots(1, 0, 1).admits(l, 3));
        assert!(slots(1, 0, 0).admits(l, 3));
        // One slot: shared, nothing kept back.
        assert!(slots(0, 0, 0).admits(l, 1));
        assert!(slots(0, 0, 0).admits(r, 1));
        assert!(!slots(0, 1, 0).admits(r, 1));
        assert!(!slots(0, 0, 1).admits(l, 1));
        // A policy of 0 slots is read as 1.
        assert!(slots(0, 0, 0).admits(l, 0));
        assert_eq!((live_limit(1), live_limit(2), live_limit(3)), (1, 1, 2));
    }

    /// A cutter that notes what it was asked and answers `n` samples at 48 kS/s, or
    /// nothing.
    struct Noting {
        asked: std::sync::Mutex<Vec<(i64, Hz)>>,
        cuts: bool,
    }

    impl BandCutter for Noting {
        fn cut(
            &self,
            _: &[Complex<f32>],
            _: Hz,
            offset_hz: i64,
            bandwidth_hz: Hz,
        ) -> Option<(Vec<Complex<f32>>, Hz)> {
            self.asked.lock().unwrap().push((offset_hz, bandwidth_hz));
            self.cuts
                .then(|| (vec![Complex::new(1.0, 0.0); 10], Hz(48_000)))
        }
    }

    fn tuning(center_hz: u64, bandwidth_hz: u64) -> Tuning {
        Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
        }
    }

    /// One band is cut as it is; a union of bands that differ is cut with its guard while
    /// the capture holds that, else bare; the record keeps the bare band and the file is
    /// centred on it. No cutter, or a cut refused, keeps the block whole.
    #[tokio::test]
    async fn the_slice_is_cut_to_its_band_and_a_union_with_a_guard() {
        let noting = |cuts| {
            Arc::new(Noting {
                asked: std::sync::Mutex::new(Vec::new()),
                cuts,
            })
        };
        let (ring, rate) = (Hz(10_000_000), Hz(1_000_000));
        let block = || (vec![Complex::new(0.0, 0.0); 100], rate, ring);
        let one = tuning(10_100_000, 12_500);
        let c = noting(true);
        let k = keep_for_trials(Some(c.clone()), (one, false), block())
            .await
            .unwrap();
        assert_eq!(
            (k.rate, k.file_center, k.band),
            (Hz(48_000), one.center_hz, one)
        );
        assert_eq!(k.samples.len(), 10);
        assert_eq!(*c.asked.lock().unwrap(), [(100_000, Hz(12_500))]);
        // A union: 1.25 times as wide, the record's band the bare union.
        let union = tuning(10_100_000, 40_000);
        let c = noting(true);
        let k = keep_for_trials(Some(c.clone()), (union, true), block())
            .await
            .unwrap();
        assert_eq!(k.band, union);
        assert_eq!(*c.asked.lock().unwrap(), [(100_000, Hz(50_000))]);
        // A union whose guard reaches past the capture: cut bare.
        let edge = tuning(10_500_000 - 20_000, 40_000);
        let c = noting(true);
        keep_for_trials(Some(c.clone()), (edge, true), block()).await;
        assert_eq!(*c.asked.lock().unwrap(), [(480_000, Hz(40_000))]);
        // No cutter, or a cut refused: the ring's block, rate and centre.
        for cutter in [None, Some(noting(false) as Arc<dyn BandCutter>)] {
            let k = keep_for_trials(cutter, (one, false), block())
                .await
                .unwrap();
            assert_eq!((k.rate, k.file_center, k.band), (rate, ring, one));
            assert_eq!(k.samples.len(), 100);
        }
    }

    const S: u64 = 1_000_000_000;

    /// A ring holding more than the slice gives the newest slice; one holding less gives
    /// what it holds but its oldest margin, which a block written meanwhile may evict.
    #[test]
    fn the_slice_keeps_clear_of_the_oldest_samples() {
        assert_eq!(slice_span(0, 10 * S, 2.0, None), Some((8 * S, 10 * S)));
        assert_eq!(
            slice_span(9 * S, 10 * S, 2.0, None),
            Some((9 * S + RING_MARGIN_NS, 10 * S))
        );
        // A run of exactly the slice still keeps the margin.
        assert_eq!(
            slice_span(8 * S, 10 * S, 2.0, None),
            Some((8 * S + RING_MARGIN_NS, 10 * S))
        );
        // A run no longer than the margin leaves nothing.
        assert_eq!(slice_span(10 * S - RING_MARGIN_NS, 10 * S, 2.0, None), None);
        assert_eq!(slice_span(10 * S, 10 * S, 2.0, None), None);
    }

    /// An asked span gives its newest slice inside the run: cut to the slice, cut at
    /// the newest sample, kept clear of the oldest, and nothing once the run has moved
    /// past it.
    #[test]
    fn an_asked_span_gives_its_newest_slice_the_ring_holds() {
        let at = |lo: u64, hi: u64| slice_span(4 * S, 10 * S, 2.0, Some((lo * S, hi * S)));
        assert_eq!(at(5, 9), Some((7 * S, 9 * S)));
        assert_eq!(at(6, 7), Some((6 * S, 7 * S)));
        assert_eq!(at(9, 12), Some((9 * S, 10 * S)));
        assert_eq!(at(3, 5), Some((4 * S + RING_MARGIN_NS, 5 * S)));
        assert_eq!(at(1, 3), None);
    }
}
