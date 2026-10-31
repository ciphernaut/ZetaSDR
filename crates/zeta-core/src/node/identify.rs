//! The `identify` job (design 7.3): a controller that holds a rigid lease on its band,
//! measures it (layer 1), scores the mode rows against it and tries every plausible one
//! as an ordinary child job, one at a time in score order, publishing one `candidate`
//! event per row on its own event stream. A proof leaves that row's child running and
//! ends the job `Stopped` with the verdict in `job_context.identify`; an exhausted list
//! ends it the same way with `mode: null` and the layer-1 label and features.
//!
//! A stop of the job, or any other end of its loop (a fault fails it with the source's
//! fault, as any job on a faulted source), stops the child it is trying (never a proved
//! one: the job has ended by then). Children are always stopped gated: the job's own lease
//! is held meanwhile, so there is nothing to idle, and a `replace` of the job holding the
//! source's gate never waits on a child that waits on that gate.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OwnedMutexGuard;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::launch::{AfterGate, StageRunning, supervise};
use super::persist::now;
use super::{Inner, NodeError, source_fault_reason};
use crate::classify::{Band, Measurement, Outcome};
use crate::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use crate::modes::Proof;
use crate::modes::score::score_rows;
use crate::modes::{LoadedMode, ModeState};
use crate::ops::{IDENTIFY_MEASURE_MS, JobKind};
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
}

/// How one attempt ended.
enum Attempt {
    Proved { evidence: String, summary: String },
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
            vec![out.clone()],
        ));
        if let Some(e) = self.jobs.lock().unwrap().get_mut(&id)
            && e.run == run
        {
            e.done = Some(h);
            e.outputs = vec![(StreamKind::Event, out)];
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
    /// run, hand it to the owner too: the owned record goes as soon as the run ends.
    fn verdict(&self, r: &Run, verdict: serde_json::Value) {
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
            .is_some_and(|e| e.gated_stop)
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
                    (ring.center_hz(), ring.earliest(), ring.latest())
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

    /// Publish one `candidate` event on the job's stream.
    fn candidate(
        &self,
        r: &Run,
        mode: &str,
        score: f32,
        outcome: Outcome,
        evidence: &str,
        job: Option<&JobId>,
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

    /// Start `mode`'s job on the job's source and band, inside the capture the job
    /// holds (so granted without a retune). The launch runs in its own task, so a
    /// stop of the job never drops it half way: a stop under the source's gate (which
    /// the launch's lease waits on) leaves it to a task that stops the child once it
    /// is up; any other waits for it and stops the child. `Ok(None)`: cancelled.
    async fn start_child(
        self: &Arc<Self>,
        r: &Run,
        mode: &LoadedMode,
    ) -> Result<Option<JobId>, String> {
        let kind = JobKind::from_name(&mode.row.id.0)
            .ok_or_else(|| format!("{} is not a job kind", mode.row.id.0))?;
        let capture = r.port.current().unwrap_or(r.tuning);
        let bin_rate = self.bank_bin_rate(&r.prov.source, capture);
        let graph = kind
            .expand(
                &r.prov.source,
                r.tuning,
                capture.bandwidth_hz,
                bin_rate,
                None,
                None,
                &self.modes,
            )
            .map_err(|e| e.to_string())?;
        let rec = JobRecord {
            format: JOB_RECORD_FORMAT,
            id: JobId::new(),
            kind: kind.name().to_string(),
            graph,
            source: r.prov.source.clone(),
            tuning: r.tuning,
            state: JobState::Running,
            context: serde_json::json!({}),
            created_ns: now(),
            input: None,
            capture_rate_hz: Some(capture.bandwidth_hz.0),
            elastic: false,
            operator_graph: false,
            owner: r.owner.clone(),
        };
        let child = rec.id.clone();
        // Recorded first, so a run resumed after a restart can stop it (see `stale`).
        let _ = self.context_merge(&r.id, serde_json::json!({"identify": {"current": child.0}}));
        let mut launch = self.launch_task(rec);
        tokio::select! {
            res = &mut launch => match res {
                Ok(Ok(())) => Ok(Some(child)),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("launch task: {e}")),
            },
            _ = r.cancel.cancelled() => {
                if self.gated(&r.id) {
                    let inner = self.clone();
                    tokio::spawn(async move {
                        if matches!(launch.await, Ok(Ok(()))) {
                            let _ = inner.stop(&child).await;
                        }
                    });
                } else if matches!(launch.await, Ok(Ok(()))) {
                    // Gated: the job's own lease is still held, so there is nothing to
                    // idle, and the stop never waits on the source's gate.
                    let _ = self.stop_gated(&child).await;
                }
                Ok(None)
            }
        }
    }

    /// Wait up to the row's `dwell_ms` for its proof on `child`'s event streams: events
    /// of the row's `protocol` (with `proof: field`, those whose `field` is true), at
    /// least `min_events` of them.
    async fn await_proof(&self, r: &Run, mode: &LoadedMode, child: &JobId) -> Attempt {
        let row = &mode.row;
        let rule = &row.identify;
        let mut rxs: Vec<_> = {
            let jobs = self.jobs.lock().unwrap();
            jobs.get(child)
                .map(|e| e.outputs.clone())
                .unwrap_or_default()
                .iter()
                .filter(|(k, _)| *k == StreamKind::Event)
                .filter_map(|(_, s)| self.bus.subscribe(s))
                .map(Some)
                .collect()
        };
        let had_streams = !rxs.is_empty();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(rule.dwell_ms);
        let need = rule.min_events.max(1);
        let mut seen = 0;
        loop {
            for slot in rxs.iter_mut() {
                let Some(rx) = slot else { continue };
                loop {
                    match rx.try_recv() {
                        Ok(f) => {
                            let Payload::Event(v) = f.payload else {
                                continue;
                            };
                            if v["protocol"] != row.protocol.as_str() {
                                continue;
                            }
                            let proof = match rule.proof {
                                Proof::Event => true,
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
            if had_streams && rxs.iter().all(Option::is_none) {
                let state = self
                    .jobs
                    .lock()
                    .unwrap()
                    .get(child)
                    .map(|e| e.record.state.clone());
                return Attempt::NoFrame(match state {
                    Some(JobState::Failed { reason }) => format!("job failed: {reason}"),
                    _ => "job ended before a proof".into(),
                });
            }
            if tokio::time::Instant::now() >= deadline {
                return Attempt::NoFrame(format!(
                    "no {} event in {} ms",
                    row.protocol, rule.dwell_ms
                ));
            }
            tokio::select! {
                _ = r.cancel.cancelled() => return Attempt::Cancelled,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
    }
}

/// The `identify` job's body: (1) measure [`IDENTIFY_MEASURE_MS`] of the ring over
/// the band with the node's layer-1 classifiers; (2) score the rows
/// ([`score_rows`], the measured centre, the job's own band; no catalogue term, which
/// lives with the classifiers) and publish `skipped_band` for those wider than the
/// band and `unavailable` for those not ready; (3) try the rest in score order, each
/// as a child job for its `identify.dwell_ms`: `no_frame` stops it before the next,
/// `proved` leaves it running and ends the job with the verdict; (4) an exhausted list
/// ends the job with `mode: null`, the layer-1 label and features.
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
    let m_center = measured
        .as_ref()
        .map_or(r.tuning.center_hz.0, |m| m.features.center_hz.0);
    let bw = r.tuning.bandwidth_hz.0;
    let scored = score_rows(&inner.modes, m_center, bw, &layer1, &|_| 0.0);
    let mut tries = Vec::new();
    for s in &scored {
        let id = s.mode.row.id.0.as_str();
        if !s.fits_band {
            let min = s
                .mode
                .row
                .band
                .min_hz
                .unwrap_or(s.mode.row.band.bandwidth_hz);
            let evidence = format!("needs a band of at least {min} Hz; the job's is {bw} Hz");
            inner.candidate(&r, id, s.score, Outcome::SkippedBand, &evidence, None);
        } else if s.mode.state != ModeState::Ready {
            let evidence = s.mode.reason.clone().unwrap_or_else(|| "not ready".into());
            inner.candidate(&r, id, s.score, Outcome::Unavailable, &evidence, None);
        } else {
            tries.push((s.mode, s.score));
        }
    }
    let faulted = |f: String| StageError::Failed(source_fault_reason(r.port.id(), &f));
    let mut current = ChildGuard {
        inner: inner.clone(),
        port: r.port.clone(),
        child: None,
    };
    let mut attempts = 0;
    for (mode, score) in tries {
        if r.cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        if let Some(f) = r.port.fault() {
            return Err(faulted(f));
        }
        let id = mode.row.id.0.as_str();
        let child = match inner.start_child(&r, mode).await {
            Ok(Some(c)) => c,
            Ok(None) => return Err(StageError::Cancelled),
            Err(e) => {
                inner.candidate(&r, id, score, Outcome::Unavailable, &e, None);
                continue;
            }
        };
        attempts += 1;
        current.child = Some(child.clone());
        match inner.await_proof(&r, mode, &child).await {
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
                inner.candidate(&r, id, score, Outcome::NoFrame, &evidence, Some(&child));
            }
            Attempt::Proved { evidence, summary } => {
                // The proved child outlives the job.
                current.child = None;
                inner.candidate(&r, id, score, Outcome::Proved, &evidence, Some(&child));
                let verdict = serde_json::json!({"identify": {
                    "verdict": {
                        "mode": id,
                        "label": mode.row.label,
                        "summary": summary,
                        "evidence": evidence,
                        "job": child.0,
                    },
                    "attempts": attempts,
                }});
                inner.verdict(&r, verdict);
                return Ok(());
            }
        }
    }
    if let Some(f) = r.port.fault() {
        return Err(faulted(f));
    }
    let verdict = serde_json::json!({"identify": {
        "verdict": {
            "mode": null,
            "label": layer1,
            "features": measured.as_ref().map(|m| &m.features),
        },
        "attempts": attempts,
    }});
    inner.verdict(&r, verdict);
    Ok(())
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
