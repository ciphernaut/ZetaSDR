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
use crate::port::Tuning;
use crate::types::JobId;

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
        let (capture, tuned) = leases
            .retune_locked(port.as_ref(), id, want, &guard)
            .await
            .map_err(|e| match e {
                LeaseError::Port(p) => NodeError::SourcePort(rec.source.clone(), p),
                e => NodeError::Lease(e),
            })?;
        let bin_rate = self.inner.bank_bin_rate(&rec.source, capture);
        let graph = match kind.reexpand_carrying(
            &rec.source,
            want,
            capture.bandwidth_hz,
            bin_rate,
            &rec.graph,
            &self.inner.modes,
            &RETUNE_CARRIED_PARAMS,
        ) {
            Ok(g) => g,
            Err(e) => {
                // Best effort: the old band back, the job still running on it.
                let back = leases
                    .retune_locked(port.as_ref(), id, rec.tuning, &guard)
                    .await;
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
