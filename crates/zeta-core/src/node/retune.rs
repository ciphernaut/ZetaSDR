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
    /// A job of a Send to group with another member running is moved with the whole
    /// group ([`Node::group_retune`]).
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
        if let Some((group, source)) = self.inner.retuned_group(id) {
            return self.group_retune(id, &group, &source, want, replace).await;
        }
        // The restart and its attached jobs' relaunch under the job's restart lock,
        // taken before the gate: another restart of it waits its turn.
        let _restart = self.inner.restart_guard(id).await;
        let mut rec = {
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
        // The handler the relaunch runs, decided as a restart decides it, before anything
        // is stopped or moved: a named one no longer ready refuses the retune (the job
        // runs on as it was), a chosen one is chosen again and the graph below is its.
        self.inner.rehandle(&mut rec)?;
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
        let graph = kind
            .reexpand_carrying(
                &rec.source,
                want,
                capture.bandwidth_hz,
                bin_rate,
                &rec.graph,
                (rec.audio_policy, rec.handler.as_deref()),
                &self.inner.modes(),
                &RETUNE_CARRIED_PARAMS,
            )
            .map_err(NodeError::Expand)
            // A watch's decoder keeps its feed (`JobRecord::sinks`).
            .and_then(|g| self.inner.sinked(g, &rec.sinks));
        let graph = match graph {
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
                return Err(e);
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

    /// [`Node::job_retune`] of `id`, a member of the Send to group `group` on `source`:
    /// the whole group moves as one lease move. The restart locks of exactly the members
    /// running once the gate is held, then the gate (other holders of the source are not
    /// locked); each member's handler settled, oldest first, as a restart settles it
    /// ([`Inner::rehandle`]: a handler another member runs is passed over), before
    /// anything is stopped or moved (a member that cannot be settled refuses the whole
    /// retune, naming it, nothing moved); the `replace` jobs stopped (a member among
    /// them is refused); every member's lease moved to `want` together
    /// ([`crate::lease::LeaseManager::retune_group_locked`]: one tune at most, a
    /// conflict naming only holders outside the group, every lease put back); then each
    /// member expanded again and relaunched in place under the one gate, `id` first and
    /// the others oldest first. A member whose expansion or relaunch fails is `Failed`
    /// with the reason and the others stand; it is not recovered on its own. Once the
    /// gate is let go a tune relaunches the elastic jobs once, and every member's
    /// attached jobs are relaunched on it. The result is `id`'s relaunch's. A member
    /// stopped by the time the gate is taken is left out.
    async fn group_retune(
        &self,
        id: &JobId,
        group: &JobId,
        source: &SourceId,
        want: Tuning,
        replace: &[JobId],
    ) -> Result<(), NodeError> {
        let port = self
            .port(source)
            .ok_or_else(|| NodeError::UnknownSource(source.clone()))?;
        let (members, _restarts, guard) = self.inner.group_restarts_then_gate(group, source).await;
        if !members.iter().any(|r| &r.id == id) {
            return Err(NodeError::Other(format!(
                "job {} is not running in this node (one-shot: use --node against zeta serve)",
                id.0
            )));
        }
        // Checked before the gate too; a member may have joined since.
        if let Some(m) = members.iter().find(|r| replace.contains(&r.id)) {
            return Err(NodeError::Other(crate::ops::group_replace_refusal(&m.id)));
        }
        // Every member's handler, oldest first, before anything is stopped or moved.
        let mut settled = Vec::with_capacity(members.len());
        for mut rec in members {
            if let Err(e) = self.inner.rehandle(&mut rec) {
                return Err(match &rec.id == id {
                    true => e,
                    false => NodeError::Other(format!(
                        "job {}, a member of the retuned job's group: {e}",
                        rec.id.0
                    )),
                });
            }
            settled.push(rec);
        }
        if !replace.is_empty() {
            self.replace_holders(source, replace, &guard).await?;
        }
        let ids: Vec<JobId> = settled.iter().map(|r| r.id.clone()).collect();
        let moved = self
            .inner
            .leases
            .retune_group_locked(port.as_ref(), &ids, want, &guard)
            .await;
        let (capture, tuned) = match moved {
            Ok(m) => m,
            Err(e) => {
                // As for one job: no restored lease outlives a run that ended meanwhile.
                for m in &ids {
                    self.inner.release_unless_live(m);
                }
                return Err(match e {
                    LeaseError::Port(p) => NodeError::SourcePort(source.clone(), p),
                    e => NodeError::Lease(e),
                });
            }
        };
        let bin_rate = self.inner.bank_bin_rate(source, capture);
        let modes = self.inner.modes();
        // The job named first, then the others oldest first (the sort is stable).
        settled.sort_by_key(|r| &r.id != id);
        let mut named = Ok(());
        let mut failed = false;
        let mut afters = Vec::new();
        let mut held = Vec::new();
        for rec in settled {
            let graph = self
                .inner
                .expandable_kind(&rec.kind)
                .ok_or_else(|| {
                    NodeError::Other(format!("{} is not a kind the node expands", rec.kind))
                })
                .and_then(|kind| {
                    kind.reexpand_carrying(
                        source,
                        want,
                        capture.bandwidth_hz,
                        bin_rate,
                        &rec.graph,
                        (rec.audio_policy, rec.handler.as_deref()),
                        &modes,
                        &RETUNE_CARRIED_PARAMS,
                    )
                    .map_err(NodeError::Expand)
                })
                .and_then(|g| self.inner.sinked(g, &rec.sinks));
            let member = rec.id.clone();
            // Before the run stops: its attached jobs' supervisors keep them `Running`.
            held.extend(self.inner.hold_attached(&member));
            let r = match graph {
                Ok(graph) => {
                    let new = JobRecord {
                        graph,
                        tuning: want,
                        capture_rate_hz: Some(capture.bandwidth_hz.0),
                        operator_graph: false,
                        ..rec
                    };
                    let (r, after) = self.inner.relaunch_in_place(new, Some(&guard)).await;
                    afters.push(after);
                    r
                }
                Err(e) => {
                    // Its lease has moved with the others: it cannot run on as it was.
                    self.inner.fail_in_place(&member, &e).await;
                    Err(e)
                }
            };
            failed |= r.is_err();
            if &member == id {
                named = r;
            }
        }
        drop(guard);
        for after in afters {
            self.inner.after_gate(after).await;
        }
        // The members' launches found their leases inside the new capture and tuned
        // nothing, so nobody else relaunches the elastic jobs.
        if tuned {
            let _ = self.inner.relaunch_elastic(source, Some(id)).await;
        }
        if failed {
            // A relaunch that failed let its lease go: perhaps the last one.
            self.idle_if_unheld(port.as_ref()).await;
        }
        // On their inputs' new streams, or `failed` as `input job <id> not running`.
        self.inner.relaunch_attached(held).await;
        named
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
        // The holders' restart locks (held until their attached jobs are relaunched),
        // then the gate.
        let (_restarts, guard) = self.inner.restarts_then_gate(source).await;
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
                    rec.audio_policy,
                    rec.handler.as_deref(),
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
    /// Wait for job `id`'s restart lock (see [`Inner::restarts`]) and hold it until the
    /// guard drops; `None` (nothing held) for a job the node has no record of. Never
    /// called while holding a source's gate.
    pub(super) async fn restart_guard(&self, id: &JobId) -> Option<OwnedMutexGuard<()>> {
        let lock = {
            let jobs = self.jobs.lock().unwrap();
            if !jobs.contains_key(id) {
                return None;
            }
            self.restarts
                .lock()
                .unwrap()
                .entry(id.clone())
                .or_default()
                .clone()
        };
        Some(lock.lock_owned().await)
    }

    /// The restart locks of every job holding `source` (in id order), then its gate,
    /// for a capture move, which learns the holders it restarts only under the gate. A
    /// holder that took its lease while the locks were taken (it holds none of them)
    /// lets everything go and the round is taken again, so no lock is ever waited for
    /// under the gate.
    pub(super) async fn restarts_then_gate(
        &self,
        source: &SourceId,
    ) -> (Vec<OwnedMutexGuard<()>>, OwnedMutexGuard<()>) {
        loop {
            let mut ids: Vec<JobId> = self
                .leases
                .holders(source)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            ids.sort();
            ids.dedup();
            let mut locked = Vec::new();
            let mut guards = Vec::new();
            for id in ids {
                if let Some(g) = self.restart_guard(&id).await {
                    guards.push(g);
                    locked.push(id);
                }
            }
            let gate = self.leases.gate_guard(source).await;
            let covered = {
                let jobs = self.jobs.lock().unwrap();
                self.leases
                    .holders(source)
                    .iter()
                    .all(|(id, _)| locked.contains(id) || !jobs.contains_key(id))
            };
            if covered {
                return (guards, gate);
            }
        }
    }

    /// The Send to group `id` is retuned with, and its source: `id` runs here (a live
    /// run, `Running`) in a group with another member running here on its source. `None`
    /// for a job in no group, or alone in its group: the retune is the one job's.
    pub(super) fn retuned_group(&self, id: &JobId) -> Option<(JobId, SourceId)> {
        let jobs = self.jobs.lock().unwrap();
        let rec = &jobs
            .get(id)
            .filter(|e| e.cancel.is_some() && e.record.state == JobState::Running)?
            .record;
        let group = rec.group.clone()?;
        let others = jobs.values().any(|e| {
            e.record.id != *id
                && e.cancel.is_some()
                && e.record.state == JobState::Running
                && e.record.group.as_ref() == Some(&group)
                && e.record.source == rec.source
        });
        others.then(|| (group, rec.source.clone()))
    }

    /// The records of the members of the Send to group `group` running here on
    /// `source` (a live run, `Running`), oldest first.
    fn running_members(&self, group: &JobId, source: &SourceId) -> Vec<JobRecord> {
        let jobs = self.jobs.lock().unwrap();
        let mut v: Vec<JobRecord> = jobs
            .values()
            .filter(|e| {
                e.cancel.is_some()
                    && e.record.state == JobState::Running
                    && e.record.group.as_ref() == Some(group)
                    && &e.record.source == source
            })
            .map(|e| e.record.clone())
            .collect();
        v.sort_by(|a, b| {
            a.created_ns
                .0
                .cmp(&b.created_ns.0)
                .then_with(|| a.id.0.cmp(&b.id.0))
        });
        v
    }

    /// [`Inner::restarts_then_gate`] for a Send to group's retune: the restart locks of
    /// the members of `group` running on `source` (in id order), then its gate, and the
    /// members running under it, oldest first. Only the members are locked, never the
    /// source's other holders. A member that started while the locks were taken (it
    /// holds none of them) lets everything go and the round is taken again, so no lock
    /// is ever waited for under the gate; one that stopped meanwhile is left out.
    pub(super) async fn group_restarts_then_gate(
        &self,
        group: &JobId,
        source: &SourceId,
    ) -> (
        Vec<JobRecord>,
        Vec<OwnedMutexGuard<()>>,
        OwnedMutexGuard<()>,
    ) {
        loop {
            let mut ids: Vec<JobId> = self
                .running_members(group, source)
                .into_iter()
                .map(|r| r.id)
                .collect();
            ids.sort();
            let mut locked = Vec::new();
            let mut guards = Vec::new();
            for id in ids {
                if let Some(g) = self.restart_guard(&id).await {
                    guards.push(g);
                    locked.push(id);
                }
            }
            let gate = self.leases.gate_guard(source).await;
            let members = self.running_members(group, source);
            if members.iter().all(|r| locked.contains(&r.id)) {
                return (members, guards, gate);
            }
        }
    }

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
        let run = match self.end_for_relaunch(&id).await {
            Ok(run) => run,
            Err(e) => return (Err(e), AfterGate::Nothing),
        };
        let (r, after) = self.launch_gated(rec, gate).await;
        if let Err(e) = &r {
            self.fail_relaunch(&id, run, e);
        }
        (r, after)
    }

    /// End the live run of `id` as [`Inner::relaunch_in_place`] does before its
    /// launch, then end the job `Failed` with `e`, the reason its relaunch could not be
    /// made (a graph that cannot be expanded for the band its lease moved to): its lease
    /// released, the port not idled here. A job without a live run is as there.
    pub(super) async fn fail_in_place(&self, id: &JobId, e: &NodeError) {
        if let Ok(run) = self.end_for_relaunch(id).await {
            self.fail_relaunch(id, run, e);
        }
    }

    /// The first half of [`Inner::relaunch_in_place`]: the live run of `id` (run
    /// number returned) stopped for a relaunch and awaited through its `ended`; a job
    /// without one is `Stopped`, its lease released.
    async fn end_for_relaunch(&self, id: &JobId) -> Result<u64, NodeError> {
        let (run, ended) = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(e) = jobs
                .get_mut(id)
                .filter(|e| e.cancel.is_some() && e.record.state == JobState::Running)
            else {
                // Stopped since the caller looked: no run holds the lease the caller
                // may have moved for it.
                drop(jobs);
                self.leases.release(id);
                return Err(NodeError::Stopped(id.clone()));
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
        Ok(run)
    }

    /// A relaunch of `id` (its run `run` ended by [`Inner::end_for_relaunch`]) that
    /// could not be made: the lease released and the job `Failed` with `e`.
    fn fail_relaunch(&self, id: &JobId, run: u64, e: &NodeError) {
        if let Some(entry) = self.jobs.lock().unwrap().get_mut(id) {
            entry.relaunch = false;
        }
        self.leases.release(id);
        self.finish(
            id,
            run,
            JobState::Failed {
                reason: e.to_string(),
            },
        );
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
    /// on its input's current streams (the launch repoints `stream_in`; it takes
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
                let mut rec = e.record.clone();
                // The record's copy of its input's band follows a retune of the input
                // (the launch persists it).
                if let Some(input) = rec.input.as_ref().and_then(|i| jobs.get(i)) {
                    rec.tuning = input.record.tuning;
                }
                rec
            };
            let tuning = rec.tuning;
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
                    // The `Failed` record keeps the tuning it was asked to run at, its
                    // input's new one (`finish` persists it).
                    if entry.record.state == JobState::Running {
                        entry.record.tuning = tuning;
                    }
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
    /// the source to re-aim its running sweep ([`super::self_sweep::Reaim`],
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
                let asked = super::self_sweep::Reaim {
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
