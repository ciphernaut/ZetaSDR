//! Resuming records: the startup resume of [`super::Node::open`] and its replay for one
//! source whose port heals ([`Inner::recover_source`]).
//!
//! A supervised worker port (`zeta-source-worker`) that loses its device faults, and every
//! job on the source fails with a `source <id>: ` reason. When a fresh worker finds the
//! device again the port bumps [`Port::healed`]; the node then does for that one source
//! what `open` does for all of them: the gain from the source's record, the pin from the
//! record, then the records that failed with the fault, relaunched under their own ids
//! in resume order (widest first, elastic jobs after the rigid ones, scans after both,
//! attached jobs last, once their inputs run). Only the records that failed during the
//! current fault episode come back: a record that failed with the same prefix before
//! (in an earlier episode, or under an earlier process) stays as it was.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OwnedMutexGuard;

use super::launch::AfterGate;
use super::persist::{now, persist};
use super::{Inner, JobEntry, NodeError, gain_error, resume_capture, source_fault_reason};
use crate::job::{JobRecord, JobState};
use crate::lease::LeaseError;
use crate::ops::JobKind;
use crate::port::{Port, Starting, Tuning, measured_line, measuring_line};
use crate::stream::{Frame, Payload};
use crate::types::{JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};

/// How long a recovery waits for the runs of the records it is about to relaunch to
/// finish ending (their supervisors to release their leases and streams), so a
/// supervisor still on its way out never releases the lease of the run that replaces
/// it. A run that failed with the fault is normally long gone by the heal.
const ENDED_WAIT: Duration = Duration::from_secs(10);

/// A source's fault episode: open from the first time the node sees the source's port
/// faulted (a job failing with the `source <id>: ` prefix, `source_list` reading the
/// fault, a tune the faulted port refused) until the heal that recovers the source. Only
/// a port that can heal ([`Port::healed`] is `Some`, a supervised worker) has episodes:
/// for any other (a KiwiSDR, another node, a VRT sender, an in-process device) no heal
/// would ever close one, and its failed records would escape the history prune forever. A
/// record is recovered only when it ended inside the episode (`JobEntry::ended_ns` at
/// or after `since_ns`), so the heal never relaunches a record an operator or an earlier
/// fault left failed. Kept in memory only: a node restart starts a fresh episode, as
/// its startup resume relaunches the records anyway.
#[derive(Clone, Copy, Debug)]
pub(super) struct Episode {
    pub(super) since_ns: u64,
}

/// The one source a [`Inner::resume_records`] call is about, with its gate the caller
/// holds: every lease is taken under it and nothing that takes the gate runs inside.
pub(super) struct Scope<'a> {
    pub(super) source: &'a SourceId,
    pub(super) gate: &'a OwnedMutexGuard<()>,
}

/// What [`Inner::resume_records`] leaves to its caller.
pub(super) struct Resumed {
    /// The capture holds [`Inner::resume_captures`] took: release each, then idle its
    /// port (outside the gate), once every record is launched.
    pub(super) holds: Vec<(JobId, Arc<dyn Port>)>,
    /// The records not launched (filtered out, or of another source than the scope's),
    /// as given.
    pub(super) skipped: Vec<JobRecord>,
    /// The records launched, in launch order.
    pub(super) launched: Vec<JobId>,
    /// What the launches under a [`Scope`]'s gate left for after it (see
    /// [`AfterGate`]); empty without a scope, whose launches ran theirs at once.
    pub(super) after: Vec<AfterGate>,
    /// The failure to persist a record whose launch failed; the loop stops there.
    pub(super) error: Option<NodeError>,
}

/// Whether `e`'s record failed with its source's fault during that source's episode in
/// `episodes`: a job on the radio whose reason carries the `source <id>: ` prefix, or an
/// attached job that failed because its input stopped running, whose input runs again
/// or is itself recoverable (so it comes back once the input does).
fn recoverable_entry(
    jobs: &HashMap<JobId, JobEntry>,
    episodes: &HashMap<SourceId, Episode>,
    e: &JobEntry,
) -> bool {
    let rec = &e.record;
    let Some(ep) = episodes.get(&rec.source) else {
        return false;
    };
    let JobState::Failed { reason } = &rec.state else {
        return false;
    };
    if e.ended_ns.0 < ep.since_ns {
        return false;
    }
    match &rec.input {
        None => reason.starts_with(&format!("source {}: ", rec.source.0)),
        Some(input) => {
            *reason == NodeError::InputNotRunning(input.clone()).to_string()
                && jobs.get(input).is_some_and(|i| {
                    (i.record.state == JobState::Running && i.cancel.is_some())
                        || recoverable_entry(jobs, episodes, i)
                })
        }
    }
}

impl Inner {
    pub(super) fn port(&self, id: &SourceId) -> Option<Arc<dyn Port>> {
        self.ports.read().unwrap().get(id).cloned()
    }

    /// Open `source`'s fault episode unless one is open (see [`Episode`]). Call it before
    /// the record that saw the fault is stamped with its end, so that record falls
    /// inside the episode. A no-op for a source whose port cannot heal: nothing would
    /// close the episode.
    pub(super) fn note_fault(&self, source: &SourceId) {
        if self.port(source).is_none_or(|p| p.healed().is_none()) {
            return;
        }
        self.episodes
            .lock()
            .unwrap()
            .entry(source.clone())
            .or_insert_with(|| Episode { since_ns: now().0 });
    }

    /// [`Inner::note_fault`] when `source`'s port reports a fault now: for a refusal
    /// that may or may not be the fault's (a tune error).
    pub(super) fn note_fault_if_faulted(&self, source: &SourceId) {
        if self.port(source).is_some_and(|p| p.fault().is_some()) {
            self.note_fault(source);
        }
    }

    /// When `source`'s open fault episode began, if one is open. Only the tests read
    /// it (`Node::debug_fault_episode`), so it is built with them alone.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub(super) fn episode_since(&self, source: &SourceId) -> Option<u64> {
        self.episodes
            .lock()
            .unwrap()
            .get(source)
            .map(|e| e.since_ns)
    }

    /// Whether `jobs` entry `e` is to be kept from the history prune: it is recoverable
    /// in its source's open episode, so the heal can still relaunch it.
    pub(super) fn keep_from_prune(&self, jobs: &HashMap<JobId, JobEntry>, e: &JobEntry) -> bool {
        recoverable_entry(jobs, &self.episodes.lock().unwrap(), e)
    }

    /// Whether the recovery of `rec`'s source relaunches it: it failed with the source's
    /// fault during the source's open episode (see [`recoverable_entry`]). Read at the
    /// moment of its launch, so an attached record sees whether its input came back.
    fn recoverable(&self, rec: &JobRecord) -> bool {
        let jobs = self.jobs.lock().unwrap();
        let Some(e) = jobs.get(&rec.id) else {
            return false;
        };
        recoverable_entry(&jobs, &self.episodes.lock().unwrap(), e)
    }

    /// Whether `open` relaunches `rec`: it is `Running`, or it `Failed` only
    /// because its source was absent and that source is now attached, or (an
    /// attached job) only because its input job was not running and it now runs.
    pub(super) fn resumable(&self, rec: &JobRecord) -> bool {
        match (&rec.state, &rec.input) {
            (JobState::Running, _) => true,
            (JobState::Failed { reason }, Some(input)) => {
                *reason == NodeError::InputNotRunning(input.clone()).to_string()
                    && !matches!(self.audio_stream(input), Err(NodeError::InputNotRunning(_)))
            }
            (JobState::Failed { reason }, None) => {
                reason.starts_with("source ")
                    && reason.ends_with(" not present")
                    && self.port(&rec.source).is_some()
            }
            _ => false,
        }
    }

    /// Relaunch, in resume order, the records `relaunch` accepts (read at each one's
    /// turn, so an attached record sees its input relaunched before it): jobs on the
    /// radio first, the widest band first (ties oldest first) inside the capture
    /// [`Inner::resume_captures`] restores, elastic jobs after every rigid one (they
    /// join whatever capture those left), scans after both however wide (a stepped
    /// scan gives way to every other request, so it waits for a source the others hold
    /// and stops the elastic jobs of one they leave free, as it would live), then
    /// leaseless jobs (a mix lists what runs), and attached jobs last, on their inputs' streams. A
    /// launch that fails leaves its record `Failed` with the reason (`source <id> not present` for an absent source,
    /// the port's refusal under the source's name), persisted, with a fresh entry.
    ///
    /// Shared by `open` (every source, `relaunch` its [`Inner::resumable`]) and
    /// [`Inner::recover_source`] (`only` the healed source under its gate, `relaunch`
    /// its [`Inner::recoverable`]), so a recovery is the startup resume for one source.
    pub(super) async fn resume_records(
        self: &Arc<Self>,
        mut records: Vec<JobRecord>,
        only: Option<Scope<'_>>,
        relaunch: impl Fn(&Inner, &JobRecord) -> bool,
    ) -> Resumed {
        records.sort_by_key(|r| {
            (
                r.input.is_some(),
                r.leaseless(),
                r.kind == JobKind::Scan.name(),
                r.elastic,
                Reverse(r.tuning.bandwidth_hz.0),
                r.created_ns.0,
            )
        });
        let holds = self
            .resume_captures(&records, only.as_ref(), &relaunch)
            .await;
        let mut out = Resumed {
            holds,
            skipped: Vec::new(),
            launched: Vec::new(),
            after: Vec::new(),
            error: None,
        };
        let mut records = records.into_iter();
        for rec in records.by_ref() {
            let in_scope = only.as_ref().is_none_or(|s| &rec.source == s.source);
            if !in_scope || !relaunch(self, &rec) {
                out.skipped.push(rec);
                continue;
            }
            let mut failed = rec.clone();
            let (id, source) = (rec.id.clone(), rec.source.clone());
            let launched = match &only {
                None => self.launch(rec).await,
                Some(s) => {
                    let (r, after) = self.launch_gated(rec, Some(s.gate)).await;
                    out.after.push(after);
                    r
                }
            };
            let e = match launched {
                Ok(()) => {
                    out.launched.push(id);
                    continue;
                }
                Err(e) => e,
            };
            let reason = match e {
                NodeError::UnknownSource(s) => format!("source {} not present", s.0),
                NodeError::Lease(LeaseError::Port(e)) => source_fault_reason(&source, &e),
                e => e.to_string(),
            };
            // Before the end is stamped, so the record falls inside the episode.
            if reason.starts_with(&format!("source {}: ", source.0)) {
                self.note_fault_if_faulted(&source);
            }
            failed.state = JobState::Failed { reason };
            if let Err(e) = persist(&self.jobs_dir, &failed) {
                out.error = Some(e);
                break;
            }
            self.jobs
                .lock()
                .unwrap()
                .insert(failed.id.clone(), JobEntry::loaded(failed, now()));
        }
        out.skipped.extend(records);
        out
    }

    /// Before resuming, hold each source at the capture its resumable rigid records
    /// shared (see [`resume_capture`]) with a lease under a fresh id, so the first
    /// launch does not tune it down to its own band and the narrower ones fit again.
    /// The caller releases each returned lease once every record is launched (a scan
    /// then waiting for the hold takes the source if nothing else holds it). A scan
    /// takes no part of the capture; a pinned source is left alone: the pin is its
    /// capture. With `only`, its source alone, leased under its gate.
    async fn resume_captures(
        &self,
        records: &[JobRecord],
        only: Option<&Scope<'_>>,
        relaunch: &impl Fn(&Inner, &JobRecord) -> bool,
    ) -> Vec<(JobId, Arc<dyn Port>)> {
        let mut sources: Vec<&SourceId> = records
            .iter()
            .filter(|r| r.input.is_none())
            .map(|r| &r.source)
            .filter(|s| only.is_none_or(|o| o.source == *s))
            .collect();
        sources.sort_by(|a, b| a.0.cmp(&b.0));
        sources.dedup();
        let mut held = Vec::new();
        for source in sources {
            let Some(port) = self.port(source) else {
                continue;
            };
            if self.leases.pinned(source).is_some() {
                continue;
            }
            let mine: Vec<&JobRecord> = records
                .iter()
                .filter(|r| &r.source == source && r.input.is_none() && relaunch(self, r))
                .collect();
            // A scan resumes after the others and takes no part of their capture.
            let mine: Vec<&JobRecord> = mine
                .into_iter()
                .filter(|r| r.kind != JobKind::Scan.name())
                .collect();
            let rigid: Vec<Tuning> = mine
                .iter()
                .filter(|r| !r.elastic)
                .map(|r| r.tuning)
                .collect();
            let elastic = mine.iter().filter(|r| r.elastic).map(|r| r.tuning);
            let Some(capture) = resume_capture(port.capabilities(), &rigid, elastic) else {
                continue;
            };
            let holder = JobId::new();
            let leased = match only {
                Some(s) => self
                    .leases
                    .ensure_locked(port.as_ref(), holder.clone(), capture, false, s.gate)
                    .await
                    .map(|(t, _)| t),
                None => {
                    self.leases
                        .ensure(port.as_ref(), holder.clone(), capture, false)
                        .await
                }
            };
            if leased.is_ok() {
                held.push((holder, port));
            }
        }
        held
    }

    /// Recover source `id` after its port healed: the startup resume replayed for this
    /// one source. Under the source's gate, so no `job_start` interleaves: cache the
    /// port's capabilities again, re-apply the gain from `sources/<id>.json` (a refusal
    /// is logged, not fatal), re-take the pin from that record (a live fault left the
    /// pin lease held, so this tunes the same band again; a pin that failed at startup
    /// was never leased, which is why the record and not the lease is the truth; a
    /// failure is logged and the record keeps the pin), then relaunch the records that
    /// failed with the source's fault during this episode with
    /// [`Inner::resume_records`], the attached ones after their inputs. The episode
    /// closes once the port reports no fault (a port that faulted again during the
    /// recovery keeps it open, so its next heal relaunches what failed this time). One
    /// stderr line says what came back. With no episode open the gain and pin are
    /// re-applied and nothing is relaunched.
    ///
    /// Jobs learn of a fault by polling [`Port::fault`], so a worker back quicker than a
    /// poll could leave one `Running` on a radio nobody tunes. A port that still reports
    /// its fault here opens the episode and has its jobs waited for, running ones too;
    /// [`Port::heal_settled`] then ends the fault, under the gate.
    pub(crate) async fn recover_source(self: &Arc<Self>, id: &SourceId) {
        let Some(port) = self.port(id) else {
            return;
        };
        let held = port.fault().is_some();
        if held {
            self.note_fault(id);
        }
        let episode = self.episodes.lock().unwrap().get(id).copied();
        if episode.is_some() {
            self.wait_ended(id, held).await;
        }
        let guard = self.leases.gate_guard(id).await;
        port.heal_settled();
        self.note_capabilities(port.as_ref());
        let (gain, pin) = self
            .sources
            .lock()
            .unwrap()
            .get(id)
            .map_or((None, None), |r| (r.gain.as_ref().map(|g| g.db), r.pin));
        let gain = match gain {
            Some(db) => match port.set_gain(db).await {
                Ok(_) => Some(db),
                Err(e) => {
                    eprintln!("zeta: source {}: gain: {}", id.0, gain_error(e));
                    None
                }
            },
            None => None,
        };
        let mut after = Vec::new();
        let pin = match pin {
            Some(t) => match self.leases.pin_locked(port.as_ref(), t, &guard).await {
                Ok((_, tuned)) => {
                    if tuned {
                        after.push(AfterGate::Relaunch {
                            source: id.clone(),
                            except: None,
                        });
                    }
                    Some(t)
                }
                Err(e) => {
                    eprintln!(
                        "zeta: source {}: pin {}/{} Hz: {e}",
                        id.0, t.center_hz.0, t.bandwidth_hz.0
                    );
                    None
                }
            },
            None => None,
        };
        let mut launched = Vec::new();
        let mut holds = Vec::new();
        if episode.is_some() {
            let records: Vec<JobRecord> = self
                .jobs
                .lock()
                .unwrap()
                .values()
                .filter(|e| &e.record.source == id)
                .map(|e| e.record.clone())
                .collect();
            let scope = Scope {
                source: id,
                gate: &guard,
            };
            let resumed = self
                .resume_records(records, Some(scope), |inner, rec| inner.recoverable(rec))
                .await;
            if let Some(e) = resumed.error {
                eprintln!("zeta: source {}: recovery: {e}", id.0);
            }
            launched = resumed.launched;
            after.extend(resumed.after);
            holds = resumed.holds;
        }
        // A tune here (the pin's, a relaunched rigid job's) moves the elastic jobs that
        // follow the capture, but the ones this recovery relaunched launched last, on
        // the capture as it now is: relaunch only when some other elastic job follows.
        let tuned = after
            .iter()
            .any(|a| matches!(a, AfterGate::Relaunch { .. }));
        after.retain(|a| !matches!(a, AfterGate::Relaunch { .. }));
        if tuned
            && self
                .leases
                .elastic_holders(id)
                .iter()
                .any(|j| !launched.contains(j))
        {
            after.push(AfterGate::Relaunch {
                source: id.clone(),
                except: None,
            });
        }
        for (holder, _) in &holds {
            self.leases.release(holder);
        }
        drop(guard);
        // Outside the gate: each of these takes it.
        for a in after {
            self.after_gate(a).await;
        }
        for (_, port) in holds {
            self.leases.idle_if_unheld(port.as_ref()).await;
        }
        if port.fault().is_none() {
            self.episodes.lock().unwrap().remove(id);
        }
        let mut parts = Vec::new();
        if let Some(t) = pin {
            parts.push(format!("pin {}/{} Hz", t.center_hz.0, t.bandwidth_hz.0));
        }
        if let Some(db) = gain {
            parts.push(match db {
                Some(db) => format!("gain {db} dB"),
                None => "gain auto".to_string(),
            });
        }
        parts.push(format!("{} jobs relaunched", launched.len()));
        eprintln!("zeta: source {}: recovered: {}", id.0, parts.join(", "));
    }

    /// Wait (up to [`ENDED_WAIT`]) for every failed record of `source` whose run is
    /// still ending to release its lease and streams, so its supervisor never releases
    /// the lease of the run a recovery starts in its place. With `running`, the port still
    /// reports its fault, so every job on the source that polls it is about to fail of it
    /// (an attached one with its input): those are waited for too. A blind run
    /// ([`JobEntry::blind`]) and a job attached to one never end of a fault, so they are
    /// not, and do not delay the others. One that outwaits the bound is logged.
    async fn wait_ended(&self, source: &SourceId, running: bool) {
        let mut ending: Vec<_> = {
            let jobs = self.jobs.lock().unwrap();
            let blind = |e: &JobEntry| {
                e.blind
                    || e.record
                        .input
                        .as_ref()
                        .and_then(|input| jobs.get(input))
                        .is_some_and(|input| input.blind)
            };
            jobs.values()
                .filter(|e| &e.record.source == source)
                // A scan waiting for the source reads no fault (it tunes nothing): it
                // waits on, and takes the source back once the recovery frees it.
                .filter(|e| match e.record.state {
                    JobState::Failed { .. } => true,
                    JobState::Running => {
                        running && !blind(e) && !self.leases.is_waiting(&e.record.id)
                    }
                    _ => false,
                })
                .filter_map(|e| Some((e.record.id.clone(), e.ended.clone()?)))
                .collect()
        };
        let all = async {
            for (_, ended) in &mut ending {
                // Closed once the run has released (`Err`), which is all this waits for.
                while ended.changed().await.is_ok() {}
            }
        };
        if tokio::time::timeout(ENDED_WAIT, all).await.is_err() {
            let late: Vec<_> = ending
                .iter()
                .filter(|(_, ended)| ended.has_changed().is_ok())
                .map(|(id, _)| id.0.as_str())
                .collect();
            eprintln!(
                "zeta: source {}: recovery: job {} did not end in {} s; left running",
                source.0,
                late.join(","),
                ENDED_WAIT.as_secs()
            );
        }
    }
}

/// Recover `source` (see [`Inner::recover_source`]) on every bump of its port's healed
/// counter `rx` to a value above 0, until the node shuts down or the port drops the
/// counter. `rx` has seen nothing when the port is attached, so a bump before that is
/// taken for a heal too, on the first look: a port holding its fault until
/// [`Port::heal_settled`] relies on it. The `> 0` guard keeps a counter that starts at 0
/// from recovering a source that never faulted.
pub(super) async fn watch_healed(
    inner: Arc<Inner>,
    source: SourceId,
    mut rx: tokio::sync::watch::Receiver<u64>,
) {
    loop {
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            r = rx.changed() => if r.is_err() {
                return;
            },
        }
        let healed = *rx.borrow_and_update();
        if healed > 0 {
            inner.recover_source(&source).await;
        }
    }
}

/// Say on `source`'s feed when its worker begins and ends a start it reports
/// ([`Port::starting_changed`], `rx`): on each bump the port's worker record is read,
/// and `starting` going from none to some publishes one event `{"source_event":
/// "measuring", "text": measuring_line(..), "starting": <it>}`, back to none
/// `{"source_event": "measured", "text": measured_line(..)}` timed from the first.
/// A step alone publishes nothing, and neither does the end of a worker that died
/// before its hello (the port faulted with no live worker: the fault says why). A
/// respawn's hello still reports the fault it heals until the node settles the heal,
/// so a live worker (its hello's `since_ns`) is what tells the two ends apart. The
/// events go out on a stream of the source's own (no job, no channel), registered at
/// the first, which the bus forwards to the feed; `events.log` has them through the
/// feed's tee. Runs until the node shuts down or the port drops the counter.
pub(super) async fn watch_starting(
    inner: Arc<Inner>,
    source: SourceId,
    mut rx: tokio::sync::watch::Receiver<u64>,
) {
    let mut since: Option<std::time::Instant> = None;
    let mut stream: Option<StreamId> = None;
    loop {
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            r = rx.changed() => if r.is_err() {
                return;
            },
        }
        rx.borrow_and_update();
        let Some(port) = inner.port(&source) else {
            return;
        };
        let worker = port.worker();
        let now_starting = worker.as_ref().and_then(|w| w.starting.clone());
        let payload = match (since, now_starting) {
            (None, Some(s)) => {
                since = Some(std::time::Instant::now());
                measuring_event(&source, &s)
            }
            (Some(at), None) => {
                since = None;
                let live = worker.as_ref().is_some_and(|w| w.since_ns.is_some());
                if port.fault().is_some() && !live {
                    continue;
                }
                serde_json::json!({
                    "source_event": "measured",
                    "text": measured_line(&source, at.elapsed()),
                })
            }
            _ => continue,
        };
        let stream = stream.get_or_insert_with(|| {
            inner
                .bus
                .register(StreamKind::Event, source_provenance(&source, Timestamp(0)))
        });
        let t = now();
        inner.bus.publish(Frame::new(
            stream.clone(),
            source_provenance(&source, t),
            Payload::Event(payload),
        ));
    }
}

/// The `measuring` event of [`watch_starting`].
fn measuring_event(source: &SourceId, s: &Starting) -> serde_json::Value {
    serde_json::json!({
        "source_event": "measuring",
        "text": measuring_line(source, s),
        "starting": s,
    })
}

/// A source's own events' provenance: no job, no channel, at `t`.
fn source_provenance(source: &SourceId, t: Timestamp) -> Provenance {
    Provenance {
        source: source.clone(),
        job: None,
        channel: None,
        span: Span {
            start_ns: t,
            end_ns: t,
        },
    }
}
