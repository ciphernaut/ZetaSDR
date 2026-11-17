//! The scan a source sweeps for itself (spec 4.7), the half of [`super::scan`]
//! that is not the stepped loop: the launch, the start under the source's gate,
//! the wait for a sweep the source refused and the clock of its rows. The
//! source's rows are forwarded in the scan row shape ([`sweep_loop`]), under a
//! sweep claim and no lease (`method: source`).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedMutexGuard, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::launch::{StageRunning, supervise};
use super::scan::{Look, SweepTiming, TIMING_EVERY_NS, dwell_rows_per_s, refusal_reason};
use super::{Inner, JobOutput, NodeError};
use crate::job::JobRecord;
use crate::ops::{
    SWEEP_RETRY_MS, ScanMethod, scan_average_budget_bytes, scan_average_in_force,
    scan_sweep_refusal,
};
use crate::port::{
    Port, PortError, SWEEP_NO_RETARGET, SweepPlan, SweepRequest, SweepRow, SweepRows, SweepState,
    Tuning,
};
use crate::spectrum::{STEP_DB, SweepAverage, quantise_row_masked};
use crate::stage::StageError;
use crate::stream::{Frame, Payload};
use crate::types::{ChannelId, Hz, JobId, Provenance, SourceId, StreamId, StreamKind};

impl Inner {
    /// Whether a scan other than `id` holds a sweep claim on `source`.
    pub(super) fn swept_by_another(&self, source: &SourceId, id: &JobId) -> bool {
        self.leases.sweepers(source).iter().any(|j| j != id)
    }

    /// Take job `id`'s sweep claim on `port` under its gate (`gate`, held), then ask
    /// the port to sweep `req`: its rows, or its refusal, the claim then dropped
    /// unless a live run of the id holds it. Taken before the call and under the
    /// gate, the claim is never taken after the job's release, and an idle never
    /// lands between it and the sweep's start. A call that has not returned after
    /// [`crate::ops::SWEEP_START_TIMEOUT_MS`] is dropped (ending the source's attempt) and
    /// read as a refusal, `sweep start timed out`: the gate is never held longer.
    pub(super) async fn start_sweep(
        &self,
        port: &dyn Port,
        id: &JobId,
        req: SweepRequest,
        gate: &OwnedMutexGuard<()>,
    ) -> Result<SweepRows, PortError> {
        self.leases.claim_sweep_locked(port.id(), id.clone(), gate);
        let wait = *self.sweep_start_timeout.lock().unwrap();
        let rows = match tokio::time::timeout(wait, port.sweep(req)).await {
            Ok(rows) => rows,
            Err(_) => Err(PortError::Io("sweep start timed out".into())),
        };
        if rows.is_err() {
            self.release_unless_live(id);
        }
        rows
    }

    /// Run a scan the source sweeps for itself: record it running with
    /// [`sweep_task`] as its one task under [`supervise`] (handed the port, so its
    /// claim is released and the port idled at the end), `blind` (it reads no
    /// fault, so a heal does not wait for it), with `job_context.scan` from the
    /// source's plan ([`sweep_context`]), or, `start` waiting (a restart the
    /// source refused, the claim already taken), [`waiting_context`] until a
    /// sweep asked again as `req` is granted. It holds no lease and stops nobody.
    /// The run's entry keeps the sender of its [`Reaim`]s (`job_retune`).
    pub(super) fn launch_self_sweep(
        self: &Arc<Self>,
        mut rec: JobRecord,
        port: Arc<dyn Port>,
        look: Look,
        calibrated: bool,
        start: SweepStart,
        req: SweepRequest,
    ) -> Result<(), NodeError> {
        let budget = scan_average_budget_bytes(self.ring_budget_bytes);
        if !rec.context.is_object() {
            rec.context = serde_json::json!({});
        }
        if let Some(ctx) = rec.context.as_object_mut() {
            let scan = match &start {
                SweepStart::Rows(rows) => {
                    let average = sweep_average(look, budget, &rows.plan);
                    sweep_context(&rows.plan, look, average, calibrated)
                }
                SweepStart::Waiting(why) => waiting_context(calibrated, why),
            };
            ctx.insert("scan".into(), scan.into());
        }
        let (id, span) = (rec.id.clone(), rec.tuning);
        let token = self.shutdown.child_token();
        let (prov, run, ended) = self.record_running(rec, &token)?;
        let out = self.bus.register_unfed(StreamKind::Event, prov.clone());
        let sweep = SelfSweep {
            id: id.clone(),
            span,
            look,
            budget,
            calibrated,
            port: port.clone(),
            req,
            prov,
            out: out.clone(),
            cancel: token.clone(),
        };
        let (reaim_tx, reaim) = mpsc::channel(1);
        let mut set = JoinSet::new();
        let (running, task) = (
            StageRunning::new(self.stages.clone()),
            sweep_task(self.clone(), sweep, start, reaim),
        );
        set.spawn(async move {
            let _running = running;
            task.await
        });
        let h = tokio::spawn(supervise(
            self.clone(),
            id.clone(),
            (run, ended),
            Some(port),
            set,
            token,
            (vec![out.clone()], None, Vec::new()),
        ));
        if let Some(e) = self.jobs.lock().unwrap().get_mut(&id)
            && e.run == run
        {
            e.done = Some(h);
            e.outputs = vec![JobOutput::new(StreamKind::Event, out)];
            e.blind = true;
            e.reaim = Some(reaim_tx);
        }
        Ok(())
    }
}

/// What a self-sweep scan needs beside its rows (which carry the plan the source
/// chose): its id, its band, its look and the averaging budget (the `average` in
/// force follows from the plan, [`sweep_average`]), whether the source's levels
/// are `calibrated`, its port and the sweep it asks for (asked again while it
/// waits, [`wait_for_sweep`]), and its output stream.
struct SelfSweep {
    id: JobId,
    span: Tuning,
    look: Look,
    budget: u64,
    calibrated: bool,
    port: Arc<dyn Port>,
    req: SweepRequest,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
}

/// A `job_retune` of a self-sweep scan, handed to its task: the new `span`, the
/// `bin_hz` asked (`None`: the scan's own) and where the task answers, with the
/// source's new plan or why not (`Unsupported`: the source cannot re-aim, or the
/// scan has no sweep running, so the caller restarts it instead).
pub(super) struct Reaim {
    pub span: Tuning,
    pub bin_hz: Option<u64>,
    pub reply: oneshot::Sender<Result<SweepPlan, PortError>>,
}

/// Why a scan waiting for its sweep cannot be re-aimed: it has none to re-aim.
const REAIM_WAITING: &str = "the scan is waiting for its sweep";

/// Answer a re-aim while the scan has no sweep running: `Unsupported`.
fn refuse_reaim(r: Reaim) {
    let _ = r
        .reply
        .send(Err(PortError::Unsupported(REAIM_WAITING.into())));
}

/// Whether the source's `plan` alternates its odd sweeps: a shift and more than
/// one step (a plan of one view holds still, whatever shift it says).
fn plan_alternates(plan: &SweepPlan) -> bool {
    plan.shift_hz.0 != 0 && plan.steps > 1
}

/// How a self-sweep scan starts: with the source's rows, or `Waiting` (the reason)
/// for a restart the source refused, asking again ([`wait_for_sweep`]).
pub(super) enum SweepStart {
    Rows(SweepRows),
    Waiting(String),
}

/// The `average` in force for `look` on the source's `plan` with `budget_bytes` for
/// the averages: one row a step, the plan's steps (twice when it alternates,
/// [`plan_alternates`]) and its bins ([`scan_average_in_force`]).
fn sweep_average(look: Look, budget_bytes: u64, plan: &SweepPlan) -> u32 {
    let grids = if plan_alternates(plan) { 2 } else { 1 };
    scan_average_in_force(
        look.average,
        budget_bytes,
        u64::from(plan.steps) * grids,
        1,
        u64::from(plan.size),
    )
}

/// A self-sweep scan's `job_context.scan` while it waits for its source to grant a
/// sweep: `method` (`source`), `mask` (none), `calibrated` and `waiting` (`why`).
/// No `steps` or other key of a plan until one arrives ([`sweep_context`]).
fn waiting_context(calibrated: bool, why: &str) -> serde_json::Map<String, serde_json::Value> {
    let scan = serde_json::json!({
        "method": ScanMethod::Source.name(),
        "mask": {"notch_hz": 0, "edge_mask": 0.0},
        "calibrated": calibrated,
        "waiting": why,
    });
    match scan {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("built as an object"),
    }
}

/// The self-sweep scan's one task: [`sweep_loop`] over the rows `start` holds, or,
/// waiting, over the rows of the sweep [`wait_for_sweep`] is granted; `reaim` brings
/// its `job_retune`s.
async fn sweep_task(
    inner: Arc<Inner>,
    s: SelfSweep,
    start: SweepStart,
    mut reaim: mpsc::Receiver<Reaim>,
) -> Result<(), StageError> {
    let rows = match start {
        SweepStart::Rows(rows) => rows,
        SweepStart::Waiting(why) => match wait_for_sweep(&inner, &s, why, &mut reaim).await {
            Some(rows) => rows,
            None => return Err(StageError::Cancelled),
        },
    };
    sweep_loop(inner, s, rows, reaim).await
}

/// Ask the source again for the sweep it refused (`waiting`, the reason), after each
/// of [`SWEEP_RETRY_MS`] in turn, the last repeating, until it is granted (`None`
/// once cancelled): each ask under the source's gate ([`Inner::start_sweep`], the
/// scan's claim kept through a refusal), skipped with the reason while the source's
/// `sweep` does not hold the span. A new reason is written as `waiting`; granted,
/// the plan's keys ([`sweep_context`]) with `waiting: null`. It never steps. A
/// re-aim meanwhile is refused `Unsupported` ([`refuse_reaim`]): there is no sweep to
/// re-aim, so `job_retune` restarts the scan at the new span instead.
async fn wait_for_sweep(
    inner: &Inner,
    s: &SelfSweep,
    mut waiting: String,
    reaim: &mut mpsc::Receiver<Reaim>,
) -> Option<SweepRows> {
    let mut k = 0usize;
    loop {
        let ms = SWEEP_RETRY_MS[k.min(SWEEP_RETRY_MS.len() - 1)];
        k += 1;
        let wake = tokio::time::Instant::now() + Duration::from_millis(ms);
        loop {
            tokio::select! {
                _ = s.cancel.cancelled() => return None,
                Some(r) = reaim.recv() => refuse_reaim(r),
                _ = tokio::time::sleep_until(wake) => break,
            }
        }
        let asked = match scan_sweep_refusal(s.port.id(), s.port.capabilities(), s.span) {
            Some(why) => Err(why),
            None => {
                // A `job_retune` holding the gate waits for its re-aim's answer.
                let gate = loop {
                    tokio::select! {
                        _ = s.cancel.cancelled() => return None,
                        Some(r) = reaim.recv() => refuse_reaim(r),
                        g = inner.leases.gate_guard(s.port.id()) => break g,
                    }
                };
                tokio::select! {
                    _ = s.cancel.cancelled() => return None,
                    r = inner.start_sweep(s.port.as_ref(), &s.id, s.req, &gate) => {
                        r.map_err(|e| refusal_reason(&e))
                    }
                }
            }
        };
        match asked {
            Ok(rows) => {
                let average = sweep_average(s.look, s.budget, &rows.plan);
                let fields = sweep_context(&rows.plan, s.look, average, s.calibrated);
                inner.scan_context_update(&s.id, |scan| scan.extend(fields));
                return Some(rows);
            }
            Err(why) if why != waiting => {
                waiting = why.clone();
                inner.scan_context_update(&s.id, |scan| {
                    scan.insert("waiting".into(), why.into());
                });
            }
            Err(_) => {}
        }
    }
}

/// A self-sweep scan's `job_context.scan` at its start: `method` (`source`), the
/// source's `plan` (`steps`, `step_hz`, `bin_hz`, `dwell_ms`, the dwell in force,
/// with `dwell_ms_asked` when `look`'s differs), `average` (in force) with
/// `average_asked` when that differs, `alternate` (the odd sweeps' shift, or
/// null: none, or a plan of one step, [`plan_alternates`]), `mask` (none: the
/// source masks its own rows), `calibrated`, and `waiting` (null). No `rate_hz`
/// and no `bits`: the scan takes no IQ.
fn sweep_context(
    plan: &SweepPlan,
    look: Look,
    average: u32,
    calibrated: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let alternate = match plan_alternates(plan) {
        false => serde_json::Value::Null,
        true => serde_json::json!({"shift_hz": plan.shift_hz.0, "steps_shifted": plan.steps}),
    };
    let mut scan = serde_json::json!({
        "method": "source",
        "steps": plan.steps,
        "step_hz": plan.step_hz.0,
        "bin_hz": plan.bin_hz,
        "dwell_ms": plan.dwell_ms,
        "average": average,
        "alternate": alternate,
        "mask": {"notch_hz": 0, "edge_mask": 0.0},
        "calibrated": calibrated,
        "waiting": null,
    });
    if plan.dwell_ms != look.dwell_ms {
        scan["dwell_ms_asked"] = look.dwell_ms.into();
    }
    if average != look.average {
        scan["average_asked"] = look.average.into();
    }
    match scan {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("built as an object"),
    }
}

/// A self-sweep's [`SweepTiming`] as its rows arrive: a row is a step of no tune
/// that took the time since the last row; when a row of the next sweep arrives,
/// each step the last sweep sent no row for is a skip, and the sweep (from the
/// last row of the one before it, or the loop's start, to its own last row) is
/// written as `job_context.scan.timing` at most once a second, else carried. The
/// write also says `rows_per_s`: the rows that arrived since the last write over
/// the time since it (since the clock began, for the first). A re-aim
/// ([`SweepClock::reaim`]) starts the sweep and the figures afresh on the new plan
/// but keeps when the timing was last written and the row count since, so a
/// re-aim never writes sooner than a second after the last write.
struct SweepClock {
    timing: SweepTiming,
    /// When the timing was last written (ns since the epoch).
    written_ns: u64,
    last_row: tokio::time::Instant,
    /// The sweep under way: its number, when it began and the steps it sent.
    sweep: Option<(u64, tokio::time::Instant, Vec<bool>)>,
    steps: usize,
    /// Rows since the last write, and when that was (the clock's start, before one).
    rows: u64,
    since: tokio::time::Instant,
}

impl SweepClock {
    fn new(steps: u32) -> Self {
        let now = tokio::time::Instant::now();
        SweepClock {
            timing: SweepTiming::default(),
            written_ns: 0,
            last_row: now,
            sweep: None,
            steps: usize::try_from(steps).unwrap_or(usize::MAX),
            rows: 0,
            since: now,
        }
    }

    /// The sweep re-aimed at a plan of `steps` steps: the sweep under way and the
    /// figures carried are dropped (they are the old plan's), the next sweep's
    /// time counts from now; when the timing was last written and the rows
    /// counted since are kept.
    fn reaim(&mut self, steps: u32) {
        *self = SweepClock {
            written_ns: self.written_ns,
            rows: self.rows,
            since: self.since,
            ..SweepClock::new(steps)
        };
    }

    /// Count a row of step `step` of sweep `sweep`, closing the sweep before it.
    fn row(&mut self, inner: &Inner, s: &SelfSweep, sweep: u64, step: u32) {
        let now = tokio::time::Instant::now();
        if let Some((_, start, sent)) = self.sweep.take_if(|(n, ..)| *n != sweep) {
            for _ in sent.iter().filter(|sent| !**sent) {
                self.timing.step(Duration::ZERO, Duration::ZERO, true);
            }
            let at_ns = super::persist::now().0;
            if at_ns.saturating_sub(self.written_ns) >= TIMING_EVERY_NS {
                let took = self.last_row - start;
                let mut t = self.timing.finish(s.span.bandwidth_hz.0, took);
                t["at_ns"] = at_ns.into();
                let secs = (now - self.since).as_secs_f64();
                let rows_per_s = if secs > 0.0 {
                    self.rows as f64 / secs
                } else {
                    0.0
                };
                t["rows_per_s"] = ((rows_per_s * 1000.0).round() / 1000.0).into();
                (self.rows, self.since) = (0, now);
                self.written_ns = at_ns;
                inner.scan_context_update(&s.id, |scan| {
                    scan.insert("timing".into(), t);
                });
            } else {
                self.timing.carry();
            }
        }
        let (steps, last_row) = (self.steps, self.last_row);
        let (_, _, sent) = self
            .sweep
            .get_or_insert_with(|| (sweep, last_row, vec![false; steps]));
        if let Some(sent) = usize::try_from(step).ok().and_then(|i| sent.get_mut(i)) {
            *sent = true;
        }
        self.timing.step(Duration::ZERO, now - self.last_row, false);
        self.last_row = now;
        self.rows += 1;
    }
}

/// A source's row as a scan row: `size`, `bin_hz`, `rows_per_s`, `center_hz`,
/// `floor_db` and `q` by [`quantise_row_masked`] over the row's `masked` (with
/// `avg`, an `average` in force over 1, from the mean of the step's last rows
/// instead), `step_db`, `masked` (when not empty), `sweeps` and, with
/// `calibrated`, `calibrated: true`.
fn sweep_row_event(
    row: &SweepRow,
    rows_per_s: u32,
    calibrated: bool,
    avg: Option<&mut SweepAverage>,
) -> serde_json::Value {
    let (mut floor_db, mut q) = quantise_row_masked(&row.db, &row.masked);
    let mut sweeps = 1;
    if let Some(avg) = avg {
        let (db, n) = avg.push((row.center_hz.0, 0), floor_db, &q);
        (floor_db, q) = quantise_row_masked(&db, &row.masked);
        sweeps = n;
    }
    let mut ev = serde_json::json!({
        "size": row.db.len(),
        "bin_hz": row.bin_hz,
        "rows_per_s": rows_per_s,
        "center_hz": row.center_hz.0,
        "floor_db": floor_db,
        "step_db": STEP_DB,
        "q": q,
        "sweeps": sweeps,
    });
    if !row.masked.is_empty() {
        ev["masked"] = serde_json::json!(row.masked);
    }
    if calibrated {
        ev["calibrated"] = true.into();
    }
    ev
}

/// The self-sweep scan's one task, until cancelled: forward each of the source's
/// rows on the scan's stream as a scan row ([`sweep_row_event`]), channel
/// `scan/<step>`, its span the row's, with `rows_per_s` the dwell's
/// ([`dwell_rows_per_s`]); with an `average` in force over 1 it keeps one
/// [`SweepAverage`] per grid, the odd sweeps apart when the plan shifts them, as
/// [`fft_task`] does. The source's state is written as `job_context.scan.waiting`
/// (the reason while it retries, else null) and each sweep's timing as `timing`
/// ([`SweepClock`]). The rows closing ends the job, failed as `sweep on source
/// <id>: <the reason>` (never `source <id>: …`: the source's IQ side is not at
/// fault). It never reads [`Port::fault`]; cancelled, it drops the rows first,
/// which ends the source's sweep.
///
/// A [`Reaim`] is asked of the source ([`SweepRows::retarget`], at the scan's
/// `bin_hz` unless it names one and the scan's dwell), bounded as a sweep's start
/// is: granted, the rows still queued (the old plan's) are dropped, the averages
/// and the sweep clock start afresh on the new plan, and the record's tuning and
/// `job_context.scan` follow it ([`Inner::scan_reaimed`]); refused, nothing
/// changes. A source with no control, or one that has not answered in time, is
/// `Unsupported`: the caller restarts the scan, which ends this sweep whatever the
/// source does with the re-aim later.
async fn sweep_loop(
    inner: Arc<Inner>,
    mut s: SelfSweep,
    rows: SweepRows,
    mut reaim: mpsc::Receiver<Reaim>,
) -> Result<(), StageError> {
    let SweepRows {
        plan,
        rows: mut rx,
        mut state,
        control,
    } = rows;
    let mut rows_per_s = dwell_rows_per_s(plan.dwell_ms);
    let mut alternates = plan_alternates(&plan);
    let mut average = sweep_average(s.look, s.budget, &plan);
    let mut averages = [0, 1].map(|_| SweepAverage::new(average));
    let mut clock = SweepClock::new(plan.steps);
    let mut waiting = None;
    // A state the source set before the loop began is written too.
    state.mark_changed();
    let mut state_open = true;
    loop {
        tokio::select! {
            biased;
            _ = s.cancel.cancelled() => {
                drop(rx);
                return Err(StageError::Cancelled);
            }
            Some(r) = reaim.recv() => {
                let req = SweepRequest {
                    span: r.span,
                    bin_hz: r.bin_hz.map_or(s.req.bin_hz, Hz),
                    dwell_ms: s.req.dwell_ms,
                };
                let answer = match &control {
                    None => Err(PortError::Unsupported(SWEEP_NO_RETARGET.into())),
                    Some(c) => {
                        let wait = *inner.sweep_start_timeout.lock().unwrap();
                        tokio::time::timeout(wait, c.retarget(req))
                            .await
                            .unwrap_or_else(|_| {
                                Err(PortError::Unsupported("sweep re-aim timed out".into()))
                            })
                    }
                };
                if let Ok(plan) = &answer {
                    // Every row queued before the answer is the old plan's.
                    while rx.try_recv().is_ok() {}
                    (s.span, s.req) = (r.span, req);
                    rows_per_s = dwell_rows_per_s(plan.dwell_ms);
                    alternates = plan_alternates(plan);
                    average = sweep_average(s.look, s.budget, plan);
                    averages = [0, 1].map(|_| SweepAverage::new(average));
                    clock.reaim(plan.steps);
                    let fields = sweep_context(plan, s.look, average, s.calibrated);
                    inner.scan_reaimed(&s.id, r.span, r.bin_hz, fields);
                    // The fields say `waiting: null`; the source's state is read again.
                    waiting = None;
                    state.mark_changed();
                }
                let _ = r.reply.send(answer);
            }
            r = rx.recv() => {
                let Some(row) = r else {
                    let why = match &*state.borrow() {
                        SweepState::Ended(r) => r.clone(),
                        _ => "ended".to_string(),
                    };
                    return Err(StageError::Failed(format!(
                        "sweep on source {}: {why}",
                        s.prov.source.0
                    )));
                };
                clock.row(&inner, &s, row.sweep, row.step);
                let grid = usize::from(alternates && row.sweep % 2 == 1);
                let avg = (average > 1).then(|| &mut averages[grid]);
                let ev = sweep_row_event(&row, rows_per_s, s.calibrated, avg);
                let prov = Provenance {
                    channel: Some(ChannelId(format!("scan/{}", row.step))),
                    span: row.span,
                    ..s.prov.clone()
                };
                inner
                    .bus
                    .publish(Frame::new(s.out.clone(), prov, Payload::Event(ev)));
            }
            changed = state.changed(), if state_open => {
                if changed.is_err() {
                    // The source is done with the state; the rows closing says the rest.
                    state_open = false;
                    continue;
                }
                let now = match &*state.borrow_and_update() {
                    SweepState::Waiting(r) => Some(r.clone()),
                    SweepState::Running | SweepState::Ended(_) => None,
                };
                if now != waiting {
                    waiting = now.clone();
                    inner.scan_context_update(&s.id, |scan| {
                        scan.insert("waiting".into(), now.into());
                    });
                }
            }
        }
    }
}
