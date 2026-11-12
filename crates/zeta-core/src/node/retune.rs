//! `job_retune`: a running job on the radio moved to a new band under the same id. Its
//! lease moves under the source's gate ([`crate::lease::LeaseManager::retune_locked`]),
//! its graph is expanded again for the new band, and its run is replaced by one on that
//! graph ([`Inner::relaunch_in_place`]) without the record leaving `Running` or the
//! lease being let go in between.

use std::sync::Arc;

use tokio::sync::OwnedMutexGuard;

use super::launch::AfterGate;
use super::{Inner, Node, NodeError};
use crate::job::{JobRecord, JobState};
use crate::lease::LeaseError;
use crate::ops::{JobKind, RETUNE_CARRIED_PARAMS};
use crate::port::{PortError, Tuning};
use crate::types::{JobId, SourceId};

impl Node {
    /// Move the running job `id` (of the expandable `kind`) to `want` under its id.
    /// Under one hold of its source's gate: the `replace` jobs are stopped (see
    /// [`Node::replace_holders`]), the job's lease is moved to `want` (inside the
    /// capture untuned, outside it a tune when nobody else holds the source, else a
    /// conflict that puts the old lease back and changes nothing), its graph is
    /// expanded again for `want` at the capture's rate and the job relaunched on it
    /// ([`Inner::relaunch_in_place`]). A graph that cannot be expanded for `want` puts
    /// the old band's lease back (best effort) and is `Expand`, the job left running as
    /// it was. Once the gate is let go, a tune relaunches the elastic jobs on the new
    /// capture.
    ///
    /// `Other("job <id> is not running in this node ...")` for a job that does not
    /// run here (a one-shot node runs only what it started).
    pub async fn job_retune(
        &self,
        id: &JobId,
        kind: &JobKind,
        want: Tuning,
        replace: &[JobId],
    ) -> Result<(), NodeError> {
        let rec = {
            let jobs = self.inner.jobs.lock().unwrap();
            let e = jobs
                .get(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            if e.cancel.is_none() || e.record.state != JobState::Running {
                return Err(NodeError::Other(format!(
                    "job {} is not running in this node (one-shot: use --node against zeta serve)",
                    id.0
                )));
            }
            e.record.clone()
        };
        let port = self
            .port(&rec.source)
            .ok_or_else(|| NodeError::UnknownSource(rec.source.clone()))?;
        let guard = self.inner.leases.gate_guard(&rec.source).await;
        if !replace.is_empty() {
            self.replace_holders(&rec.source, replace, &guard).await?;
        }
        let leases = &self.inner.leases;
        let (capture, tuned) = match leases.retune_locked(port.as_ref(), id, want, &guard).await {
            Ok(r) => r,
            Err(e) => {
                // The old lease is back; but the run may have ended meanwhile (a port
                // error took a while) and released a lease that was set aside then:
                // the restored one must not outlive the job.
                self.inner.release_unless_live(id);
                return Err(match e {
                    LeaseError::Port(p) => NodeError::SourcePort(rec.source.clone(), p),
                    e => NodeError::Lease(e),
                });
            }
        };
        let bin_rate = self.inner.bank_bin_rate(&rec.source, capture);
        let graph = match kind.reexpand_carrying(
            &rec.source,
            want,
            capture.bandwidth_hz,
            bin_rate,
            &rec.graph,
            &self.inner.modes(),
            &RETUNE_CARRIED_PARAMS,
        ) {
            Ok(g) => g,
            Err(e) => {
                // Best effort: the old band back, the job still running on it.
                let back = leases
                    .retune_locked(port.as_ref(), id, rec.tuning, &guard)
                    .await;
                // As above: no lease outlives a run that ended meanwhile.
                self.inner.release_unless_live(id);
                drop(guard);
                if tuned || back.is_ok_and(|(_, t)| t) {
                    let _ = self.inner.relaunch_elastic(&rec.source, Some(id)).await;
                }
                return Err(NodeError::Expand(e));
            }
        };
        let source = rec.source.clone();
        let new = JobRecord {
            graph,
            tuning: want,
            capture_rate_hz: Some(capture.bandwidth_hz.0),
            operator_graph: false,
            ..rec
        };
        // Before the run stops: its attached jobs' supervisors keep them `Running`.
        let held = self.inner.hold_attached(id);
        let (r, after) = self.inner.relaunch_in_place(new, Some(&guard)).await;
        drop(guard);
        self.inner.after_gate(after).await;
        // The job's own launch found its lease inside the new capture and tuned
        // nothing, so nobody else relaunches the elastic jobs.
        if tuned {
            let _ = self.inner.relaunch_elastic(&source, Some(id)).await;
        }
        if r.is_err() {
            // The relaunch failed or was stopped meanwhile: its lease is gone.
            self.idle_if_unheld(port.as_ref()).await;
        }
        // On the input's new stream, or `failed` as `input job <id> not running`.
        self.inner.relaunch_attached(held).await;
        r
    }
}

impl Node {
    /// `source_ensure` with `widen`: move `source`'s capture to `want` though jobs
    /// hold it. Under one hold of the source's gate: the `replace` jobs are stopped
    /// (see [`Node::replace_holders`]), the move is decided and the port tuned
    /// ([`crate::lease::LeaseManager::widen_locked`]: every holder's band inside
    /// `want`, else a conflict and nothing changed), then each job holding the
    /// source is relaunched on the new capture under its id, its lease kept
    /// ([`Inner::relaunch_in_place`]: its graph is expanded again for the new rate,
    /// its channel filters and the bank made for the new centre). A relaunch that
    /// fails ends that job `Failed` with the reason and the others go on. Once the
    /// gate is let go the elastic jobs are relaunched on the new capture and the
    /// attached jobs on their inputs' new streams. A source nobody holds, or a
    /// band inside the capture, is as `source_ensure` without `widen`.
    pub async fn source_widen(
        &self,
        source: &SourceId,
        want: Tuning,
        replace: &[JobId],
    ) -> Result<Tuning, NodeError> {
        self.move_capture(source, Some(want), replace).await
    }

    /// `source_fit`: narrow `source`'s capture to the jobs holding it. Under one hold
    /// of the source's gate the band bounding every rigid lease and the pin is asked
    /// of the port, which grants its narrowest rate that holds it, and each holder is
    /// relaunched on that capture as [`Node::source_widen`] does. A port that grants
    /// the capture it already has restarts nothing. A source no job or pin holds is
    /// `Other("nothing holds source <id>: ...")`.
    pub async fn source_fit(&self, source: &SourceId) -> Result<Tuning, NodeError> {
        self.move_capture(source, None, &[]).await
    }

    /// The body of [`Node::source_widen`] (`want` given) and [`Node::source_fit`]
    /// (`None`: the band bounding the holders, read under the gate).
    async fn move_capture(
        &self,
        source: &SourceId,
        want: Option<Tuning>,
        replace: &[JobId],
    ) -> Result<Tuning, NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        let guard = self.inner.leases.gate_guard(source).await;
        let narrow = want.is_none();
        let want = match want {
            Some(w) => w,
            None => match self.inner.leases.held_band(source, &guard) {
                Some(b) => b,
                None => {
                    return Err(NodeError::Other(format!(
                        "nothing holds source {}: its capture is fitted to a job when one starts",
                        source.0
                    )));
                }
            },
        };
        if !replace.is_empty()
            && let Err(e) = self.replace_holders(source, replace, &guard).await
        {
            drop(guard);
            if !matches!(e, NodeError::UnknownJob(_) | NodeError::Other(_)) {
                self.idle_if_unheld(port.as_ref()).await;
            }
            return Err(e);
        }
        // Before anything moves: a holder whose graph cannot be made for a capture of
        // the width asked would fail at its restart, so the widen is refused instead.
        // (The port may grant a wider rate still; the restart then decides.)
        let outside = port.current().is_none_or(|c| {
            let edges = |t: &Tuning| {
                let half = t.bandwidth_hz.0 / 2;
                (
                    t.center_hz.0.saturating_sub(half),
                    t.center_hz.0.saturating_add(half),
                )
            };
            let ((wl, wh), (cl, ch)) = (edges(&want), edges(&c));
            wl < cl || wh > ch
        });
        if outside && !narrow {
            for (id, _) in self.inner.leases.holders(source) {
                let rec = self
                    .inner
                    .jobs
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map(|e| e.record.clone());
                let Some(rec) = rec.filter(|r| !r.operator_graph) else {
                    continue;
                };
                let Some(kind) = self.inner.expandable_kind(&rec.kind) else {
                    continue;
                };
                if let Err(e) = kind.reexpand(
                    &rec.source,
                    rec.tuning,
                    want.bandwidth_hz,
                    None,
                    &rec.graph,
                    &self.inner.modes(),
                ) {
                    drop(guard);
                    if !replace.is_empty() {
                        self.idle_if_unheld(port.as_ref()).await;
                    }
                    return Err(NodeError::Other(format!(
                        "job {} ({}) cannot run on a {} Hz capture: {e}; nothing was changed",
                        id.0, rec.kind, want.bandwidth_hz.0
                    )));
                }
            }
        }
        let moved = self
            .inner
            .leases
            .widen_locked(port.as_ref(), want, narrow, &guard)
            .await;
        let (got, tuned, holders) = match moved {
            Ok(m) => m,
            Err(e) => {
                drop(guard);
                let e = match e {
                    LeaseError::Port(p) => {
                        self.inner.note_fault_if_faulted(source);
                        NodeError::SourcePort(source.clone(), p)
                    }
                    e => NodeError::Lease(e),
                };
                if !replace.is_empty() {
                    self.idle_if_unheld(port.as_ref()).await;
                }
                return Err(e);
            }
        };
        let mut afters = Vec::new();
        let mut attached = Vec::new();
        let mut failed = false;
        for id in &holders {
            // A holder that is no running job here (a band a starting set holds)
            // has no run to restart.
            let rec = {
                let jobs = self.inner.jobs.lock().unwrap();
                jobs.get(id)
                    .filter(|e| e.cancel.is_some() && e.record.state == JobState::Running)
                    .map(|e| e.record.clone())
            };
            let Some(rec) = rec else {
                continue;
            };
            // Before the run stops: its attached jobs' supervisors keep them `Running`.
            attached.extend(self.inner.hold_attached(id));
            let (r, after) = self.inner.relaunch_in_place(rec, Some(&guard)).await;
            failed |= r.is_err();
            afters.push(after);
        }
        drop(guard);
        for after in afters {
            self.inner.after_gate(after).await;
        }
        if tuned {
            // The holders' launches found their leases inside the new capture and
            // tuned nothing, so nobody else relaunches the elastic jobs.
            let _ = self.inner.relaunch_elastic(source, None).await;
        }
        if failed {
            // A relaunch that failed let its lease go: perhaps the last one.
            self.idle_if_unheld(port.as_ref()).await;
        }
        self.inner.relaunch_attached(attached).await;
        Ok(got)
    }
}

impl Inner {
    /// Replace the live run of the job `rec.id` by a launch of `rec` under the same
    /// id, the record staying `Running` and the lease held throughout: the old run's
    /// supervisor, seeing `relaunch` (see [`super::JobEntry::relaunch`]), releases
    /// nothing but its streams and idles nothing. With `gate` (the source's gate,
    /// held by the caller) the launch leases under it, as [`Inner::launch_gated`]
    /// does, and what must wait for the gate is returned. The old run is awaited
    /// through its `ended` (released and unregistered), never its supervisor's handle,
    /// so a supervisor that goes on to idle the port (a run that ended on its own)
    /// cannot deadlock on the gate held here. A launch that fails releases the lease
    /// and ends the job `Failed` with the reason; the port is not idled here (the
    /// caller idles it once the gate is let go). A job without a live run (stopped
    /// meanwhile) is `Stopped`, its lease released and nothing else done.
    ///
    /// [`Inner::relaunch_elastic`] does the same for the elastic jobs a tune moved.
    pub(super) async fn relaunch_in_place(
        self: &Arc<Self>,
        rec: JobRecord,
        gate: Option<&OwnedMutexGuard<()>>,
    ) -> (Result<(), NodeError>, AfterGate) {
        let id = rec.id.clone();
        let (run, ended) = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(e) = jobs
                .get_mut(&id)
                .filter(|e| e.cancel.is_some() && e.record.state == JobState::Running)
            else {
                // Stopped since the caller looked: no run holds the lease the caller
                // may have moved for it.
                drop(jobs);
                self.leases.release(&id);
                return (Err(NodeError::Stopped(id)), AfterGate::Nothing);
            };
            e.relaunch = true;
            if let Some(cancel) = e.cancel.take() {
                cancel.cancel();
            }
            // Detached, never awaited: see below.
            drop(e.done.take());
            (e.run, e.ended.clone())
        };
        // Wait for the run to release and unregister (its `ended` closes), never for
        // its supervisor to return: a run that ended on its own just before `relaunch`
        // was set goes on to idle the port, which takes the source's gate the caller
        // may hold (as a gated stop waits, see `Inner::stop_with`). Such a run is
        // recorded ended, and `record_running` then refuses the relaunch as `Stopped`.
        if let Some(mut ended) = ended {
            let _ = ended.changed().await;
        }
        let (r, after) = self.launch_gated(rec, gate).await;
        if let Err(e) = &r {
            if let Some(entry) = self.jobs.lock().unwrap().get_mut(&id) {
                entry.relaunch = false;
            }
            self.leases.release(&id);
            self.finish(
                &id,
                run,
                JobState::Failed {
                    reason: e.to_string(),
                },
            );
        }
        (r, after)
    }

    /// Hold the jobs attached to `input` for a relaunch: every running one (state
    /// `Running`, a live run) gets `relaunch` (see [`super::JobEntry::relaunch`]), so
    /// its supervisor, ending when the input's streams unregister, leaves its record
    /// `Running`. Called before the input's run is stopped; the ids, each with the run
    /// held, go to [`Inner::relaunch_attached`] once the input runs again (or failed to).
    pub(super) fn hold_attached(&self, input: &JobId) -> Vec<(JobId, u64)> {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.values_mut()
            .filter(|e| {
                e.record.input.as_ref() == Some(input)
                    && e.record.state == JobState::Running
                    && e.cancel.is_some()
            })
            .map(|e| {
                e.relaunch = true;
                (e.record.id.clone(), e.run)
            })
            .collect()
    }

    /// Relaunch the attached jobs [`Inner::hold_attached`] held, each under its own id
    /// on its input's current audio stream (the launch repoints `stream_in`; it takes
    /// no lease and no gate). Each held run is cancelled (if still live) and awaited
    /// through its `ended`, as [`Inner::relaunch_in_place`] does: an attached run has
    /// no port, so its supervisor has nothing left to do once `ended` closes. Only the
    /// run held is touched: an entry a later run took (another restart of the input
    /// relaunched it meanwhile) is left to that run, never cancelled or ended here. A
    /// job the operator stopped meanwhile stays stopped; a launch that fails (the input
    /// not running) ends the job `Failed` with the reason.
    pub(super) async fn relaunch_attached(self: &Arc<Self>, held: Vec<(JobId, u64)>) {
        for (id, run) in held {
            let ended = {
                let mut jobs = self.jobs.lock().unwrap();
                let Some(e) = jobs.get_mut(&id).filter(|e| e.run == run) else {
                    continue;
                };
                // Taken before the launch, or `record_running` refuses it as
                // `already running a launch`.
                if let Some(cancel) = e.cancel.take() {
                    cancel.cancel();
                }
                drop(e.done.take());
                e.ended.clone()
            };
            if let Some(mut ended) = ended {
                let _ = ended.changed().await;
            }
            let rec = {
                let mut jobs = self.jobs.lock().unwrap();
                let Some(e) = jobs.get_mut(&id).filter(|e| e.run == run) else {
                    continue;
                };
                if e.record.state != JobState::Running {
                    // Stopped by the operator meanwhile: it stays so.
                    e.relaunch = false;
                    continue;
                }
                e.record.clone()
            };
            if let Err(e) = self.launch(rec).await {
                // A launch refused because another relaunch took the id leaves that
                // run's entry alone (`finish` ends only run `run`).
                if let Some(entry) = self
                    .jobs
                    .lock()
                    .unwrap()
                    .get_mut(&id)
                    .filter(|e| e.run == run)
                {
                    entry.relaunch = false;
                }
                self.finish(
                    &id,
                    run,
                    JobState::Failed {
                        reason: e.to_string(),
                    },
                );
            }
        }
    }
}

impl Node {
    /// Re-aim the running scan `id` that its source sweeps for itself at `want` (and
    /// `bin_hz`, when given), under one hold of its source's gate. The run's task asks
    /// the source to re-aim its running sweep ([`super::scan::Reaim`],
    /// [`crate::port::SweepRows::retarget`]); granted, the job keeps its id, stream,
    /// claim and `timing`, its record following the new plan. A source that cannot
    /// re-aim (or a scan still waiting for its sweep) is `Unsupported`: the scan is then
    /// relaunched in place at `want` ([`Inner::relaunch_in_place`]), which ends its
    /// sweep and asks the source for a new one (waiting, never stepping, when the source
    /// refuses: it ran as `source`). Any other refusal is `sweep on source <id>:
    /// <reason>`, the scan running on as it was. The caller has checked the method, the
    /// span against the source's `sweep` and `bin_hz`; a job that does not run here is
    /// as [`Node::job_retune`] says.
    pub(crate) async fn retune_self_sweep(
        &self,
        id: &JobId,
        want: Tuning,
        bin_hz: Option<u64>,
    ) -> Result<(), NodeError> {
        let running = |rec: &JobRecord| {
            NodeError::Other(format!(
                "job {} is not running in this node (one-shot: use --node against zeta serve)",
                rec.id.0
            ))
        };
        let source = {
            let jobs = self.inner.jobs.lock().unwrap();
            let e = jobs
                .get(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            if e.cancel.is_none() || e.record.state != JobState::Running {
                return Err(running(&e.record));
            }
            e.record.source.clone()
        };
        let port = self
            .port(&source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        let guard = self.inner.leases.gate_guard(&source).await;
        // Read again under the gate: the run whose task is asked is the live one.
        let (rec, reaim) = {
            let jobs = self.inner.jobs.lock().unwrap();
            let e = jobs
                .get(id)
                .ok_or_else(|| NodeError::UnknownJob(id.clone()))?;
            if e.cancel.is_none() || e.record.state != JobState::Running {
                return Err(running(&e.record));
            }
            (e.record.clone(), e.reaim.clone())
        };
        let answer = match reaim {
            Some(tx) => {
                let (reply, answer) = tokio::sync::oneshot::channel();
                let asked = super::scan::Reaim {
                    span: want,
                    bin_hz,
                    reply,
                };
                match tx.send(asked).await {
                    // Dropped unanswered: the run is ending; the relaunch below says how.
                    Ok(()) => answer.await.unwrap_or_else(|_| {
                        Err(PortError::Unsupported("the scan's sweep ended".into()))
                    }),
                    Err(_) => Err(PortError::Unsupported("the scan's sweep ended".into())),
                }
            }
            None => Err(PortError::Unsupported(
                "the scan has no sweep to re-aim".into(),
            )),
        };
        match answer {
            Ok(_) => Ok(()),
            Err(PortError::Unsupported(_)) => {
                let mut new = JobRecord {
                    tuning: want,
                    ..rec
                };
                if let Some(b) = bin_hz
                    && let Some(p) = new
                        .graph
                        .nodes
                        .iter_mut()
                        .find(|n| n.kind == "fft")
                        .and_then(|n| n.params.as_object_mut())
                {
                    p.insert("bin_hz".into(), b.into());
                }
                let (r, after) = self.inner.relaunch_in_place(new, Some(&guard)).await;
                drop(guard);
                self.inner.after_gate(after).await;
                if r.is_err() {
                    self.idle_if_unheld(port.as_ref()).await;
                }
                r
            }
            Err(e) => Err(NodeError::Other(format!(
                "sweep on source {}: {}",
                source.0,
                match &e {
                    PortError::Io(m) => m.clone(),
                    e => e.to_string(),
                }
            ))),
        }
    }
}
