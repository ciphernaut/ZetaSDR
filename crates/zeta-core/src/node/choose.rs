//! The `choose` job (design 5.3): a controller that climbs the proof ladder for its band
//! as far as its context's `choose.step` allows (the choice policy's step for the surface
//! that asked, [`crate::choice_policy::ChoicePolicy::steps`]), taking no lease.
//!
//! Its selection is the job's band over the newest `budget.slice_s` seconds of the span
//! it was asked (the page's box), else of the source's ring (what the ring holds, when
//! less; a span the ring no longer holds is a `cannot_tell`). Step 1 classifies it at
//! once, as `classify` does, and chooses ([`choose_now`]); a watch's own `choose` skips
//! it, taking the watch's classification of the signal and the samples it read
//! ([`StepOne`]), so the band is classified once. Step 2 writes the same slice into the
//! burst store as a trial capture (pinned while the job runs) and replays it through the
//! `could_prove` rows ([`replay_trials`]), at most `budget.concurrent` trials at once
//! across every `choose` of the node ([`Inner::trial_slot`]), each judged by its row's
//! identify rule within `budget.trial_s` in all; the first proof settles the job. A
//! slice under 1 s, or one the trial quota cannot hold even shortened to 1 s beside the
//! slices other chooses have pinned, is not tried, and `not_tried` says why. Step 3
//! tries the rows still in `could_prove` on the live band, one at a time in score order
//! within what is left of `budget.trial_s` ([`Inner::step_three`]), each holding one of
//! the node's trial slots: each as an ordinary job the `choose` owns, started only where
//! a running capture already holds the row's band (lease setting `share`), or after
//! growing a held capture to hold it, every holder keeping its band (`widen`). A trial's
//! lease gives way to any foreground request that changes the capture.
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
    Alternative, Choice, ChoiceJob, NotTried, Settled, catalogue_entry, choose_now, job_for,
};
use crate::choice_policy::{Budget, LeaseSetting, Surface};
use crate::classify::{Classification, Measurement};
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
    /// Held across a slice's sizing, write and pin ([`Inner::write_slice`]), so two
    /// chooses never both fit beside the same pinned bytes.
    slice_gate: std::sync::Mutex<()>,
    /// The trials running now across every `choose` ([`Inner::trial_slot`]).
    trials: std::sync::Mutex<u32>,
    /// The most trials ever running at once (tests).
    peak: AtomicU32,
    /// Woken whenever a trial ends, for the trials waiting for a slot.
    freed: tokio::sync::Notify,
    /// The step 1 a watch hands its `choose` child ([`StepOne`]), by the child's id, until
    /// the child takes it as it starts.
    seeds: std::sync::Mutex<HashMap<JobId, StepOne>>,
    /// How many bands a `choose` classified itself (tests).
    classified: AtomicU64,
}

/// A `choose`'s step 1: the selection, the span's length in ns, its classification,
/// the samples it read (at the ring's `rate`, centred `ring_center`) and the answer.
/// A watch hands its own to its `choose` child, which then classifies nothing again.
pub(super) struct StepOne {
    pub sel: Selection,
    pub held_ns: u64,
    pub classified: Classification,
    pub samples: Vec<Complex<f32>>,
    pub rate: Hz,
    pub ring_center: Hz,
    pub choice: Choice,
}

/// One running trial's slot ([`Inner::trial_slot`]), given back on drop.
struct TrialSlot {
    inner: Arc<Inner>,
}

impl Drop for TrialSlot {
    fn drop(&mut self) {
        let c = &self.inner.choosing;
        *c.trials.lock().unwrap() -= 1;
        c.freed.notify_waiters();
    }
}

#[cfg(feature = "test-fixtures")]
impl Node {
    /// Tests only: the most trials (step 2's replays and step 3's live jobs) that ever
    /// ran at once across the node's chooses.
    pub fn test_trial_peak(&self) -> u32 {
        self.inner.choosing.peak.load(Ordering::SeqCst)
    }

    /// Tests only: how many bands the node's chooses classified themselves (a watch's
    /// child takes the watch's step 1 instead).
    pub fn test_choose_classifications(&self) -> u64 {
        self.inner.choosing.classified.load(Ordering::SeqCst)
    }
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

    /// Write `samples` (the selection `sel` of the ring at `rate`, centred
    /// `ring_center`) as a trial capture, shortened to the newest the trial quota holds
    /// beside the trial slices other chooses have pinned, and pin it for the job. The
    /// `not tried: ...` reason when it cannot: the quota holds under 1 s of it
    /// ([`OVER_QUOTA`]), or the write or the pin fails.
    fn write_slice(
        &self,
        node: &Node,
        mut sel: Selection,
        (mut samples, rate, ring_center): (Vec<Complex<f32>>, Hz, Hz),
    ) -> Result<TrialSlice, String> {
        let _gate = self.choosing.slice_gate.lock().unwrap();
        let room = self
            .bursts
            .policy()
            .trial_keep_bytes
            .saturating_sub(self.bursts.pinned_trial_bytes());
        let fits = usize::try_from(room / SAMPLE_BYTES).unwrap_or(usize::MAX);
        if samples.len() > fits {
            if (fits as u64) < rate.0 {
                return Err(OVER_QUOTA.into());
            }
            let cut = samples.len() - fits;
            samples.drain(..cut);
            sel.span.start_ns.0 += cut as u64 * 1_000_000_000 / rate.0.max(1);
        }
        let rec = crate::ops::write_ring_slice(node, &sel, &samples, rate, ring_center, true)
            .map_err(|e| format!("not tried: the slice could not be written: {e}"))?;
        let pinned = Pinned::new(self.bursts.clone(), &rec.id)
            .map_err(|e| format!("not tried: the slice could not be pinned: {e}"))?;
        Ok(TrialSlice {
            id: rec.id,
            sel,
            _pinned: pinned,
        })
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
        let rows = &choice.could_prove;
        let replays = replay_trials(self, job, &slice.id, rows, (budget, deadline)).await;
        self.add_trials(r, &replays.trials);
        if r.cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        let modes = self.modes();
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
            return Ok(proved_at(choice, (2, &mode.row.id.0), job, vec![evidence]));
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

    /// Drop a step 1 handed to the `choose` `id` that it has not taken (it never started).
    pub(super) fn choosing_unseed(&self, id: &JobId) {
        self.choosing.seeds.lock().unwrap().remove(id);
    }

    /// A slot for one trial, waited for while the node's chooses already run the current
    /// policy's `budget.concurrent` between them: `None` when `cancel` fires or
    /// `deadline` passes first.
    async fn trial_slot(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Option<TrialSlot> {
        let c = &self.choosing;
        loop {
            let freed = c.freed.notified();
            tokio::pin!(freed);
            // Registered before the count is read: a slot given back meanwhile wakes it.
            freed.as_mut().enable();
            {
                let limit = self.choice_policy.lock().unwrap().budget.concurrent.max(1);
                let mut running = c.trials.lock().unwrap();
                if *running < limit {
                    *running += 1;
                    c.peak.fetch_max(*running, Ordering::SeqCst);
                    return Some(TrialSlot {
                        inner: self.clone(),
                    });
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

    /// Step 3: try each of `choice.could_prove` (in its order, the score's) on the live
    /// band, one at a time, until `deadline`: the row's job for the measurement `m` in
    /// `sel` ([`job_for`]), started on `port` as an ordinary job the `choose` owns
    /// ([`Inner::trial_row`]) only where a running capture already holds its band
    /// untuned, and judged by the row's identify rule for its dwell (cut to what is left
    /// of the budget, its start included). Under [`LeaseSetting::Widen`] a band outside
    /// the running capture first grows it ([`widen_for`]; `widened the capture to
    /// <centre>/<width> Hz for a trial` in the answer's evidence when the capture moved);
    /// a row still uncovered is [`NOT_COVERED`], one the budget leaves no time for
    /// [`OVER_BUDGET`], one whose trial a foreground request took the capture from
    /// [`GAVE_WAY`] (its job stopped, the next row tried). Every trial is in
    /// `context.choose.trials` (`step: 3`). The answer: `choice` revised by the first
    /// proof (step 3, `proved`, the old answer its runner-up) with, when `keep`, the
    /// proved job still running in its guard; else `choice` with the notes. Every other
    /// trial job is stopped. `Cancelled` when the job is.
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
        for row in &choice.could_prove {
            // Step 2 found the budget spent before this row.
            if !budget_left(choice, row) {
                continue;
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
            let Some(_slot) = self.trial_slot(&r.cancel, deadline).await else {
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

/// The `choose` job's body: (1) the watch's step 1 its child was handed ([`StepOne`]), else
/// [`step_one`] (none is a `cannot_tell`), the policy's cap noted; (2) when the job's
/// step reaches 2 and step 1 proved nothing, the same samples written as the trial
/// slice ([`Inner::write_slice`], pinned until just before the verdict) and
/// [`Inner::step_two`] over it, unless the policy's slice is under 1 s
/// ([`POLICY_SLICE_SHORT`]), the span asked is ([`SPAN_SHORT`]), the selection is
/// ([`RING_SHORT`]) or layer 1 measured
/// nothing ([`UNMEASURED`]); (3) when the job's step reaches 3 and
/// nothing proved yet, [`Inner::step_three`] on the live band with what is left of the
/// trials' budget; then the verdict, and with `start` the verdict's job: a step-3
/// proof's own job, kept running, its lease made an ordinary one and the job disowned
/// (unless its lease gave way first: then it is stopped and the job started afresh),
/// else one started.
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
        samples,
        rate,
        ring_center,
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
        match inner.write_slice(&node, sel.clone(), (samples, rate, ring_center)) {
            Ok(s) => {
                r.ctx.insert("slice".into(), s.id.clone().into());
                slice = Some(s);
            }
            Err(reason) => note(&mut choice, reason),
        }
    }
    inner.publish_choice(&mut r, &choice);
    // Both trial steps share the budget's seconds.
    let deadline = Instant::now() + Duration::from_secs(budget.trial_s.into());
    if let (Some(s), Some(m)) = (&slice, &classified.measurement) {
        let revised = inner
            .step_two(&mut r, &choice, m, s, (budget, deadline))
            .await?;
        if revised != choice {
            choice = revised;
            inner.publish_choice(&mut r, &choice);
        }
    }
    // The proved step-3 job, kept to be the started one.
    let mut kept = None;
    if r.ask.step >= 3
        && choice.settled != Settled::Proved
        && let Some(m) = &classified.measurement
    {
        let on = (&port, lease);
        let at = (deadline, r.ask.start);
        let (revised, k) = inner.step_three(&mut r, on, &choice, (m, &sel), at).await?;
        kept = k;
        if revised != choice {
            choice = revised;
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
                let modes = inner.modes();
                let choice =
                    choose_now(&modes, modes.weights(), &classified, &sel, &inner.region());
                return Ok(Ok(StepOne {
                    sel,
                    held_ns,
                    classified,
                    samples,
                    rate,
                    ring_center,
                    choice,
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
    }
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

/// A `capture_decode` of the capture `slice` through `row`, owned by the `choose` job
/// `owner` (so the replay is the job's to stop, and goes once it ends), in its own task
/// (see [`start_task`]).
fn replay_task(
    inner: &Arc<Inner>,
    owner: &JobId,
    slice: &str,
    row: &str,
) -> JoinHandle<Result<JobId, NodeError>> {
    let (node, slice, row) = (Node::handle(inner), slice.to_string(), row.to_string());
    let owner = Some(owner.clone());
    tokio::spawn(async move {
        node.capture_decode_owned(&slice, &row, None, false, owner)
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

/// The step-2 trial engine: replay the capture `slice` through each of `rows` in order
/// ([`replay_trial`]), each replay owned by the job `owner`, at most `budget.concurrent`
/// at once (and each waiting for a node-wide trial slot), each spawned under `cancel` and
/// judged until `deadline` (the end of the trials' `budget.trial_s`). The first proof
/// stops the trials still running and starts no more; a row not started by the deadline
/// is `unstarted`.
async fn replay_trials(
    inner: &Arc<Inner>,
    (owner, cancel): (&JobId, &CancellationToken),
    slice: &str,
    rows: &[String],
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
        out.unstarted = slotless.into_iter().chain(queue.cloned()).collect();
    }
    out
}

/// Replay `slice` through `row`, the replay owned by `owner` ([`replay_task`]), once a
/// node-wide trial slot is free ([`Inner::trial_slot`]), and wait for the row's proof on
/// the replay's events ([`Inner::await_proof`] on the replay feed, subscribed before the
/// replay starts) until `deadline`, or until `settle` is cancelled; the replay is
/// stopped either way. The trial and, for a proof, its `decode <row>: ...` line; the
/// row itself when no slot came free before the deadline (or `settle`).
async fn replay_trial(
    (inner, owner, slice): (Arc<Inner>, JobId, String),
    row: String,
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
    let Some(mode) = modes.get(&row) else {
        return Ok((trial(None, "unavailable", format!("no row {row}")), None));
    };
    let Some(_slot) = inner.trial_slot(&settle, deadline).await else {
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
    let job = match replay_task(&inner, &owner, &slice, &row).await {
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

/// A trial capture pinned while the job's ladder runs: unpinned on drop, however the
/// job ends, unless it was pinned already (then this neither pinned nor unpins it).
struct Pinned {
    store: Arc<crate::bursts::BurstStore>,
    id: String,
    /// Whether this guard pinned it.
    ours: bool,
}

impl Pinned {
    /// Pin `id` in `store`; the store's refusal, worded, when it cannot.
    fn new(store: Arc<crate::bursts::BurstStore>, id: &str) -> Result<Self, String> {
        let ours = !store.get(id).is_some_and(|r| r.pinned);
        if ours {
            store.pin(id).map_err(|e| e.to_string())?;
        }
        Ok(Self {
            store,
            id: id.to_string(),
            ours,
        })
    }
}

impl Drop for Pinned {
    fn drop(&mut self) {
        if self.ours {
            let _ = self.store.unpin(&self.id);
        }
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
