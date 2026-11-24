//! The `choose` job (design 5.3): a controller that climbs the proof ladder for its band
//! as far as its context's `choose.step` allows (the choice policy's step for the surface
//! that asked, [`crate::choice_policy::ChoicePolicy::steps`]), taking no lease.
//!
//! Its selection is the job's band over the newest `budget.slice_s` seconds of the
//! source's ring (what the ring holds, when less). Step 1 classifies it at once, as
//! `classify` does, and chooses ([`choose_now`]). Step 2 writes the same slice into the
//! burst store as a trial capture (pinned while the job runs) and replays it through the
//! `could_prove` rows ([`replay_trials`]), `budget.concurrent` at a time, each judged by
//! its row's identify rule within `budget.trial_s` in all; the first proof settles the
//! job. A slice under 1 s, or one the trial quota cannot hold even shortened to 1 s, is
//! not tried, and `not_tried` says why.
//!
//! The job's event stream carries `{"protocol": "choice", "step", "choice"}` when step
//! 1 answers and whenever the answer changes, then `{"protocol": "verdict", "choice"}`;
//! `context.choose` keeps the ask, `choice` (the latest), `slice`, `trials` and
//! `verdict`. With `start` the verdict's job is started on the job's source (`started`,
//! or `start_error`). The job ends `stopped`, a `cannot_tell` included: a source with
//! no ring answers `cannot_tell`, its reason that the source is not capturing.

use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::OwnedMutexGuard;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::identify::{Attempt, ProofFrom};
use super::launch::{AfterGate, Ending, StageRunning, supervise};
use super::persist::now;
use super::{Inner, Node, NodeError, REPLAY_SOURCE, source_fault_reason};
use crate::choice::{Alternative, Choice, ChoiceJob, NotTried, Settled, choose_now, job_for};
use crate::choice_policy::{Budget, Surface};
use crate::classify::Measurement;
use crate::graph::{Graph, GraphId};
use crate::job::JobRecord;
use crate::ops::{JobKind, Op, OpError, OpResult};
use crate::port::{Port, Tuning};
use crate::ring::RingError;
use crate::stage::StageError;
use crate::stream::{Frame, Payload};
use crate::types::{Hz, JobId, Provenance, Selection, SourceId, Span, StreamId, StreamKind};

/// Step 2's note when the ring holds under a second of the band.
pub const RING_SHORT: &str = "not tried: the ring holds under 1 s";
/// Step 2's note when the trial quota cannot hold a second of the slice.
pub const OVER_QUOTA: &str = "not tried: the slice exceeds the trial quota";
/// The note of a row the trial budget left no time for.
pub const OVER_BUDGET: &str = "not tried: budget";
/// The trial steps' note when layer 1 measured nothing to size a proved job by.
pub const UNMEASURED: &str = "not tried: layer 1 did not measure the band";
/// The shortest slice step 2 replays.
const MIN_SLICE_NS: u64 = 1_000_000_000;
/// How far after a ring's oldest sample a selection of all it holds starts: the next
/// block written may evict that much before the selection is read (a ring of 1 s
/// written in 100 ms blocks).
const RING_MARGIN_NS: u64 = 100_000_000;
/// Bytes one sample of a slice takes in the burst store (`cf32`).
const SAMPLE_BYTES: u64 = 8;

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
    /// A short-lived ask (no effect yet).
    pub transient: bool,
}

impl Node {
    /// A `Node` over `inner` for a controller job's own task, so the job applies what a
    /// caller of the node would: it shares the node, and its drop shuts nothing down.
    fn handle(inner: &Arc<Inner>) -> Node {
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
        let graph = Graph {
            id: GraphId::new(),
            nodes: vec![],
            edges: vec![],
        };
        let rec = JobRecord {
            context: serde_json::json!({ "choose": ask }),
            ..super::running_record(JobKind::Choose.name(), source, tuning, graph, None, false)
        };
        let id = rec.id.clone();
        self.inner.launch(rec).await?;
        Ok(id)
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
                }
            });
        let ctx = match serde_json::to_value(&ask) {
            Ok(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        let (id, source, tuning) = (rec.id.clone(), rec.source.clone(), rec.tuning);
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
            ctx,
        };
        let mut set = JoinSet::new();
        let (running, body) = (
            StageRunning::new(self.stages.clone()),
            choose_loop(self.clone(), r, port),
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
        let payload = serde_json::json!({
            "protocol": "verdict",
            "choice": choice,
            "summary": summary(c),
        });
        self.choose_event(r, payload);
    }

    /// The job's selection: its band over the newest `slice_s` seconds of the source's
    /// ring, or the newest contiguous run when that is shorter, with the span's length
    /// in ns. `None` while the ring holds nothing.
    fn newest_slice(&self, r: &Run, slice_s: f32) -> Option<(Selection, u64)> {
        let ring = self.rings.lock().unwrap().get(&r.source).cloned()?;
        let ring = ring.read().unwrap();
        let (start, latest) = (ring.latest_run_start()?, ring.latest()?);
        let (from, held_ns) = slice_span(start.0, latest.0, slice_s)?;
        let sel = Selection {
            source: r.source.clone(),
            span: Span {
                start_ns: crate::types::Timestamp(from),
                end_ns: latest,
            },
            center_hz: r.tuning.center_hz,
            bandwidth_hz: r.tuning.bandwidth_hz,
        };
        Some((sel, held_ns))
    }

    /// Write `samples` (the selection `sel` of the ring at `rate`, centred
    /// `ring_center`) as a trial capture, shortened to the newest the trial quota holds,
    /// and pin it for the job. The `not tried: ...` reason when it cannot: the quota
    /// holds under 1 s of it ([`OVER_QUOTA`]), or the write or the pin fails.
    fn write_slice(
        &self,
        node: &Node,
        mut sel: Selection,
        (mut samples, rate, ring_center): (Vec<Complex<f32>>, Hz, Hz),
    ) -> Result<TrialSlice, String> {
        let fits = usize::try_from(self.bursts.policy().trial_keep_bytes / SAMPLE_BYTES)
            .unwrap_or(usize::MAX);
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
    /// ([`replay_trials`]). The answer: `choice` revised by the first proof (step 2,
    /// `proved`, the old answer its runner-up), or `choice` with [`OVER_BUDGET`] for the
    /// rows the budget left untried. `Cancelled` when the job is.
    async fn step_two(
        self: &Arc<Self>,
        r: &mut Run,
        choice: &Choice,
        m: &Measurement,
        slice: &TrialSlice,
        budget: Budget,
    ) -> Result<Choice, StageError> {
        let mut next = choice.clone();
        let sel = &slice.sel;
        let job = (&r.id, &r.cancel);
        let replays = replay_trials(self, job, &slice.id, &choice.could_prove, budget).await;
        r.ctx.insert(
            "trials".into(),
            serde_json::to_value(&replays.trials).expect("trials serialise"),
        );
        self.choose_context(r);
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
            return Ok(proved_at_two(choice, &mode.row.id.0, job, evidence));
        }
        next.not_tried
            .extend(replays.unstarted.into_iter().map(|row| NotTried {
                row: Some(row),
                reason: OVER_BUDGET.into(),
            }));
        Ok(next)
    }
}

/// The `choose` job's body: (1) the selection ([`Inner::newest_slice`]; none is a
/// `cannot_tell`, and one the ring moved past before it was read is taken again once),
/// classified and chosen ([`choose_now`]), the policy's cap noted; (2) when the job's
/// step reaches 2 and step 1 proved nothing, the same samples written as the trial
/// slice ([`Inner::write_slice`], pinned until just before the verdict) and
/// [`Inner::step_two`] over it, unless the selection is under 1 s ([`RING_SHORT`]) or
/// layer 1 measured nothing ([`UNMEASURED`]); then the verdict, and with `start` the
/// verdict's job started.
async fn choose_loop(inner: Arc<Inner>, mut r: Run, port: Arc<dyn Port>) -> Result<(), StageError> {
    if let Some(f) = port.fault() {
        return Err(StageError::Failed(source_fault_reason(port.id(), &f)));
    }
    let node = Node::handle(&inner);
    let budget = inner.choice_policy.lock().unwrap().budget;
    let capped = r
        .ask
        .capped
        .clone()
        .map(|reason| NotTried { row: None, reason });
    let not_capturing = || format!("source {} is not capturing", r.source.0);
    // A selection the ring moved past before it was read (a block evicted its oldest
    // samples in between) is taken again once, from the ring as it is then.
    let mut classified = None;
    for _ in 0..2 {
        let Some((sel, held_ns)) = inner.newest_slice(&r, budget.slice_s) else {
            break;
        };
        let c = tokio::select! {
            c = crate::ops::classify_samples(&node, &sel) => c,
            _ = r.cancel.cancelled() => return Err(StageError::Cancelled),
        };
        let too_old = matches!(
            &c,
            Err(OpError::Ring {
                detail: RingError::TooOld { .. }
            })
        );
        classified = Some((sel, held_ns, c));
        if !too_old {
            break;
        }
    }
    let (sel, held_ns, classified) = match classified {
        Some((sel, held_ns, Ok(c))) => (sel, held_ns, c),
        found => {
            let reason = match found {
                Some((.., Err(e))) => e.to_string(),
                _ => not_capturing(),
            };
            let c = cannot_tell(reason, capped.into_iter().collect());
            inner.publish_choice(&mut r, &c);
            inner.choose_verdict(&mut r, &c);
            return Ok(());
        }
    };
    let (classified, samples, rate, ring_center) = classified;
    node.supersede_proved(&classified.tried, r.tuning);
    let modes = inner.modes();
    let mut choice = choose_now(&modes, modes.weights(), &classified, &sel, &inner.region());
    choice.not_tried.extend(capped);
    let step_two = r.ask.step >= 2 && choice.settled != Settled::Proved;
    let note = |c: &mut Choice, reason: String| c.not_tried.push(NotTried { row: None, reason });
    // The trial slice lives until the verdict, pinned, for every trial step.
    let mut slice = None;
    if step_two && held_ns < MIN_SLICE_NS {
        note(&mut choice, RING_SHORT.into());
    } else if step_two && classified.measurement.is_none() {
        note(&mut choice, UNMEASURED.into());
    } else if step_two && !choice.could_prove.is_empty() {
        match inner.write_slice(&node, sel, (samples, rate, ring_center)) {
            Ok(s) => {
                r.ctx.insert("slice".into(), s.id.clone().into());
                slice = Some(s);
            }
            Err(reason) => note(&mut choice, reason),
        }
    }
    inner.publish_choice(&mut r, &choice);
    if let (Some(s), Some(m)) = (&slice, &classified.measurement) {
        let revised = inner.step_two(&mut r, &choice, m, s, budget).await?;
        if revised != choice {
            choice = revised;
            inner.publish_choice(&mut r, &choice);
        }
    }
    // Unpinned before the verdict's job starts and the verdict is published.
    drop(slice);
    if r.ask.start
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

/// `prev` settled by a step-2 decode of `row` (`evidence` its `decode <row>: ...`
/// line): `job`, `proved` at step 2 with layer 1's line and the decode as evidence,
/// `prev`'s answer its runner-up, `row` out of `could_prove`.
fn proved_at_two(prev: &Choice, row: &str, job: ChoiceJob, evidence: String) -> Choice {
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
        step: 2,
        settled: Settled::Proved,
        evidence: layer1.chain(std::iter::once(evidence)).collect(),
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

/// One trial of step 2, as `context.choose.trials` lists it.
#[derive(Clone, Debug, Serialize)]
struct Trial {
    row: String,
    /// The replay job; `null` when it did not start.
    job: Option<String>,
    /// `proved`, `no_frame`, `unavailable` (the replay did not start) or `stopped`
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
/// at once, each spawned under `cancel` and judged until `budget.trial_s` from now has
/// passed. The first proof stops the trials still running and starts no more; a row not
/// started by the deadline is `unstarted`.
async fn replay_trials(
    inner: &Arc<Inner>,
    (owner, cancel): (&JobId, &CancellationToken),
    slice: &str,
    rows: &[String],
    budget: Budget,
) -> Replays {
    let deadline = Instant::now() + Duration::from_secs(budget.trial_s.into());
    let settle = cancel.child_token();
    let concurrent = usize::try_from(budget.concurrent)
        .unwrap_or(usize::MAX)
        .max(1);
    let mut queue = rows.iter();
    let mut set = JoinSet::new();
    let mut out = Replays::default();
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
        let Ok((trial, proof)) = done else { continue };
        if let Some(evidence) = proof.filter(|_| out.proved.is_none()) {
            out.proved = Some((trial.row.clone(), evidence));
            settle.cancel();
        }
        out.trials.push(trial);
    }
    if out.proved.is_none() && !cancel.is_cancelled() {
        out.unstarted = queue.cloned().collect();
    }
    out
}

/// Replay `slice` through `row`, the replay owned by `owner` ([`replay_task`]), and wait
/// for the row's proof on the replay's events ([`Inner::await_proof`] on the replay
/// feed, subscribed before the replay starts) until `deadline`, or until `settle` is
/// cancelled; the replay is stopped either way. The trial and, for a proof, its
/// `decode <row>: ...` line.
async fn replay_trial(
    (inner, owner, slice): (Arc<Inner>, JobId, String),
    row: String,
    settle: CancellationToken,
    deadline: Instant,
) -> (Trial, Option<String>) {
    let trial = |job: Option<&JobId>, outcome, evidence| Trial {
        row: row.clone(),
        job: job.map(|j| j.0.clone()),
        outcome,
        evidence,
    };
    let modes = inner.modes();
    let Some(mode) = modes.get(&row) else {
        return (trial(None, "unavailable", format!("no row {row}")), None);
    };
    let feed = inner
        .bus
        .feed_of(&SourceId(REPLAY_SOURCE.into()))
        .and_then(|f| inner.bus.subscribe(&f));
    let Some(feed) = feed else {
        let why = "the replay feed is not open".to_string();
        return (trial(None, "unavailable", why), None);
    };
    let job = match replay_task(&inner, &owner, &slice, &row).await {
        Ok(Ok(job)) => job,
        Ok(Err(e)) => return (trial(None, "unavailable", e.to_string()), None),
        Err(e) => {
            return (
                trial(None, "unavailable", format!("replay task: {e}")),
                None,
            );
        }
    };
    let mut guard = ReplayGuard {
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
    match attempt {
        Attempt::Proved { evidence, .. } => {
            let t = trial(Some(&job), "proved", evidence.clone());
            (t, Some(evidence))
        }
        Attempt::NoFrame(why) => (trial(Some(&job), "no_frame", why), None),
        Attempt::Cancelled => {
            let why = "the job settled or ended first".to_string();
            (trial(Some(&job), "stopped", why), None)
        }
    }
}

/// A trial's replay job: stopped by [`ReplayGuard::stop`], or, on any other exit of the
/// trial (a panic, an abort), from a task the drop spawns.
struct ReplayGuard {
    inner: Arc<Inner>,
    job: Option<JobId>,
}

impl ReplayGuard {
    async fn stop(&mut self) {
        if let Some(job) = self.job.take() {
            let _ = self.inner.stop(&job).await;
        }
    }
}

impl Drop for ReplayGuard {
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

/// The newest `slice_s` seconds of a ring whose newest contiguous run starts at
/// `run_start_ns` and ends at `latest_ns`: `(from_ns, held_ns)`. A run shorter than that
/// starts [`RING_MARGIN_NS`] after its oldest sample, which the next block written may
/// evict before the selection is read; `None` when nothing is left.
fn slice_span(run_start_ns: u64, latest_ns: u64, slice_s: f32) -> Option<(u64, u64)> {
    let want_ns = (f64::from(slice_s) * 1e9) as u64;
    let oldest = run_start_ns.saturating_add(RING_MARGIN_NS);
    let from = latest_ns.saturating_sub(want_ns).max(oldest);
    (latest_ns > from).then(|| (from, latest_ns - from))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    /// A ring holding more than the slice gives the newest slice; one holding less gives
    /// what it holds but its oldest margin, which a block written meanwhile may evict.
    #[test]
    fn the_slice_keeps_clear_of_the_oldest_samples() {
        assert_eq!(slice_span(0, 10 * S, 2.0), Some((8 * S, 2 * S)));
        assert_eq!(
            slice_span(9 * S, 10 * S, 2.0),
            Some((9 * S + RING_MARGIN_NS, S - RING_MARGIN_NS))
        );
        // A run of exactly the slice still keeps the margin.
        assert_eq!(
            slice_span(8 * S, 10 * S, 2.0),
            Some((8 * S + RING_MARGIN_NS, 2 * S - RING_MARGIN_NS))
        );
        // A run no longer than the margin leaves nothing.
        assert_eq!(slice_span(10 * S - RING_MARGIN_NS, 10 * S, 2.0), None);
        assert_eq!(slice_span(10 * S, 10 * S, 2.0), None);
    }
}
