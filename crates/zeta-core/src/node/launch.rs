//! Launching jobs: lease, spawn stages, supervise, the IQ forwarders, and
//! the `scan` job's step loop. Attached jobs launch here too, without the radio; the
//! `identify` job's controller is in [`super::identify`].

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::OwnedMutexGuard;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::persist::{now, persist};
use super::{Inner, JobEntry, LogFile, NodeError, ring_seconds_for, source_fault_reason, watch};
use crate::graph::{self, Graph, GraphError, NodeId};
use crate::job::{JobRecord, JobState};
use crate::lease::LeaseError;
use crate::ops::{
    JobKind, SPECTRUM_ROWS_PER_S, ScanGrid, local_hardware, scan_grid, scan_mask_default,
    scan_notch_in_force, spectrum_size,
};
use crate::port::{IqBlock, IqReceiver, Port, Tuning};
use crate::ring::IqRing;
use crate::stage::{Stage, StageContext, StageError, StageSpec};
use crate::stream::{Frame, Payload, StreamBus};
use crate::stream_in::STREAM_IN_KIND;
use crate::types::{
    ChannelId, Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};

/// What a launch under the source's gate leaves for once the gate is released
/// (see [`Inner::launch_gated`]): each takes the gate itself.
#[must_use]
pub(crate) enum AfterGate {
    Nothing,
    /// Relaunch the elastic jobs on `source` but `except`: the launch tuned it.
    Relaunch {
        source: SourceId,
        except: Option<JobId>,
    },
    /// Stop the elastic jobs on the source: a scan took it.
    StopElastic(SourceId),
}

impl Inner {
    /// [`Inner::launch_gated`] without a held gate, running its [`AfterGate`].
    pub(super) async fn launch(self: &Arc<Self>, rec: JobRecord) -> Result<(), NodeError> {
        let (r, after) = self.launch_gated(rec, None).await;
        self.after_gate(after).await;
        r
    }

    /// Run what a gated launch left for after the gate. Never under the gate.
    pub(super) async fn after_gate(self: &Arc<Self>, after: AfterGate) {
        match after {
            AfterGate::Nothing => {}
            AfterGate::Relaunch { source, except } => {
                let _ = self.relaunch_elastic(&source, except.as_ref()).await;
            }
            AfterGate::StopElastic(source) => self.stop_elastic(&source).await,
        }
    }

    /// Validate, lease, record as `Running`, persist, then spawn the job's tasks.
    /// On error nothing is recorded and no lease is held. A built-in kind's graph
    /// expanded for another rate than the leased capture is re-expanded first
    /// (see [`Inner::fit_capture`]). An elastic job joins the capture and follows
    /// it (see [`Inner::follow_capture`]); a rigid one that tuned the port then
    /// relaunches the elastic jobs there ([`Inner::relaunch_elastic`]).
    ///
    /// An attached job (`rec.input`) never touches the radio: no port, lease, tune,
    /// bank or IQ stream. Its `stream_in` nodes are first pointed at the input job's
    /// current audio stream (`InputNotRunning` / `NoAudioStream` otherwise), so a
    /// resumed attached job reads the resumed input.
    ///
    /// With `gate` (the source's gate, held by the caller) the lease is taken
    /// under it and nothing here takes the gate: the relaunch of the elastic jobs
    /// a tune moved (or a scan's stop of them) is returned instead, for the
    /// caller to run with [`Inner::after_gate`] once it drops the guard.
    pub(super) async fn launch_gated(
        self: &Arc<Self>,
        rec: JobRecord,
        gate: Option<&OwnedMutexGuard<()>>,
    ) -> (Result<(), NodeError>, AfterGate) {
        let mut after = AfterGate::Nothing;
        let r = self.launch_inner(rec, gate, &mut after).await;
        if r.is_err() {
            match &mut after {
                // The port moved and the new job did not start: every elastic job follows.
                AfterGate::Relaunch { except, .. } => *except = None,
                // No scan holds the source: the elastic jobs may stay.
                AfterGate::StopElastic(_) => after = AfterGate::Nothing,
                AfterGate::Nothing => {}
            }
        }
        (r, after)
    }

    async fn launch_inner(
        self: &Arc<Self>,
        mut rec: JobRecord,
        gate: Option<&OwnedMutexGuard<()>>,
        after: &mut AfterGate,
    ) -> Result<(), NodeError> {
        let port = match rec.input.clone() {
            Some(input) => {
                let stream = self.audio_stream(&input)?;
                point_stream_in(&mut rec.graph, &input, &stream);
                None
            }
            None => {
                let port = self.ports.read().unwrap().get(&rec.source).cloned();
                let port = port.ok_or_else(|| NodeError::UnknownSource(rec.source.clone()))?;
                if rec.kind == JobKind::Scan.name() {
                    return self.launch_scan(rec, port, gate, after).await;
                }
                if rec.kind == JobKind::Identify.name() {
                    return self.launch_identify(rec, port, gate, after).await;
                }
                Some(port)
            }
        };
        // A graph stored before the squelch gets its params (persisted with the record).
        crate::ops::fill_squelch_params(&mut rec.graph);
        let mut stages = self.stages(&rec.graph)?;
        let mut bank = None;
        // The receiver's DC (the capture's centre) for a job on a local hardware source.
        let mut dc_hz = None;
        if let Some(port) = &port {
            // Without `gate` the source's gate is held only inside `ensure_tuned`.
            let leased = match gate {
                Some(g) => {
                    self.leases
                        .ensure_locked(port.as_ref(), rec.id.clone(), rec.tuning, rec.elastic, g)
                        .await
                }
                None => {
                    self.leases
                        .ensure_tuned(port.as_ref(), rec.id.clone(), rec.tuning, rec.elastic)
                        .await
                }
            };
            // A tune the port refused is the source's failure: named after it, as a
            // job that failed on the faulted source is (a conflict stays a conflict).
            let (granted, tuned) = leased.map_err(|e| match e {
                LeaseError::Port(e) => {
                    // Before the caller stamps the failure's end (a resume, a recovery).
                    self.note_fault_if_faulted(&rec.source);
                    NodeError::SourcePort(rec.source.clone(), e)
                }
                e => NodeError::Lease(e),
            })?;
            // Only a rigid request tunes a port elastic jobs follow (an elastic one
            // tunes only an unheld port), so a relaunch never relaunches again.
            if tuned && !rec.elastic {
                *after = AfterGate::Relaunch {
                    source: rec.source.clone(),
                    except: Some(rec.id.clone()),
                };
            }
            if rec.elastic
                && let Err(e) =
                    self.follow_capture(&mut rec, port.current().unwrap_or(granted), &mut stages)
            {
                self.release_unless_live(&rec.id);
                return Err(e);
            }
            let capture = port.current();
            dc_hz = capture
                .filter(|_| local_hardware(port.capabilities()))
                .map(|b| b.center_hz.0);
            // A built-in graph expanded for another rate than the capture the lease
            // left the port at (a resume, or a rate change since the expand) is
            // expanded again for this one; the record below persists it.
            if let Some(band) = capture
                && let Err(e) = self.fit_capture(&mut rec, band, &mut stages)
            {
                self.release_unless_live(&rec.id);
                return Err(e);
            }
            // The bank for the band the lease left the port at, so the stages
            // never subscribe to one made for a band the port has left.
            bank = match capture {
                Some(band) => self.sync_bank(&rec.source, band),
                None => self
                    .banks
                    .lock()
                    .unwrap()
                    .get(&rec.source)
                    .and_then(|s| s.bank.clone()),
            };
        }
        let id = rec.id.clone();
        // Weak: a running stage's note never keeps the node alive.
        let (inner, job) = (Arc::downgrade(self), id.clone());
        // The row the job's kind expands from, for the events' envelope; a row that is
        // not ready never got this far (the expand refused it).
        let mode = self
            .expandable_kind(&rec.kind)
            .and_then(|k| k.mode_row(&self.modes).ok().flatten())
            .map(|row| Arc::new(row.clone()));
        let ctx = StageContext {
            bank,
            mode,
            dc_hz,
            ..StageContext::new(rec.source.clone())
        }
        .with_note(Arc::new(move |v| {
            if let Some(inner) = inner.upgrade() {
                let _ = inner.context_merge(&job, v);
            }
        }));

        let token = self.shutdown.child_token();
        let graph = rec.graph.clone();
        // A `transcribe` job's record for its transcript tees (see `tee_transcripts`).
        let tee = (rec.kind == JobKind::Transcribe.name()).then(|| rec.clone());
        // A `watch`'s controller runs beside its stages (see `super::watch`).
        let watch =
            (rec.kind == JobKind::Watch.name()).then(|| (watch::Seed::of(&rec), rec.tuning));
        // On error the port moved anyway: `launch_gated` relaunches every elastic job.
        let (prov, run, ended) = self.record_running(rec, &token)?;

        // One stream per edge; a node with no incoming edge whose stage takes input
        // reads the job's IQ stream (a job on the radio only). Each node's outputs get
        // streams by `node_outputs`.
        let edge_streams: Vec<_> = graph
            .edges
            .iter()
            .map(|e| self.bus.register(e.kind, prov.clone()))
            .collect();
        let roots = graph.nodes.iter().zip(&stages).any(|(n, st)| {
            !st.spec().inputs.is_empty() && !graph.edges.iter().any(|e| e.to == n.id)
        });
        let iq = (roots && port.is_some()).then(|| self.bus.register(StreamKind::Iq, prov.clone()));
        let mut streams: Vec<StreamId> = edge_streams.iter().cloned().chain(iq.clone()).collect();
        // The job's output streams (no edge consumes them), in graph order: what a job
        // attached to this one can read.
        let mut job_outputs = Vec::new();
        // The nodes with no outputs (a sink): an edge into one is still an output of
        // the job, so the sink does not hide the events it is fed from `stream tail`
        // and from the watch's silence check.
        let sinks: HashSet<&NodeId> = graph
            .nodes
            .iter()
            .zip(&stages)
            .filter(|(_, st)| st.spec().outputs.is_empty())
            .map(|(n, _)| &n.id)
            .collect();
        let mut set = JoinSet::new();
        for (n, stage) in graph.nodes.iter().zip(stages) {
            let mut inputs = Vec::new();
            let mut leaving = Vec::new();
            for (e, sid) in graph.edges.iter().zip(&edge_streams) {
                if e.to == n.id {
                    inputs.push(self.bus.subscribe(sid).expect("registered"));
                }
                if e.from == n.id {
                    leaving.push((e.kind, sid.clone()));
                    if sinks.contains(&e.to) {
                        job_outputs.push((e.kind, sid.clone()));
                    }
                }
            }
            if inputs.is_empty()
                && !stage.spec().inputs.is_empty()
                && let Some(sid) = &iq
            {
                inputs.push(self.bus.subscribe(sid).expect("registered"));
            }
            let before = job_outputs.len();
            let kinds = stage.outputs_for(&n.params);
            let outputs = node_outputs(stage.spec(), &kinds, leaving, |k| {
                let sid = self.bus.register(k, prov.clone());
                streams.push(sid.clone());
                job_outputs.push((k, sid.clone()));
                sid
            });
            // Subscribed before the stage runs, so its first event is not missed.
            if let Some(rec) = &tee {
                self.tee_transcripts(rec, &job_outputs[before..], &token);
            }
            let ids = outputs.into_iter().map(|(_, s)| s).collect();
            let (params, bus, cancel) = (n.params.clone(), self.bus.clone(), token.clone());
            let ctx = ctx.clone();
            let running = StageRunning::new(self.stages.clone());
            set.spawn(async move {
                let _running = running;
                stage.run(params, inputs, ids, bus, cancel, ctx).await
            });
        }
        if let (Some((seed, tuning)), Some(port)) = (watch, &port) {
            // The controller reads `detect`'s events and publishes its own beside them.
            let out = job_outputs
                .iter()
                .find(|(k, _)| *k == StreamKind::Event)
                .map(|(_, s)| s.clone());
            let rx = out.as_ref().and_then(|s| self.bus.subscribe(s));
            if let (Some(out), Some(rx)) = (out, rx) {
                let run = watch::Run::new(
                    id.clone(),
                    port.clone(),
                    tuning,
                    prov.clone(),
                    out,
                    rx,
                    token.clone(),
                    seed,
                );
                let running = StageRunning::new(self.stages.clone());
                let body = watch::watch_loop(self.clone(), run);
                set.spawn(async move {
                    let _running = running;
                    body.await
                });
            }
        }
        if let (Some(sid), Some(port)) = (iq, &port) {
            let rx = port.subscribe();
            let running = StageRunning::new(self.stages.clone());
            let forward = forward_iq(self.bus.clone(), prov, sid, port.clone(), rx, token.clone());
            set.spawn(async move {
                let _running = running;
                forward.await
            });
        }
        let h = tokio::spawn(supervise(
            self.clone(),
            id.clone(),
            (run, ended),
            port,
            set,
            token,
            streams,
        ));
        if let Some(e) = self.jobs.lock().unwrap().get_mut(&id)
            && e.run == run
        {
            e.done = Some(h);
            e.outputs = job_outputs;
        }
        Ok(())
    }

    /// Append every `transcript` event of `rec`'s `Event` outputs among `outputs`
    /// to `transcripts.log`, one task per output, each subscribed before this
    /// returns. A tee ends when its stream is unregistered at the job's end or on
    /// `token` (draining what is buffered); it is not one of the job's stages, so
    /// it never ends the job. A lag is recorded once per tee in `persist_errors`.
    pub(super) fn tee_transcripts(
        self: &Arc<Self>,
        rec: &JobRecord,
        outputs: &[(StreamKind, StreamId)],
        token: &CancellationToken,
    ) {
        for (_, sid) in outputs.iter().filter(|(k, _)| *k == StreamKind::Event) {
            let Some(mut rx) = self.bus.subscribe(sid) else {
                continue;
            };
            let job = serde_json::json!({
                "job": rec.id.0,
                "input": rec.input.as_ref().map(|j| j.0.clone()),
                "source": rec.source.0,
                "center_hz": rec.tuning.center_hz.0,
                "bandwidth_hz": rec.tuning.bandwidth_hz.0,
            });
            let (id, token) = (rec.id.clone(), token.clone());
            // Weak: a tee never keeps the node alive.
            let inner = Arc::downgrade(self);
            let running = TeeRunning::new(self.tees.clone());
            tokio::spawn(async move {
                let _running = running;
                let mut lagged = false;
                let log = |frame: Frame| {
                    if let (Some(inner), Some(line)) =
                        (inner.upgrade(), transcript_line(&job, &frame))
                    {
                        inner.log_line(LogFile::Transcripts, &line);
                    }
                };
                loop {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => {
                            // What is still buffered, past any lag, until empty.
                            loop {
                                match rx.try_recv() {
                                    Ok(frame) => log(frame),
                                    Err(TryRecvError::Lagged(_)) => continue,
                                    Err(TryRecvError::Empty | TryRecvError::Closed) => return,
                                }
                            }
                        }
                        r = rx.recv() => match r {
                            Ok(frame) => log(frame),
                            Err(RecvError::Lagged(n)) => {
                                if !std::mem::replace(&mut lagged, true)
                                    && let Some(inner) = inner.upgrade()
                                {
                                    inner.persist_errors.lock().unwrap().push(format!(
                                        "transcripts.log: job {}: {n} events skipped",
                                        id.0
                                    ));
                                }
                            }
                            Err(RecvError::Closed) => return,
                        },
                    }
                }
            });
        }
    }

    /// Relaunch every running elastic job on `source` but `except` under the same
    /// id, on the source's current capture: its stages are stopped (the record
    /// stays `Running`, the lease held), then [`Inner::launch`] joins the capture,
    /// sets the record's `tuning` to it, re-expands its graph for it and persists
    /// it, with new stream ids. Called after a tune of the port, never under the
    /// source's gate (the relaunch's own `ensure` takes it). A relaunch that fails
    /// leaves the job `Failed` with the reason; a stop that lands meanwhile wins,
    /// even one that lands while the relaunched launch waits on the gate (see
    /// [`Inner::record_running`]).
    ///
    /// The relaunch runs in its own task, whose handle is returned for the caller
    /// to await: it is cancel-safe, as dropping the op that awaits it (an HTTP
    /// timeout, a client gone) never leaves a job half relaunched with its record
    /// `Running`, its lease held and no supervisor. (A concrete handle, not an
    /// `async fn`, also keeps `launch`'s future type from containing itself.)
    pub(super) fn relaunch_elastic(
        self: &Arc<Self>,
        source: &SourceId,
        except: Option<&JobId>,
    ) -> tokio::task::JoinHandle<()> {
        let (inner, source, except) = (self.clone(), source.clone(), except.cloned());
        tokio::spawn(async move {
            inner.relaunch_elastic_now(&source, except.as_ref()).await;
        })
    }

    /// The body of [`Inner::relaunch_elastic`], in its task.
    async fn relaunch_elastic_now(self: &Arc<Self>, source: &SourceId, except: Option<&JobId>) {
        let Some(port) = self.ports.read().unwrap().get(source).cloned() else {
            return;
        };
        let _one = self.relaunching.lock().await;
        for id in self.leases.elastic_holders(source) {
            if except == Some(&id) || self.shutdown.is_cancelled() {
                continue;
            }
            let (rec, run, done) = {
                let mut jobs = self.jobs.lock().unwrap();
                let Some(e) = jobs.get_mut(&id) else {
                    continue;
                };
                // A launch in progress (recorded, its supervisor not yet set) is left
                // to finish: it joined the capture its lease was granted on.
                if e.record.state != JobState::Running || e.done.is_none() {
                    continue;
                }
                let Some(cancel) = e.cancel.take() else {
                    continue;
                };
                e.relaunch = true;
                cancel.cancel();
                (e.record.clone(), e.run, e.done.take())
            };
            if let Some(h) = done {
                let _ = h.await;
            }
            let running = self
                .jobs
                .lock()
                .unwrap()
                .get(&id)
                .is_some_and(|e| e.record.state == JobState::Running);
            let result = match running {
                true => self.launch(rec).await,
                false => Ok(()),
            };
            if !running || result.is_err() {
                if let Some(e) = self.jobs.lock().unwrap().get_mut(&id) {
                    e.relaunch = false;
                }
                self.leases.release(&id);
                self.leases.idle_if_unheld(port.as_ref()).await;
            }
            if let Err(e) = result {
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

    /// Stop every elastic job on `source`, each `Stopped` as by `job_stop`: a scan
    /// is about to time-share the source, and no capture outlasts one of its steps.
    /// A stop that cannot be persisted leaves that job running.
    async fn stop_elastic(&self, source: &SourceId) {
        let _one = self.relaunching.lock().await;
        for id in self.leases.elastic_holders(source) {
            let _ = self.stop(&id).await;
        }
    }

    /// Validate `graph` and select a stage for each of its nodes, in graph order.
    fn stages(&self, graph: &Graph) -> Result<Vec<Arc<dyn Stage>>, NodeError> {
        graph::validate(graph, &self.registry)?;
        let mut stages = Vec::with_capacity(graph.nodes.len());
        for n in &graph.nodes {
            let stage = self
                .registry
                .select(&n.kind, &self.backends)
                .ok_or_else(|| GraphError::UnknownStage {
                    node: n.id.clone(),
                    kind: n.kind.clone(),
                    known: self.registry.kinds(),
                })?;
            stages.push(stage);
        }
        Ok(stages)
    }

    /// Re-expand `rec`'s graph for the capture `band` when it is of a built-in kind
    /// and was expanded for another rate (`capture_rate_hz`), recording the new rate
    /// and reselecting `stages`. A record without a rate that the node expanded
    /// (written before the field existed) is re-expanded too, and so is one expanded
    /// for this rate that no expansion at it reproduces (an older expansion: a plan
    /// channel recorded before the adjacent-channel guard); an operator's graph
    /// (`operator_graph`, from `graph_put`) is left as stored.
    fn fit_capture(
        &self,
        rec: &mut JobRecord,
        band: Tuning,
        stages: &mut Vec<Arc<dyn Stage>>,
    ) -> Result<(), NodeError> {
        let rate = band.bandwidth_hz;
        let Some(kind) = self.expandable_kind(&rec.kind) else {
            return Ok(());
        };
        if rec.operator_graph {
            return Ok(());
        }
        let bin_rate = self.bank_bin_rate(&rec.source, band);
        let modes = &self.modes;
        let graph = kind.reexpand(&rec.source, rec.tuning, rate, bin_rate, &rec.graph, modes);
        // At the same rate the stored graph stands while one of today's expansions (on
        // the bank or the source rate) still gives it, or when none can be made.
        if rec.capture_rate_hz == Some(rate.0) {
            let unbanked = || kind.reexpand(&rec.source, rec.tuning, rate, None, &rec.graph, modes);
            let current = |g: &Result<Graph, _>| {
                g.as_ref()
                    .is_ok_and(|g| graph::same_pipeline(g, &rec.graph))
            };
            if graph.is_err() || current(&graph) || current(&unbanked()) {
                return Ok(());
            }
        }
        let graph = graph.map_err(NodeError::Expand)?;
        *stages = self.stages(&graph)?;
        rec.graph = graph;
        rec.capture_rate_hz = Some(rate.0);
        Ok(())
    }

    /// Point an elastic `rec` at the capture `band` it joined: its `tuning` becomes
    /// the capture and a built-in kind's graph is expanded again for it (whatever
    /// rate it was expanded for), reselecting `stages`; an operator's graph
    /// (`operator_graph`) keeps its graph. Unchanged when it is already there.
    fn follow_capture(
        &self,
        rec: &mut JobRecord,
        band: Tuning,
        stages: &mut Vec<Arc<dyn Stage>>,
    ) -> Result<(), NodeError> {
        if rec.tuning == band {
            return Ok(());
        }
        rec.tuning = band;
        if rec.operator_graph {
            return Ok(());
        }
        let Some(kind) = self.expandable_kind(&rec.kind) else {
            return Ok(());
        };
        let bin_rate = self.bank_bin_rate(&rec.source, band);
        let graph = kind
            .reexpand(
                &rec.source,
                band,
                band.bandwidth_hz,
                bin_rate,
                &rec.graph,
                &self.modes,
            )
            .map_err(NodeError::Expand)?;
        *stages = self.stages(&graph)?;
        rec.graph = graph;
        rec.capture_rate_hz = Some(band.bandwidth_hz.0);
        Ok(())
    }

    /// Mark a leased job `Running`, persist it and enter it in the job table
    /// with `token`; the job's base provenance. On a failed write the lease
    /// is released and nothing is entered. The check, the write and the entry
    /// happen under the jobs lock, so a `job_stop` lands either before (an elastic
    /// relaunch is then refused with `Stopped`, its lease released) or after (it
    /// cancels the new launch). A launch of a job a `replace` stopped meanwhile (a
    /// `graph_put` restart that took its lease before the replace took the gate)
    /// is refused the same way, whatever the kind.
    ///
    /// Also the run's generation (see [`JobEntry::run`]) and the sender whose drop
    /// closes the entry's `ended` (see [`JobEntry::ended`]). A launch of a job whose
    /// entry still holds a live run's token (another launch of the id recorded
    /// first: two overlapping `graph_put`s) is refused, its lease left to that run,
    /// which holds the same id's lease; replacing the entry would drop that run's
    /// token uncancelled and leave its stages running.
    pub(super) fn record_running(
        &self,
        mut rec: JobRecord,
        token: &CancellationToken,
    ) -> Result<(Provenance, u64, tokio::sync::watch::Sender<()>), NodeError> {
        rec.state = JobState::Running;
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.get(&rec.id).is_some_and(|e| e.cancel.is_some()) {
            return Err(NodeError::Other(format!(
                "job {} is already running a launch",
                rec.id.0
            )));
        }
        // An elastic job being relaunched, or any job a `replace` stopped, that was
        // stopped (or ended) meanwhile stays so: its stop already returned, so this
        // launch must not undo it.
        if jobs
            .get(&rec.id)
            .is_some_and(|e| (e.relaunch || e.gated_stop) && e.record.state != JobState::Running)
        {
            self.leases.release(&rec.id);
            return Err(NodeError::Stopped(rec.id));
        }
        if let Err(e) = persist(&self.jobs_dir, &rec) {
            self.leases.release(&rec.id);
            return Err(e);
        }
        let t = now();
        let prov = Provenance {
            source: rec.source.clone(),
            job: Some(rec.id.clone()),
            channel: None,
            span: Span {
                start_ns: t,
                end_ns: t,
            },
        };
        let run = self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let (ended, ended_rx) = tokio::sync::watch::channel(());
        jobs.insert(
            rec.id.clone(),
            JobEntry {
                ended_ns: rec.created_ns,
                record: rec,
                run,
                cancel: Some(token.clone()),
                done: None,
                ended: Some(ended_rx),
                outputs: Vec::new(),
                relaunch: false,
                gated_stop: false,
                released: false,
                restarting: false,
            },
        );
        Ok((prov, run, ended))
    }

    /// Launch a `scan` job: take a time-shared lease on its whole band
    /// (`rec.tuning`), then run [`scan_loop`] under [`supervise`] with one
    /// `Event` output stream. The registered `fft` stage does the spectra; of
    /// the graph only the first `fft` node's `notch_hz` and `edge_mask` are
    /// read (its mask). The first launch writes them into that node from the
    /// source's capabilities ([`scan_mask_default`]); a resume or a `graph put`
    /// finds them and keeps them. A record with no `fft` node (persisted before
    /// the scan carried a mask) gets the kind's expansion first. The graph is
    /// validated before the lease, so a bad stored mask fails the launch there.
    /// The elastic jobs on the source are stopped: first, or with `gate` (held
    /// by the caller, see [`Inner::launch_gated`]) once the lease is granted,
    /// through `after`.
    async fn launch_scan(
        self: &Arc<Self>,
        rec: JobRecord,
        port: Arc<dyn Port>,
        gate: Option<&OwnedMutexGuard<()>>,
        after: &mut AfterGate,
    ) -> Result<(), NodeError> {
        let stage = self.registry.select("fft", &self.backends).ok_or_else(|| {
            GraphError::UnknownStage {
                node: NodeId("scan".into()),
                kind: "fft".into(),
                known: self.registry.kinds(),
            }
        })?;
        let mut rec = rec;
        if !rec.graph.nodes.iter().any(|n| n.kind == "fft") {
            rec.graph = JobKind::Scan
                .expand(
                    &rec.source,
                    rec.tuning,
                    Hz(0),
                    None,
                    None,
                    None,
                    &self.modes,
                )
                .map_err(|e| NodeError::Other(format!("scan graph: {e}")))?;
        }
        graph::validate(&rec.graph, &self.registry)?;
        let grid = scan_grid(rec.tuning, port.capabilities());
        let w = grid.even[0].bandwidth_hz.0;
        let fft = rec
            .graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == "fft")
            .expect("expanded above");
        if let Some(p) = fft.params.as_object_mut()
            && !p.contains_key("notch_hz")
        {
            let (notch, edge) = scan_mask_default(port.capabilities());
            p.insert("notch_hz".into(), notch.into());
            p.entry("edge_mask").or_insert(edge.into());
            p.insert("size".into(), spectrum_size(w).into());
        }
        let notch_cfg = fft.params["notch_hz"].as_u64().unwrap_or(0);
        let edge_mask = fft.params["edge_mask"].as_f64().unwrap_or(0.0);
        let notch_hz = scan_notch_in_force(w, notch_cfg);
        match gate {
            Some(g) => {
                self.leases
                    .acquire_shared_locked(port.as_ref(), rec.id.clone(), rec.tuning, g)?;
                *after = AfterGate::StopElastic(rec.source.clone());
            }
            None => {
                self.stop_elastic(&rec.source).await;
                self.leases
                    .acquire_shared(port.as_ref(), rec.id.clone(), rec.tuning)
                    .await?;
                // An elastic start between the stop and the lease joined the port;
                // none can join once the scan holds it (an elastic request on a
                // time-shared source is a conflict), so stopping again after closes it.
                *after = AfterGate::StopElastic(rec.source.clone());
            }
        }
        let id = rec.id.clone();
        // `job_context` tells a reader (the agent above all) how many steps a
        // sweep takes, how the odd sweeps shift and the mask in force, instead
        // of leaving it to guess from the rows.
        if !rec.context.is_object() {
            rec.context = serde_json::json!({});
        }
        if let Some(ctx) = rec.context.as_object_mut() {
            let alternate = if grid.alternates() {
                serde_json::json!({
                    "shift_hz": grid.shift_hz(),
                    "steps_shifted": grid.odd.len(),
                })
            } else {
                serde_json::Value::Null
            };
            ctx.insert(
                "scan".into(),
                serde_json::json!({
                    "steps": grid.even.len(),
                    "step_hz": w,
                    "dwell_ms": self.scan_dwell_ms,
                    "alternate": alternate,
                    "mask": {"notch_hz": notch_hz, "edge_mask": edge_mask},
                }),
            );
        }
        let token = self.shutdown.child_token();
        let (prov, run, ended) = self.record_running(rec, &token)?;
        let out = self.bus.register(StreamKind::Event, prov.clone());
        let scan = Scan {
            id: id.clone(),
            grid,
            notch_hz,
            edge_mask,
            port: port.clone(),
            ctx: StageContext::new(prov.source.clone()),
            stage,
            prov,
            out: out.clone(),
            cancel: token.clone(),
        };
        let mut set = JoinSet::new();
        let (running, scan) = (
            StageRunning::new(self.stages.clone()),
            scan_loop(self.clone(), scan),
        );
        set.spawn(async move {
            let _running = running;
            scan.await
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

    /// The last `dwell` of `source`'s ring as one block, if the ring holds
    /// samples captured at `center` (the step's band, not an earlier one's).
    fn dwell_block(&self, source: &SourceId, center: Hz, dwell: Duration) -> Option<IqBlock> {
        let ring = self.rings.lock().unwrap().get(source).cloned()?;
        let ring = ring.read().unwrap();
        if ring.center_hz() != Some(center) {
            return None;
        }
        let (earliest, latest) = (ring.earliest()?, ring.latest()?);
        let dwell_ns = u64::try_from(dwell.as_nanos()).unwrap_or(u64::MAX);
        let start = latest.0.saturating_sub(dwell_ns).max(earliest.0);
        let samples = ring
            .read(Span {
                start_ns: Timestamp(start),
                end_ns: latest,
            })
            .ok()?;
        Some(IqBlock {
            t0: Timestamp(start),
            sample_rate_hz: ring.sample_rate_hz(),
            center_hz: center,
            samples: samples.into(),
        })
    }

    /// Release `id`'s lease after a launch failed past its grant, unless a live run
    /// of the id (another launch that recorded first) holds it: the lease is keyed
    /// by the id, and that run's supervisor releases it.
    fn release_unless_live(&self, id: &JobId) {
        let live = self
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|e| e.cancel.is_some());
        if !live {
            self.leases.release(id);
        }
    }

    /// Record how run `run` of a job ended on its own, unless it was stopped
    /// meanwhile or a later run took the job's entry (see [`JobEntry::run`]).
    /// A failed write keeps the new state in memory and is reported through
    /// `persist_errors`; the job will be resumed on the next `open`.
    fn finish(&self, id: &JobId, run: u64, state: JobState) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(entry) = jobs.get_mut(id)
            && entry.run == run
            && entry.record.state == JobState::Running
        {
            // A job on the radio failed of its source's fault: the node has seen the
            // fault, and the episode opens (if it is not open) before this end is
            // stamped, so the heal finds the record inside it.
            if let JobState::Failed { reason } = &state
                && entry.record.input.is_none()
                && reason.starts_with(&format!("source {}: ", entry.record.source.0))
            {
                self.note_fault(&entry.record.source);
            }
            entry.record.state = state;
            entry.ended_ns = now();
            entry.cancel = None;
            if let Err(e) = persist(&self.jobs_dir, &entry.record) {
                self.persist_errors
                    .lock()
                    .unwrap()
                    .push(format!("job {}: {e}", id.0));
            }
            self.prune_history(&mut jobs);
        }
    }
}

/// Point every `stream_in` node of an attached job's graph at `stream` of `input`.
fn point_stream_in(graph: &mut Graph, input: &JobId, stream: &StreamId) {
    for n in graph.nodes.iter_mut().filter(|n| n.kind == STREAM_IN_KIND) {
        if let Some(p) = n.params.as_object_mut() {
            p.insert("job".into(), input.0.clone().into());
            p.insert("stream".into(), stream.0.clone().into());
        }
    }
}

/// The streams a node's stage outputs to, with their kinds, in the order the stage
/// receives them. Each of `kinds` (the stage's [`outputs_for`](Stage::outputs_for) the
/// node's params), in order, takes the first edge of
/// its kind leaving the node not yet taken, else a job output stream from `fresh`; edges
/// left over (a second edge of one kind) follow. A stage that
/// [emits any kind](StageSpec::emits_any_kind) has one output: the edges leaving it, or
/// one job output stream of its first kind. For a single-output stage both rules give
/// the edges leaving it, or one job output stream when there is none.
fn node_outputs(
    spec: &StageSpec,
    kinds: &[StreamKind],
    mut leaving: Vec<(StreamKind, StreamId)>,
    mut fresh: impl FnMut(StreamKind) -> StreamId,
) -> Vec<(StreamKind, StreamId)> {
    if spec.emits_any_kind() {
        if leaving.is_empty()
            && let Some(&k) = spec.outputs.first()
        {
            leaving.push((k, fresh(k)));
        }
        return leaving;
    }
    let mut out = Vec::with_capacity(kinds.len());
    for &k in kinds {
        match leaving.iter().position(|(ek, _)| *ek == k) {
            Some(i) => out.push(leaving.remove(i)),
            None => out.push((k, fresh(k))),
        }
    }
    out.extend(leaving);
    out
}

/// Await a job's stages. The first to end for any reason other than
/// cancellation stops the rest; `Failed` wins over a clean end. Then release
/// the lease, tell the port it is idle if that was its last holder (a job on
/// the radio; an attached job has no port), and unregister the job's streams.
/// All but the streams (this run's own) only while the job's entry is still
/// for `run`: a later run of the id holds the record, the lease and the port.
/// `ended` is dropped once released, before the idle (see [`JobEntry::ended`]).
pub(super) async fn supervise(
    inner: Arc<Inner>,
    id: JobId,
    (run, ended): (u64, tokio::sync::watch::Sender<()>),
    port: Option<Arc<dyn Port>>,
    mut set: JoinSet<Result<(), StageError>>,
    token: CancellationToken,
    streams: Vec<StreamId>,
) {
    let mut failure = None;
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Err(StageError::Cancelled)) => {}
            Ok(Ok(())) => token.cancel(),
            Ok(Err(StageError::Failed(m))) => {
                failure.get_or_insert(m);
                token.cancel();
            }
            Err(e) => {
                failure.get_or_insert(format!("stage task: {e}"));
                token.cancel();
            }
        }
    }
    // An elastic job being relaunched keeps its record `Running` and its lease
    // for the relaunch (see `Inner::relaunch_elastic`). A run whose entry a later
    // run took leaves the record, the lease and the port to that run.
    let (ours, relaunch) = inner
        .jobs
        .lock()
        .unwrap()
        .get(&id)
        .map_or((true, false), |e| (e.run == run, e.relaunch));
    let keep = relaunch || !ours;
    // After node shutdown the record must stay `Running` so the job resumes.
    if !inner.shutdown.is_cancelled() && !keep {
        let end = match failure {
            Some(reason) => JobState::Failed { reason },
            None => match inner.input_lost_to_fault(&id) {
                Some(input) => JobState::Failed {
                    reason: NodeError::InputNotRunning(input).to_string(),
                },
                None => JobState::Stopped,
            },
        };
        inner.finish(&id, run, end);
    }
    // The only place a started job's lease is released, but for a relaunch that
    // fails (`Inner::relaunch_elastic`).
    if port.is_some() && !keep {
        inner.leases.release(&id);
    }
    for s in &streams {
        inner.bus.unregister(s);
    }
    // Read under the lock a gated stop sets it under (`Inner::stop_gated`): that
    // stop holds the source's gate, so the idle (which takes it) is left out.
    let gated_stop = {
        let mut jobs = inner.jobs.lock().unwrap();
        match jobs.get_mut(&id) {
            Some(e) if e.run == run => {
                // A relaunch keeps the lease; nothing was released.
                e.released = !relaunch;
                let gated = e.gated_stop;
                // An owned record that ended goes now (see `Inner::prune_history`).
                if e.record.owner.is_some() {
                    inner.prune_history(&mut jobs);
                }
                gated
            }
            _ => false,
        }
    };
    drop(ended);
    if let Some(port) = port
        && !keep
        && !gated_stop
    {
        inner.leases.idle_if_unheld(port.as_ref()).await;
    }
}

impl Inner {
    /// The input job of the attached job `id` when that input died of its source's
    /// fault: its record is `Failed` with a `source <id>: ` reason and its source's port
    /// still reports a fault. Such an attached job ends `Failed` as `InputNotRunning`
    /// rather than `Stopped` (its `stream_in` ended cleanly on the closed stream), the
    /// reason a resume relaunches once the input runs again, so the node's recovery of
    /// the source brings it back with its input. `None` for a job on the radio and for
    /// any other end of the input (stopped, restarted, failed for another reason).
    fn input_lost_to_fault(&self, id: &JobId) -> Option<JobId> {
        let jobs = self.jobs.lock().unwrap();
        let input = jobs.get(id)?.record.input.clone()?;
        let rec = &jobs.get(&input)?.record;
        let JobState::Failed { reason } = &rec.state else {
            return None;
        };
        if !reason.starts_with("source ") {
            return None;
        }
        let source = rec.source.clone();
        drop(jobs);
        let port = self.ports.read().unwrap().get(&source).cloned()?;
        port.fault().is_some().then_some(input)
    }
}

/// Republish a port's blocks as `Payload::Iq` frames on a job's IQ stream,
/// as one of the job's supervised tasks. A port fault (checked on every block
/// and every 200 ms) fails the job. A port that closes without a fault closes
/// the IQ stream so the stages drain and end the job; the forwarder itself
/// then waits for cancellation, so it never ends a job on its own.
async fn forward_iq(
    bus: Arc<StreamBus>,
    prov: Provenance,
    stream: StreamId,
    port: Arc<dyn Port>,
    mut rx: IqReceiver,
    cancel: CancellationToken,
) -> Result<(), StageError> {
    let faulted = |f: String| StageError::Failed(source_fault_reason(port.id(), &f));
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let block = tokio::select! {
            _ = cancel.cancelled() => return Err(StageError::Cancelled),
            _ = tick.tick() => {
                if let Some(f) = port.fault() {
                    return Err(faulted(f));
                }
                continue;
            }
            r = rx.recv() => match r {
                Ok(b) => b,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => {
                    if let Some(f) = port.fault() {
                        return Err(faulted(f));
                    }
                    bus.unregister(&stream);
                    cancel.cancelled().await;
                    return Err(StageError::Cancelled);
                }
            },
        };
        if let Some(f) = port.fault() {
            return Err(faulted(f));
        }
        let rate = block.sample_rate_hz.0.max(1) as u128;
        let end = block.t0.0 + (block.samples.len() as u128 * 1_000_000_000 / rate) as u64;
        let provenance = Provenance {
            span: Span {
                start_ns: block.t0,
                end_ns: Timestamp(end),
            },
            ..prov.clone()
        };
        bus.publish(Frame::new(stream.clone(), provenance, Payload::Iq(block)));
    }
}

/// Push a port's blocks into its ring, creating the ring on the first block
/// and sizing it to the memory budget at the new rate when the rate changes
/// (a retune; the ring restarts, so the old rate's samples are not trimmed).
pub(super) async fn feed_ring(inner: Arc<Inner>, source: SourceId, mut rx: IqReceiver) {
    let mut logged: HashSet<u64> = HashSet::new();
    loop {
        let block = tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            r = rx.recv() => match r {
                Ok(b) => b,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            },
        };
        let rate = block.sample_rate_hz.0;
        let seconds = ring_seconds_for(inner.ring_budget_bytes, rate, inner.ring_seconds);
        if logged.insert(rate) {
            eprintln!(
                "zeta: ring for {}: {seconds:.2} s at {rate} Hz ({:.1} MB)",
                source.0,
                seconds * rate as f64 * 8.0 / 1e6
            );
        }
        let ring = inner
            .rings
            .lock()
            .unwrap()
            .entry(source.clone())
            .or_insert_with(|| Arc::new(RwLock::new(IqRing::new(seconds, block.sample_rate_hz))))
            .clone();
        let mut ring = ring.write().unwrap();
        if ring.sample_rate_hz() != block.sample_rate_hz {
            // `push` restarts the ring at the new rate with this length.
            ring.set_capacity_seconds(seconds);
        }
        ring.push(&block);
    }
}

/// Forward a port's blocks to its channel bank, which does its filtering
/// elsewhere ([`ChannelBank::push`] never blocks). The first block of a new
/// capture band (rate or centre) replaces the bank with one for that band. A
/// lag (blocks lost before the bank) resets the bank.
///
/// [`ChannelBank::push`]: crate::bank::ChannelBank::push
pub(super) async fn feed_bank(inner: Arc<Inner>, source: SourceId, mut rx: IqReceiver) {
    loop {
        let block = tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            r = rx.recv() => match r {
                Ok(b) => b,
                Err(RecvError::Lagged(_)) => {
                    let bank = inner
                        .banks
                        .lock()
                        .unwrap()
                        .get(&source)
                        .and_then(|s| s.bank.clone());
                    if let Some(bank) = bank {
                        bank.reset();
                    }
                    continue;
                }
                Err(RecvError::Closed) => return,
            },
        };
        let band = Tuning {
            center_hz: block.center_hz,
            bandwidth_hz: block.sample_rate_hz,
        };
        if let Some(bank) = inner.sync_bank(&source, band) {
            bank.push(&block);
        }
    }
}

/// What a running scan needs: its lease holder id, the grids its sweeps
/// alternate between, the mask its `fft` runs apply (`notch_hz` the notch in
/// force), the `fft` stage, and its output stream.
struct Scan {
    id: JobId,
    port: Arc<dyn Port>,
    grid: ScanGrid,
    notch_hz: u64,
    edge_mask: f64,
    stage: Arc<dyn Stage>,
    ctx: StageContext,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
}

/// The `scan` job's body, until cancelled: tune the source to each step of the
/// sweep in turn (under the lease gate; even sweeps take the grid's even
/// steps, odd ones its odd steps), wait the node's dwell, run the `fft` stage
/// over the step's last dwell of ring samples (skipped when the ring holds
/// less than one FFT of the step's band), and re-publish its rows on the
/// job's stream with channel `scan/<step index>` (the index within the
/// sweep's own list) and the step's `center_hz`.
/// Cancellation is checked before every tune. A failed tune or a port fault
/// fails the job; the port is left at the last step.
async fn scan_loop(inner: Arc<Inner>, s: Scan) -> Result<(), StageError> {
    let dwell = Duration::from_millis(inner.scan_dwell_ms);
    let faulted = |f: String| StageError::Failed(source_fault_reason(s.port.id(), &f));
    let (mut sweep, mut i) = (0u64, 0usize);
    loop {
        // Checked before each tune, so a stopped scan never retunes again; a
        // tune is never abandoned half way, so it is checked after it too.
        if s.cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        let steps = s.grid.steps(sweep);
        let got = match inner
            .leases
            .retune_shared(s.port.as_ref(), &s.id, steps[i], &s.cancel)
            .await
        {
            Ok(Some(t)) => t,
            Ok(None) => return Err(StageError::Cancelled),
            Err(e) => return Err(faulted(e.to_string())),
        };
        tokio::select! {
            _ = s.cancel.cancelled() => return Err(StageError::Cancelled),
            _ = tokio::time::sleep(dwell) => {}
        }
        if let Some(f) = s.port.fault() {
            return Err(faulted(f));
        }
        let size = spectrum_size(steps[i].bandwidth_hz.0);
        if let Some(block) = inner.dwell_block(&s.prov.source, got.center_hz, dwell)
            && block.samples.len() >= size as usize
        {
            fft_step(&inner, &s, i, got.center_hz, block, size).await?;
        }
        i += 1;
        if i == steps.len() {
            i = 0;
            sweep += 1;
        }
    }
}

/// Run the scan's `fft` stage to completion over `block` as a one-shot IQ
/// stream, then forward the rows it emitted to the scan's stream, tagged
/// with channel `scan/<step>` and `center_hz`. Rows wait in the stage's
/// output stream (bus capacity) until the stage ends; one dwell yields a few.
async fn fft_step(
    inner: &Arc<Inner>,
    s: &Scan,
    step: usize,
    center: Hz,
    block: IqBlock,
    size: u32,
) -> Result<(), StageError> {
    let rate = block.sample_rate_hz.0.max(1) as u128;
    let end = block.t0.0 + (block.samples.len() as u128 * 1_000_000_000 / rate) as u64;
    let prov = Provenance {
        channel: Some(ChannelId(format!("scan/{step}"))),
        span: Span {
            start_ns: block.t0,
            end_ns: Timestamp(end),
        },
        ..s.prov.clone()
    };
    let bus = &inner.bus;
    let input = bus.register(StreamKind::Iq, prov.clone());
    let rx = bus.subscribe(&input).expect("registered");
    let output = bus.register(StreamKind::Event, prov.clone());
    let mut rows = bus.subscribe(&output).expect("registered");
    bus.publish(Frame::new(input.clone(), prov.clone(), Payload::Iq(block)));
    // Closing the input ends the stage once it has drained the block.
    bus.unregister(&input);
    let params = serde_json::json!({
        "size": size,
        "window": "hann",
        "rows_per_s": SPECTRUM_ROWS_PER_S,
        "notch_hz": s.notch_hz,
        "edge_mask": s.edge_mask,
    });
    let ran = s
        .stage
        .run(
            params,
            vec![rx],
            vec![output.clone()],
            bus.clone(),
            s.cancel.clone(),
            s.ctx.clone(),
        )
        .await;
    bus.unregister(&output);
    ran?;
    loop {
        let f = match rows.try_recv() {
            Ok(f) => f,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => return Ok(()),
        };
        let Payload::Event(mut v) = f.payload else {
            continue;
        };
        if let Some(o) = v.as_object_mut() {
            o.insert("center_hz".into(), center.0.into());
        }
        let p = Provenance {
            span: f.provenance.span,
            ..prov.clone()
        };
        bus.publish(Frame::new(s.out.clone(), p, Payload::Event(v)));
    }
}

/// Counts a running transcript tee in `Inner::tees` while it lives.
struct TeeRunning(Arc<std::sync::atomic::AtomicUsize>);

impl TeeRunning {
    fn new(count: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for TeeRunning {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Counts a running task of a job's stage set in `Inner::stages` while it lives:
/// made before the task is spawned and moved into it, so the count drops however
/// the task ends, an abort included.
pub(super) struct StageRunning(Arc<std::sync::atomic::AtomicUsize>);

impl StageRunning {
    pub(super) fn new(count: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for StageRunning {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The `transcripts.log` line for `frame` (`None` unless it is a `transcript`
/// event): `job`'s fields, the frame's span, and the event's `text`,
/// `language`, `dropped` and `error` (`null`/`false` when absent).
fn transcript_line(job: &serde_json::Value, frame: &Frame) -> Option<serde_json::Value> {
    let Payload::Event(v) = &frame.payload else {
        return None;
    };
    if v["protocol"] != "transcript" {
        return None;
    }
    let text = |k: &str| v.get(k).filter(|x| x.is_string()).cloned();
    let mut line = job.clone();
    let m = line.as_object_mut()?;
    m.insert("t_ns".into(), now().0.into());
    m.insert("start_ns".into(), frame.provenance.span.start_ns.0.into());
    m.insert("end_ns".into(), frame.provenance.span.end_ns.0.into());
    m.insert("text".into(), text("text").into());
    m.insert("language".into(), text("language").into());
    m.insert("dropped".into(), (v["dropped"] == true).into());
    m.insert("error".into(), text("error").into());
    Some(line)
}
