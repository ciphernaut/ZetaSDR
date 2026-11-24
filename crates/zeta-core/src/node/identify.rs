//! The `identify` job (design 7.3): a controller that holds a rigid lease on its band,
//! measures it (layer 1), scores the mode rows against it and tries every plausible one
//! as an ordinary child job, one at a time in score order, publishing one `candidate`
//! event per row on its own event stream. A proof leaves that row's child running and
//! ends the job `Stopped` with the verdict in `job_context.identify`; an exhausted list
//! ends it the same way with `mode: null` and the layer-1 label and features, or the
//! learned signature the measurement matches (`learned`). A run a watch starts with
//! `context.identify.candidates` (its fitting `modes`) tries only those rows and
//! publishes no `candidate` event for the rest. `context.identify.dwell_scale`
//! (`job_start`'s `dwell_scale`, or the watch's retry) multiplies each row's dwell.
//!
//! A stop of the job, or any other end of its loop (a fault fails it with the source's
//! fault, as any job on a faulted source), stops the child it is trying (never a proved
//! one: the job has ended by then). Children are always stopped gated: the job's own lease
//! is held meanwhile, so there is nothing to idle, and a `replace` of the job holding the
//! source's gate never waits on a child that waits on that gate.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OwnedMutexGuard;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::launch::{AfterGate, Ending, StageRunning, supervise};
use super::origin::{OriginQuery, described, judged, measured_modulation, origins_of};
use super::persist::now;
use super::{Inner, NodeError, source_fault_reason};
use crate::classify::{Band, Measurement, Outcome};
use crate::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use crate::knowledge::{Allocation, nearest_of};
use crate::learned::{LEARNED_VERDICT_MIN, label_score, learned_match};
use crate::lease::Uncovered;
use crate::modes::Proof;
use crate::modes::score::{allocation_match, allocation_term, passed_over, score_rows};
use crate::modes::{Identify, LoadedMode};
use crate::ops::{IDENTIFY_DWELL_SCALE_MAX, IDENTIFY_MEASURE_MS, JobKind, identify_dwell_ms};
use crate::port::{Port, Tuning};
use crate::stage::StageError;
use crate::stream::{Frame, Payload};
use crate::types::{Hz, JobId, Provenance, Span, StreamId, StreamKind, Timestamp};

/// How long the job waits for the ring to hold [`IDENTIFY_MEASURE_MS`] of its capture
/// before it measures what there is (and fails when there is nothing).
const MEASURE_WAIT: Duration = Duration::from_secs(5);
/// How often the ring and a child's event streams are polled.
const POLL: Duration = Duration::from_millis(25);

/// What a running `identify` job needs.
struct Run {
    id: JobId,
    port: Arc<dyn Port>,
    tuning: Tuning,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
    /// The child a previous run of the job (before a node restart) was trying.
    stale: Option<JobId>,
    /// The controller that owns this run (a `watch`): set on the children it starts,
    /// and handed the verdict (see [`Inner::verdict`]).
    owner: Option<JobId>,
    /// The rows the run may try, when its owner restricted it (a watch's fitting
    /// `modes`, from `context.identify.candidates`); `None` tries every row.
    candidates: Option<Vec<String>>,
    /// What each row's `identify.dwell_ms` is multiplied by ([`identify_dwell_ms`]),
    /// from `context.identify.dwell_scale` (`job_start`'s `dwell_scale`, or a watch's
    /// retry); 1 when absent.
    dwell_scale: u32,
}

/// `q` as the register is asked about a signal a decoder proved: the row's `modulation`
/// in place of the measured label (kept when the row lists none), and the proving
/// `event`'s `bandwidth_hz` in place of the measured width when the event has one.
fn decoded_query(q: &OriginQuery, modulation: &[String], event: &serde_json::Value) -> OriginQuery {
    let mut decoded = q.clone();
    if !modulation.is_empty() {
        decoded.modulation = modulation.to_vec();
    }
    if let Some(width) = event["bandwidth_hz"].as_u64().filter(|w| *w > 0) {
        decoded.bandwidth_hz = Hz(width);
    }
    decoded
}

/// How one attempt ended.
pub(super) enum Attempt {
    /// `event` is the event that completed the proof, as the decoder published it.
    Proved {
        evidence: String,
        summary: String,
        event: serde_json::Value,
    },
    NoFrame(String),
    Cancelled,
}

impl Inner {
    /// Launch an `identify` job: a rigid lease on its band (as a decoder's, under
    /// `gate` when the caller holds it), then [`identify_loop`] under [`supervise`]
    /// with one `Event` output stream. The graph is ignored.
    pub(super) async fn launch_identify(
        self: &Arc<Self>,
        rec: JobRecord,
        port: Arc<dyn Port>,
        gate: Option<&OwnedMutexGuard<()>>,
        after: &mut AfterGate,
    ) -> Result<(), NodeError> {
        let (_, tuned) = match gate {
            Some(g) => {
                self.leases
                    .ensure_locked(port.as_ref(), rec.id.clone(), rec.tuning, false, g)
                    .await?
            }
            None => {
                self.leases
                    .ensure_tuned(port.as_ref(), rec.id.clone(), rec.tuning, false)
                    .await?
            }
        };
        if tuned {
            *after = AfterGate::Relaunch {
                source: rec.source.clone(),
                except: Some(rec.id.clone()),
            };
        }
        let stale = rec.context["identify"]["current"]
            .as_str()
            .map(|s| JobId(s.into()));
        let candidates = rec.context["identify"]["candidates"].as_array().map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        });
        let dwell_scale = rec.context["identify"]["dwell_scale"]
            .as_u64()
            .and_then(|s| u32::try_from(s).ok())
            .filter(|s| (1..=IDENTIFY_DWELL_SCALE_MAX).contains(s))
            .unwrap_or(1);
        let (id, tuning, owner) = (rec.id.clone(), rec.tuning, rec.owner.clone());
        let token = self.shutdown.child_token();
        let (prov, run, ended) = self.record_running(rec, &token)?;
        let out = self.bus.register(StreamKind::Event, prov.clone());
        let r = Run {
            id: id.clone(),
            port: port.clone(),
            tuning,
            prov,
            out: out.clone(),
            cancel: token.clone(),
            stale,
            owner,
            candidates,
            dwell_scale,
        };
        let mut set = JoinSet::new();
        let (running, body) = (
            StageRunning::new(self.stages.clone()),
            identify_loop(self.clone(), r),
        );
        set.spawn(async move {
            let _running = running;
            body.await
        });
        let h = tokio::spawn(supervise(
            self.clone(),
            id.clone(),
            (run, ended),
            Some(port),
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

    /// [`Inner::launch`] of `rec` in its own task. A concrete handle, not an `async fn`,
    /// keeps `launch`'s future type (which runs this job's loop) from containing itself.
    pub(super) fn launch_task(
        self: &Arc<Self>,
        rec: JobRecord,
    ) -> tokio::task::JoinHandle<Result<(), NodeError>> {
        let inner = self.clone();
        tokio::spawn(async move { inner.launch(rec).await })
    }

    /// [`Inner::launch_task`] under the source's gate `gate`, held until the lease is
    /// taken, then dropped before what the launch left for after it runs.
    fn launch_task_gated(
        self: &Arc<Self>,
        rec: JobRecord,
        gate: OwnedMutexGuard<()>,
    ) -> tokio::task::JoinHandle<Result<(), NodeError>> {
        let inner = self.clone();
        tokio::spawn(async move {
            let (r, after) = inner.launch_gated(rec, Some(&gate)).await;
            drop(gate);
            inner.after_gate(after).await;
            r
        })
    }

    /// Stop `stale`, the child a run before a restart was trying, gated. A resume
    /// launches identify before its child (the same band, the older record), so the
    /// child may not be listed yet: wait up to [`MEASURE_WAIT`] for its entry (a record
    /// that never appears has nothing to stop). `false` when the job is cancelled first.
    async fn stop_stale(&self, r: &Run, stale: &JobId) -> bool {
        let deadline = tokio::time::Instant::now() + MEASURE_WAIT;
        while !self.jobs.lock().unwrap().contains_key(stale) {
            if tokio::time::Instant::now() >= deadline {
                return true;
            }
            tokio::select! {
                _ = r.cancel.cancelled() => return false,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
        let _ = self.stop_gated(stale).await;
        true
    }

    /// Record the run's verdict (`{"identify": ...}`) in its context and, for an owned
    /// run, hand it to the owner too: the owned record goes as soon as the run ends. The
    /// verdict is also published as one `verdict` event (`mode`, `label`, `center_hz`,
    /// `job`, `allocations`, `register`, `summary` [`job_summary`]), `center_hz` the
    /// measured centre. A proof (a `mode`) on the band `tuning` (the proved child's)
    /// supersedes the accepted signatures it overlaps ([`Inner::supersede_by`]).
    fn verdict(&self, r: &Run, verdict: serde_json::Value, center_hz: u64, tuning: Tuning) {
        let v = &verdict["identify"]["verdict"];
        let label = v["label"].as_str().unwrap_or("unknown");
        let summary = job_summary(v, center_hz);
        let t = now();
        if let Some(mode) = v["mode"].as_str() {
            self.supersede_by(mode, tuning, t.0);
        }
        let payload = serde_json::json!({
            "protocol": "verdict",
            "mode": v["mode"],
            "label": label,
            "center_hz": center_hz,
            "job": v["job"],
            "allocations": v["allocations"],
            "register": v["register"],
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
        if r.owner.is_some() {
            self.verdicts
                .lock()
                .unwrap()
                .insert(r.id.clone(), verdict["identify"].clone());
        }
        let _ = self.context_merge(&r.id, verdict);
    }

    /// Whether the job's stop came under the source's gate (a `replace`).
    pub(super) fn gated(&self, id: &JobId) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|e| e.gated_stop.is_some())
    }

    /// The last [`IDENTIFY_MEASURE_MS`] of the source's ring at the capture `capture`,
    /// once it holds that much (or, after [`MEASURE_WAIT`], whatever it holds), with
    /// its rate and centre. `Ok(None)` when cancelled.
    async fn measure_samples(
        &self,
        r: &Run,
        capture: Tuning,
    ) -> Result<Option<(Vec<num_complex::Complex<f32>>, Hz, Hz)>, StageError> {
        let want_ns = IDENTIFY_MEASURE_MS * 1_000_000;
        let deadline = tokio::time::Instant::now() + MEASURE_WAIT;
        loop {
            if let Some(f) = r.port.fault() {
                return Err(StageError::Failed(source_fault_reason(r.port.id(), &f)));
            }
            let late = tokio::time::Instant::now() >= deadline;
            let ring = self.rings.lock().unwrap().get(&r.prov.source).cloned();
            if let Some(ring) = ring {
                let ring = ring.read().unwrap();
                if let (Some(center), Some(earliest), Some(latest)) =
                    (ring.center_hz(), ring.latest_run_start(), ring.latest())
                    && center == capture.center_hz
                    && (late || latest.0.saturating_sub(earliest.0) >= want_ns)
                    && latest.0 > earliest.0
                {
                    let start = latest.0.saturating_sub(want_ns).max(earliest.0);
                    let span = Span {
                        start_ns: Timestamp(start),
                        end_ns: latest,
                    };
                    if let Ok(iq) = ring.read(span) {
                        return Ok(Some((iq, ring.sample_rate_hz(), center)));
                    }
                }
            }
            if late {
                return Err(StageError::Failed(format!(
                    "identify: source {} delivered no samples in {} ms",
                    r.port.id().0,
                    MEASURE_WAIT.as_millis()
                )));
            }
            tokio::select! {
                _ = r.cancel.cancelled() => return Ok(None),
                _ = tokio::time::sleep(POLL) => {}
            }
        }
    }

    /// Publish one `candidate` event on the job's stream; `allocation` is the register's
    /// assignment that fits the row best ([`allocation_match`]), if any.
    #[allow(clippy::too_many_arguments)]
    fn candidate(
        &self,
        r: &Run,
        mode: &str,
        score: f32,
        outcome: Outcome,
        evidence: &str,
        job: Option<&JobId>,
        allocation: Option<&Allocation>,
    ) {
        let outcome_name = serde_json::to_value(outcome).expect("outcome serialises");
        let summary = format!("{mode}: {}", outcome_name.as_str().unwrap_or_default());
        let t = now();
        let payload = serde_json::json!({
            "protocol": "candidate",
            "mode": mode,
            "score": score,
            "outcome": outcome_name,
            "evidence": evidence,
            "summary": summary,
            "job": job.map(|j| j.0.clone()),
            "allocation": allocation,
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
    }

    /// Start `mode`'s job on the trier's source at `tuning`, owned by `t.owner`, as
    /// `t.lease` allows: inside the capture identify holds (so granted without a retune),
    /// or, for a trier holding no lease, only under the source's gate and only where the
    /// running capture holds the band untuned
    /// ([`crate::lease::LeaseManager::covers_untuned`]; else [`NotStarted::Uncovered`]),
    /// its lease a trial's, which gives way to a foreground request
    /// ([`crate::lease::LeaseManager::mark_trial`], `t.cancel` its token). The launch
    /// runs in its own task, so a stop of the job never drops it half way: a stop under
    /// the source's gate (which the launch's lease waits on) leaves it to a task that
    /// stops the child once it is up; any other waits for it and stops the child.
    /// `Ok(None)`: cancelled.
    async fn start_child(
        self: &Arc<Self>,
        t: &Trier<'_>,
        mode: &LoadedMode,
        tuning: Tuning,
    ) -> Result<Option<JobId>, NotStarted> {
        let kind = JobKind::from_name(&mode.row.id.0)
            .ok_or_else(|| format!("{} is not a job kind", mode.row.id.0))?;
        let source = t.port.id().clone();
        let capture = t.port.current().unwrap_or(t.band);
        let bin_rate = self.bank_bin_rate(&source, capture);
        // The policy's handler, as a start that names none gets it.
        let modes = self.modes();
        let handler = self
            .plain_handler(&kind, &modes)
            .map_err(|e| e.to_string())?;
        let graph = kind
            .expand_voiced(
                &source,
                tuning,
                capture.bandwidth_hz,
                bin_rate,
                (None, None, None),
                handler.as_deref(),
                &modes,
            )
            .map_err(|e| e.to_string())?;
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
            capture_rate_hz: Some(capture.bandwidth_hz.0),
            elastic: false,
            operator_graph: false,
            owner: t.owner.clone(),
            audio_policy: None,
            stream: None,
            sinks: Vec::new(),
            handler,
            handler_named: false,
            needs: Vec::new(),
            group: None,
            replay: None,
            transient: false,
        };
        let child = rec.id.clone();
        let mut launch = match t.lease {
            TrialLease::Own => {
                // Recorded first, so a run resumed after a restart can stop it (see
                // `stale`).
                let current = serde_json::json!({"identify": {"current": child.0}});
                let _ = self.context_merge(t.id, current);
                self.launch_task(rec)
            }
            TrialLease::Share => {
                let gate = tokio::select! {
                    g = self.leases.gate_guard(&source) => g,
                    _ = t.cancel.cancelled() => return Ok(None),
                };
                if let Err(why) = self.leases.covers_untuned(t.port.as_ref(), tuning, &gate) {
                    return Err(NotStarted::Uncovered(why));
                }
                self.leases.mark_trial(child.clone(), t.cancel.clone());
                self.launch_task_gated(rec, gate)
            }
        };
        let started = tokio::select! {
            res = &mut launch => match res {
                Ok(Ok(())) => Ok(Some(child.clone())),
                Ok(Err(e)) => Err(NotStarted::Failed(e.to_string())),
                Err(e) => Err(NotStarted::Failed(format!("launch task: {e}"))),
            },
            _ = t.cancel.cancelled() => {
                if self.gated(t.id) {
                    let (inner, child) = (self.clone(), child.clone());
                    tokio::spawn(async move {
                        if matches!(launch.await, Ok(Ok(()))) {
                            let _ = inner.stop(&child).await;
                        }
                    });
                } else if matches!(launch.await, Ok(Ok(()))) {
                    self.stop_trial(&t.lease, &child).await;
                }
                Ok(None)
            }
        };
        // A trial mark no lease took (the launch failed or was undone) goes.
        if !matches!(started, Ok(Some(_))) {
            self.leases.unmark_trial(&child);
        }
        started
    }

    /// Stop a trier's child: gated under identify's own lease (there is nothing to
    /// idle, and the stop never waits on the source's gate), plainly for a trier that
    /// holds none.
    pub(super) async fn stop_trial(&self, lease: &TrialLease, child: &JobId) {
        let _ = match lease {
            TrialLease::Own => self.stop_gated(child).await,
            TrialLease::Share => self.stop(child).await,
        };
    }

    /// One row's trial on the live band: `mode`'s job started at `tuning`
    /// ([`Inner::start_child`]; set in `child` once it runs, for the caller's guard to
    /// stop) and its proof awaited on its own events for `dwell_ms`, cut to what is left
    /// before `until` once it runs ([`Inner::await_proof`]). `Err`: it did not start,
    /// and why. `Cancelled` when the trier is, `child` then set only if it started.
    pub(super) async fn trial_row(
        self: &Arc<Self>,
        t: &Trier<'_>,
        mode: &LoadedMode,
        (tuning, dwell_ms, until): (Tuning, u64, Option<tokio::time::Instant>),
        child: &mut Option<JobId>,
    ) -> Result<Attempt, NotStarted> {
        let Some(started) = self.start_child(t, mode, tuning).await? else {
            return Ok(Attempt::Cancelled);
        };
        *child = Some(started.clone());
        // The start took its share of the budget.
        let dwell_ms = until.map_or(dwell_ms, |until| {
            let left = until.saturating_duration_since(tokio::time::Instant::now());
            dwell_ms.min(u64::try_from(left.as_millis()).unwrap_or(u64::MAX))
        });
        Ok(self
            .await_proof(t.cancel, mode, &started, ProofFrom::Own, dwell_ms)
            .await)
    }

    /// Wait up to `dwell_ms` for `mode`'s proof on `child`'s events, read `from` its own
    /// streams or the replay feed: events of the row's `protocol` (with `proof: field`,
    /// those whose `field` is true), at least `min_events` of them. `Cancelled` once
    /// `cancel` is.
    pub(super) async fn await_proof(
        &self,
        cancel: &CancellationToken,
        mode: &LoadedMode,
        child: &JobId,
        from: ProofFrom,
        dwell_ms: u64,
    ) -> Attempt {
        let row = &mode.row;
        let rule = &row.identify;
        let (mut rxs, replay): (Vec<_>, bool) = match from {
            ProofFrom::Own => {
                let jobs = self.jobs.lock().unwrap();
                let rxs = jobs
                    .get(child)
                    .map(|e| e.outputs.clone())
                    .unwrap_or_default()
                    .iter()
                    .filter(|o| o.kind == StreamKind::Event)
                    .filter_map(|o| self.bus.subscribe(&o.id))
                    .map(Some)
                    .collect();
                (rxs, false)
            }
            ProofFrom::Replay(rx) => (vec![Some(rx)], true),
        };
        let had_streams = !rxs.is_empty();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(dwell_ms);
        let need = rule.min_events.max(1);
        let mut seen = 0;
        let state = || {
            self.jobs
                .lock()
                .unwrap()
                .get(child)
                .map(|e| e.record.state.clone())
        };
        loop {
            // A replay has ended once its record has: its last frames are on the feed
            // by then, so the drain below still reads them.
            let ended = replay && state().is_none_or(|s| s != JobState::Running);
            for slot in rxs.iter_mut() {
                let Some(rx) = slot else { continue };
                loop {
                    match rx.try_recv() {
                        Ok(f) => {
                            if replay && f.provenance.job.as_ref() != Some(child) {
                                continue;
                            }
                            let Payload::Event(v) = f.payload else {
                                continue;
                            };
                            if v["protocol"] != row.protocol.as_str() {
                                continue;
                            }
                            let proof = match rule.proof {
                                Proof::Event => true,
                                Proof::None => false,
                                Proof::Field => rule.field.as_deref().is_some_and(|k| {
                                    v.get(k) == Some(&serde_json::Value::Bool(true))
                                }),
                            };
                            if !proof {
                                continue;
                            }
                            seen += 1;
                            if seen >= need {
                                let summary = match v["summary"].as_str() {
                                    Some(s) => s.to_string(),
                                    None => v.to_string(),
                                };
                                return Attempt::Proved {
                                    evidence: format!("decode {}: {summary}", row.id.0),
                                    summary,
                                    event: v,
                                };
                            }
                        }
                        Err(TryRecvError::Lagged(_)) => continue,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Closed) => {
                            *slot = None;
                            break;
                        }
                    }
                }
            }
            if ended || (had_streams && rxs.iter().all(Option::is_none)) {
                return Attempt::NoFrame(match state() {
                    Some(JobState::Failed { reason }) => format!("job failed: {reason}"),
                    _ => "job ended before a proof".into(),
                });
            }
            if tokio::time::Instant::now() >= deadline {
                return Attempt::NoFrame(format!("no {} event in {dwell_ms} ms", row.protocol));
            }
            tokio::select! {
                _ = cancel.cancelled() => return Attempt::Cancelled,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
    }
}

/// The controller a live trial runs for ([`Inner::trial_row`]): an `identify` run, or a
/// `choose` at step 3.
pub(super) struct Trier<'a> {
    /// The controller.
    pub id: &'a JobId,
    /// The source its children run on.
    pub port: &'a Arc<dyn Port>,
    /// The controller's band: taken as the capture when the port reports none.
    pub band: Tuning,
    /// Each child's owner.
    pub owner: Option<JobId>,
    /// The controller's cancellation.
    pub cancel: &'a CancellationToken,
    /// How a child gets the radio.
    pub lease: TrialLease,
}

/// Why a trial's job did not start ([`Inner::start_child`]).
#[derive(Debug)]
pub(super) enum NotStarted {
    /// No running capture holds its band (a trier holding no lease only).
    Uncovered(Uncovered),
    /// The start failed, worded.
    Failed(String),
}

impl From<String> for NotStarted {
    fn from(why: String) -> Self {
        NotStarted::Failed(why)
    }
}

impl std::fmt::Display for NotStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotStarted::Uncovered(why) => write!(f, "no running capture covers it: {why}"),
            NotStarted::Failed(why) => f.write_str(why),
        }
    }
}

/// How a trier's child gets the radio.
pub(super) enum TrialLease {
    /// Inside the capture the trier's own lease holds (identify): the child is recorded
    /// as `identify.current` before it launches, and stopped gated.
    Own,
    /// The trier holds none (a `choose`): the child launches only where a running
    /// capture already holds its band, on a lease that gives way to a foreground
    /// request (the trier's `cancel` fired when it does), and is stopped plainly.
    Share,
}

/// Where [`Inner::await_proof`] reads a child's events.
pub(super) enum ProofFrom {
    /// The child's own event streams, subscribed when the wait starts: a live child
    /// (identify's).
    Own,
    /// The replay feed, subscribed before the child launched so no early frame is
    /// missed: a replay (a `choose`'s trial). Only the child's frames count, and the
    /// child's end is its record leaving `Running`.
    Replay(broadcast::Receiver<Frame>),
}

/// The `identify` job's body: (1) measure [`IDENTIFY_MEASURE_MS`] of the ring over
/// the band with the node's layer-1 classifiers, and ask the knowledge providers once
/// about the measured band ([`described`]): its allocations ([`nearest_of`]), its
/// origins ([`origins_of`]) and the register answer with its verdict ([`judged`]),
/// all carried on the verdict;
/// (2) score the rows ([`score_rows`], the measured centre, the job's own band, the
/// register's [`allocation_term`]; no catalogue term, which lives with the
/// classifiers) and publish `skipped_band` for those wider than the band and
/// `unavailable` for those not ready; (3) try the rest in score order, each as a child
/// job for its `identify.dwell_ms` (times the run's `dwell_scale`), centred on the
/// measured centre when that lies in the band (else on the band's): `no_frame` stops
/// it before the next, `proved` leaves
/// it running and ends the job with the verdict; (4) an exhausted list ends the job
/// with `mode: null`, the layer-1 label and features, `recentred`, `allocations` and
/// `learned`: the accepted signature the measurement matches (`{id, name, confidence}`,
/// the label's confidence; the verdict's label then `learned <name>`) when its match
/// score reaches [`LEARNED_VERDICT_MIN`], else `null`.
async fn identify_loop(inner: Arc<Inner>, r: Run) -> Result<(), StageError> {
    // A child the run before a restart was trying (resumed as an ordinary job): stopped
    // first, needing no samples, so a measurement that fails cannot orphan it.
    if let Some(stale) = &r.stale
        && !inner.stop_stale(&r, stale).await
    {
        return Err(StageError::Cancelled);
    }
    let capture = r.port.current().unwrap_or(r.tuning);
    let Some((iq, rate, center)) = inner.measure_samples(&r, capture).await? else {
        return Err(StageError::Cancelled);
    };
    let band = Band {
        offset_hz: r.tuning.center_hz.0 as i64 - center.0 as i64,
        bandwidth_hz: Hz(r.tuning.bandwidth_hz.0.min(rate.0)),
    };
    let classifiers = inner.classifiers.read().unwrap().clone();
    let measured: Option<Measurement> = classifiers.measure(&iq, rate, center, band).await;
    drop(iq);
    let layer1 = measured
        .as_ref()
        .map_or("unknown", |m| m.label.name.as_str())
        .to_string();
    // The accepted signature the measurement matches best, its score high enough to
    // name it (whatever the record's confidence).
    let learned = match &measured {
        Some(m) => classifiers
            .learned(m)
            .await
            .into_iter()
            .filter(|l| {
                label_score(&inner.signatures, l, m).is_some_and(|s| s >= LEARNED_VERDICT_MIN)
            })
            .max_by(|a, b| a.confidence.total_cmp(&b.confidence)),
        None => None,
    };
    let learned_json = learned.as_ref().and_then(|l| {
        learned_match(l).map(
            |(id, name)| serde_json::json!({"id": id, "name": name, "confidence": l.confidence}),
        )
    });
    let m_center = measured
        .as_ref()
        .map_or(r.tuning.center_hz.0, |m| m.features.center_hz.0);
    let bw = r.tuning.bandwidth_hz.0;
    let (trial, recentred) = trial_tuning(
        r.tuning,
        capture,
        measured
            .as_ref()
            .map(|m| (m.features.center_hz.0, m.label.name.as_str())),
    );
    // One register lookup for the measured band: the allocations, the origins (scored on
    // the measured label; one `origin` event after the lookup, and `origins` in the
    // job's context) and the register answer with its verdict all read it.
    let q = OriginQuery {
        source: r.port.id().clone(),
        center_hz: Hz(m_center),
        bandwidth_hz: measured
            .as_ref()
            .map_or(r.tuning.bandwidth_hz, |m| m.features.bandwidth_hz),
        modulation: measured_modulation(measured.as_ref().map(|m| m.label.name.as_str())),
        symbol_rate_baud: measured.as_ref().and_then(|m| m.features.symbol_rate_baud),
        job: Some(r.id.clone()),
        at: now(),
    };
    let d = tokio::select! {
        d = described(&inner, &q.source, q.center_hz, q.bandwidth_hz) => d,
        _ = r.cancel.cancelled() => return Err(StageError::Cancelled),
    };
    let allocations = nearest_of(d.rows.clone());
    let origins = origins_of(&q, &d);
    let register = judged(&d, &origins);
    // The answer is already in hand for the verdict: a failed merge (the job was removed
    // under the run) only loses the context copy and the event, so the run goes on.
    let _ = inner.keep_origin(&r.id, r.port.id(), &origins);
    let modes = inner.modes();
    let allocation = |m: &LoadedMode| allocation_term(m, &allocations, modes.weights());
    let scored = score_rows(
        modes.weights(),
        &modes,
        m_center,
        bw,
        &layer1,
        &|_| 0.0,
        &allocation,
    );
    let faulted = |f: String| StageError::Failed(source_fault_reason(r.port.id(), &f));
    let mut current = ChildGuard {
        inner: inner.clone(),
        port: r.port.clone(),
        child: None,
    };
    let trier = Trier {
        id: &r.id,
        port: &r.port,
        band: r.tuning,
        owner: r.owner.clone(),
        cancel: &r.cancel,
        lease: TrialLease::Own,
    };
    let mut attempts = 0;
    // One pass in score order: a row that cannot run publishes its candidate at its turn.
    for s in &scored {
        let (mode, score) = (s.mode, s.score);
        // A row the owner left out is not a candidate at all: no event.
        if r.candidates
            .as_ref()
            .is_some_and(|c| !c.contains(&mode.row.id.0))
        {
            continue;
        }
        // A row marked `when_named` is no candidate of an identify that did not name it.
        if passed_over(mode, r.candidates.as_deref()) {
            continue;
        }
        let fit = allocation_match(mode, &allocations);
        if !s.fits_band {
            let min = mode.row.band.min_hz.unwrap_or(mode.row.band.bandwidth_hz);
            let evidence = match s.in_range {
                true => format!("needs a band of at least {min} Hz; the job's is {bw} Hz"),
                false => format!("the measured centre {m_center} Hz is outside the row's range"),
            };
            let id = mode.row.id.0.as_str();
            inner.candidate(&r, id, score, Outcome::SkippedBand, &evidence, None, fit);
            continue;
        }
        if !mode.startable() {
            let evidence = mode.reason.clone().unwrap_or_else(|| "not ready".into());
            let id = mode.row.id.0.as_str();
            inner.candidate(&r, id, score, Outcome::Unavailable, &evidence, None, fit);
            continue;
        }
        if r.cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        if let Some(f) = r.port.fault() {
            return Err(faulted(f));
        }
        let id = mode.row.id.0.as_str();
        let (trial, recentred) = row_trial(&mode.row.identify, r.tuning, trial, recentred);
        let dwell_ms = identify_dwell_ms(mode.row.identify.dwell_ms, r.dwell_scale);
        let attempt = match inner
            .trial_row(&trier, mode, (trial, dwell_ms, None), &mut current.child)
            .await
        {
            Ok(a) => a,
            Err(e) => {
                let e = e.to_string();
                inner.candidate(&r, id, score, Outcome::Unavailable, &e, None, fit);
                continue;
            }
        };
        // Cancelled before the child started: nothing to stop.
        let Some(child) = current.child.clone() else {
            return Err(StageError::Cancelled);
        };
        attempts += 1;
        match attempt {
            Attempt::Cancelled => {
                current.stop().await;
                return Err(StageError::Cancelled);
            }
            Attempt::NoFrame(evidence) => {
                current.stop().await;
                // A child the fault failed is no answer about the mode: the job fails.
                if let Some(f) = r.port.fault() {
                    return Err(faulted(f));
                }
                let job = Some(&child);
                inner.candidate(&r, id, score, Outcome::NoFrame, &evidence, job, fit);
            }
            Attempt::Proved {
                evidence,
                summary,
                event,
            } => {
                // The proved child outlives the job.
                current.child = None;
                let job = Some(&child);
                inner.candidate(&r, id, score, Outcome::Proved, &evidence, job, fit);
                // What the decoder proved replaces what layer 1 measured in the
                // register's comparison (layer 1 has no label for many signals and
                // under-reads a wide flat one): the row's modulation, and the width the
                // proving event names (`bandwidth_hz`) when it names one. The lookup is
                // the one already made, and the `origin` event stays the measured one.
                let decoded = decoded_query(&q, &mode.row.modulation, &event);
                let origins = origins_of(&decoded, &d);
                let register = judged(&d, &origins);
                let verdict = serde_json::json!({"identify": {
                    "verdict": {
                        "mode": id,
                        "label": mode.row.label,
                        "summary": summary,
                        "evidence": evidence,
                        "job": child.0,
                        "recentred": recentred,
                        "allocations": allocations,
                        "learned": null,
                        "origins": origins,
                        "register": register,
                    },
                    "attempts": attempts,
                }});
                inner.verdict(&r, verdict, m_center, trial);
                return Ok(());
            }
        }
    }
    if let Some(f) = r.port.fault() {
        return Err(faulted(f));
    }
    // `learned <name>` when a match is sure enough, else the layer-1 label.
    let label = learned
        .filter(|_| learned_json.is_some())
        .map_or(layer1, |l| l.name);
    let verdict = serde_json::json!({"identify": {
        "verdict": {
            "mode": null,
            "label": label,
            "features": measured.as_ref().map(|m| &m.features),
            "recentred": recentred,
            "allocations": allocations,
            "learned": learned_json,
            "origins": origins,
            "register": register,
        },
        "attempts": attempts,
    }});
    inner.verdict(&r, verdict, m_center, trial);
    Ok(())
}

/// An identify verdict `v`'s one line, as the watch words it, `center_hz` the measured
/// centre: a proof's `<label> at <MHz>: <the decoder's summary>`; a learned match's
/// `learned <name> at <MHz>: <features>`; an unknown's `unknown at <MHz>: <label>
/// <features>` and the register's words ([`super::watch::register_words`]: the register
/// verdict's summary with a register answer, else `; register says ...` from the nearest
/// transmitter of `allocations`, as a verdict kept before the register answer reads).
fn job_summary(v: &serde_json::Value, center_hz: u64) -> String {
    let at = format!("{:.4} MHz", center_hz as f64 / 1e6);
    let label = v["label"].as_str().unwrap_or("unknown");
    match v["mode"].as_str() {
        Some(_) => format!(
            "{label} at {at}: {}",
            v["summary"].as_str().unwrap_or("proved")
        ),
        None if v["learned"].is_object() => {
            format!("{label} at {at}:{}", super::watch::measured(&v["features"]))
        }
        None => format!(
            "unknown at {at}: {label}{}{}",
            super::watch::measured(&v["features"]),
            super::watch::register_words(v),
        ),
    }
}

/// The band each trial job runs on, and whether it is re-centred: the identify band's
/// width on the measured centre (`measured`: the layer-1 centre and label) when the label
/// is not `noise`, the centre lies in `band`, and the shifted band lies inside `capture`
/// (a trial outside the capture the job holds would need a retune its lease forbids);
/// else `band` itself.
fn trial_tuning(band: Tuning, capture: Tuning, measured: Option<(u64, &str)>) -> (Tuning, bool) {
    let half = band.bandwidth_hz.0 / 2;
    let edges = |t: Tuning| {
        let h = t.bandwidth_hz.0 / 2;
        (
            t.center_hz.0.saturating_sub(h),
            t.center_hz.0.saturating_add(h),
        )
    };
    let (cap_lo, cap_hi) = edges(capture);
    let fits = |c: u64| {
        c.abs_diff(band.center_hz.0) <= half
            && c.saturating_sub(half) >= cap_lo
            && c.saturating_add(half) <= cap_hi
    };
    match measured {
        Some((c, label)) if label != "noise" && fits(c) => (
            Tuning {
                center_hz: Hz(c),
                bandwidth_hz: band.bandwidth_hz,
            },
            true,
        ),
        _ => (band, false),
    }
}

/// The band one row's trial runs on, and whether it is re-centred: the run's `trial` band
/// ([`trial_tuning`]), or for a row tried `as_asked` the identify `band` itself.
fn row_trial(rule: &Identify, band: Tuning, trial: Tuning, recentred: bool) -> (Tuning, bool) {
    match rule.as_asked {
        true => (band, false),
        false => (trial, recentred),
    }
}

/// The child the loop is trying. Stopped by [`ChildGuard::stop`], or, on any other exit
/// of the loop with one set (an error, a panic), from a task the drop spawns: a gated
/// stop (its lease goes without waiting on the source's gate), then the port's idle.
/// A proved child is cleared from it first, so it outlives the job.
struct ChildGuard {
    inner: Arc<Inner>,
    port: Arc<dyn Port>,
    child: Option<JobId>,
}

impl ChildGuard {
    /// Stop the child, if any, under the job's own lease: a gated stop, never waiting on
    /// the source's gate (a `replace` of the job may hold it while it waits for the job).
    async fn stop(&mut self) {
        if let Some(child) = self.child.take() {
            let _ = self.inner.stop_gated(&child).await;
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (inner, port) = (self.inner.clone(), self.port.clone());
        rt.spawn(async move {
            let _ = inner.stop_gated(&child).await;
            inner.leases.idle_if_unheld(port.as_ref()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(center_hz: u64, bandwidth_hz: u64) -> Tuning {
        Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
        }
    }

    /// A proof replaces the measured label and width in the register's question: the
    /// row's modulation, and the proving event's `bandwidth_hz` when it names one.
    #[test]
    fn a_proof_asks_the_register_about_what_was_decoded() {
        let measured = OriginQuery {
            source: crate::types::SourceId("radio".into()),
            center_hz: Hz(600_000_000),
            bandwidth_hz: Hz(1_500_000),
            modulation: vec![],
            symbol_rate_baud: None,
            job: None,
            at: Timestamp(0),
        };
        let ofdm = ["ofdm".to_string()];
        let locked = serde_json::json!({"locked": true, "bandwidth_hz": 7_000_000});
        let q = decoded_query(&measured, &ofdm, &locked);
        assert_eq!(
            (q.modulation, q.bandwidth_hz),
            (ofdm.to_vec(), Hz(7_000_000))
        );
        assert_eq!(q.center_hz, measured.center_hz);
        // An event with no width, a null one or zero keeps the measured width; a row
        // with no modulation keeps the measured label.
        for event in [
            serde_json::json!({"id": "S1234567"}),
            serde_json::json!({"bandwidth_hz": null}),
            serde_json::json!({"bandwidth_hz": 0}),
        ] {
            let q = decoded_query(&measured, &ofdm, &event);
            assert_eq!(q.bandwidth_hz, measured.bandwidth_hz, "{event}");
        }
        let fm = OriginQuery {
            modulation: vec!["fm".into()],
            ..measured.clone()
        };
        assert_eq!(decoded_query(&fm, &[], &locked).modulation, ["fm"]);
    }

    /// A row tried `as_asked` runs on the identify band itself, whatever the measurement
    /// moved the other rows' trials to; any other row takes the run's trial.
    #[test]
    fn a_row_tried_as_asked_is_not_recentred() {
        let rule = |as_asked| Identify {
            proof: Proof::Field,
            field: Some("locked".into()),
            min_events: 1,
            dwell_ms: 8_000,
            min_span_ms: 0,
            quick: None,
            priority: 0,
            as_asked,
            when_named: false,
        };
        // The band as asked, and the trial a measurement of the upper half of a wide
        // uneven signal moved it to.
        let (band, trial) = (t(634_500_000, 7_100_000), t(635_690_000, 7_100_000));
        assert_eq!(row_trial(&rule(true), band, trial, true), (band, false));
        assert_eq!(row_trial(&rule(false), band, trial, true), (trial, true));
        assert_eq!(row_trial(&rule(false), band, band, false), (band, false));
    }

    #[test]
    fn an_identify_verdict_kept_before_the_register_answer_still_reads() {
        // An unknown verdict as a node before the register answer kept it: no `register`.
        let old = serde_json::json!({
            "mode": null,
            "label": "fsk",
            "features": {"bandwidth_hz": 12_500},
            "recentred": false,
            "allocations": [{"role": "tx", "licensee": "Example Paging", "distance_km": 12.3}],
            "learned": null,
            "origins": null,
        });
        assert_eq!(
            job_summary(&old, 148_637_500),
            "unknown at 148.6375 MHz: fsk 12.5 kHz; register says Example Paging, 12 km"
        );
        // With a register answer its verdict's summary takes the register's place.
        let mut new = old.clone();
        new["register"] = serde_json::json!({"verdict": {
            "kind": "agrees", "weak": false, "summary": "register agrees: Example Paging"
        }});
        assert_eq!(
            job_summary(&new, 148_637_500),
            "unknown at 148.6375 MHz: fsk 12.5 kHz; register agrees: Example Paging"
        );
        // An unreachable answer with no verdict still says so, never nothing.
        new["register"] = serde_json::json!({"verdict": null, "status": {
            "state": "unreachable", "reason": "timed out after 5000 ms", "failed": []
        }});
        new["allocations"] = serde_json::json!([]);
        assert_eq!(
            job_summary(&new, 148_637_500),
            "unknown at 148.6375 MHz: fsk 12.5 kHz; register unreachable: timed out after 5000 ms"
        );
    }

    #[test]
    fn trials_recentre_only_inside_the_band_and_the_capture() {
        let band = t(144_800_000, 12_500);
        let wide = t(144_800_000, 2_048_000);
        // Inside both: the measured centre, the band's width.
        let got = trial_tuning(band, wide, Some((144_798_000, "fsk")));
        assert_eq!(got, (t(144_798_000, 12_500), true));
        // A measured centre outside the band: the band.
        let got = trial_tuning(band, wide, Some((144_810_000, "fsk")));
        assert_eq!(got, (band, false));
        // Noise, or no measurement: the band.
        let got = trial_tuning(band, wide, Some((144_798_000, "noise")));
        assert_eq!(got, (band, false));
        assert_eq!(trial_tuning(band, wide, None), (band, false));
        // A capture no wider than the band: the shifted band would leave it.
        let got = trial_tuning(band, band, Some((144_798_000, "fsk")));
        assert_eq!(got, (band, false));
        // On the band's own centre the trial fits any capture holding the band.
        let got = trial_tuning(band, band, Some((144_800_000, "fsk")));
        assert_eq!(got, (band, true));
    }
}
