//! Launching jobs: lease, spawn stages, supervise, the IQ forwarders, and
//! the `scan` job's step loop.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::persist::{now, persist};
use super::{Inner, JobEntry, NodeError, ring_seconds_for};
use crate::graph::{self, GraphError, NodeId};
use crate::job::{JobRecord, JobState};
use crate::ops::{JobKind, SPECTRUM_ROWS_PER_S, scan_steps, spectrum_size};
use crate::port::{IqBlock, IqReceiver, Port, Tuning};
use crate::ring::IqRing;
use crate::stage::{Stage, StageContext, StageError};
use crate::stream::{Frame, Payload, StreamBus};
use crate::types::{
    ChannelId, Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};

impl Inner {
    /// Validate, lease, record as `Running`, persist, then spawn the job's tasks.
    /// On error nothing is recorded and no lease is held.
    pub(super) async fn launch(self: &Arc<Self>, rec: JobRecord) -> Result<(), NodeError> {
        let port = self.ports.read().unwrap().get(&rec.source).cloned();
        let port = port.ok_or_else(|| NodeError::UnknownSource(rec.source.clone()))?;
        if rec.kind == JobKind::Scan.name() {
            return self.launch_scan(rec, port).await;
        }
        graph::validate(&rec.graph, &self.registry)?;
        let mut stages = Vec::with_capacity(rec.graph.nodes.len());
        for n in &rec.graph.nodes {
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
        self.leases
            .ensure(port.as_ref(), rec.id.clone(), rec.tuning)
            .await?;

        // The bank for the band the lease left the port at, so the stages
        // never subscribe to one made for a band the port has left.
        let bank = match port.current() {
            Some(band) => self.sync_bank(&rec.source, band),
            None => self
                .banks
                .lock()
                .unwrap()
                .get(&rec.source)
                .and_then(|s| s.bank.clone()),
        };
        let ctx = StageContext {
            source: rec.source.clone(),
            bank,
        };

        let id = rec.id.clone();
        let token = self.shutdown.child_token();
        let graph = rec.graph.clone();
        let prov = self.record_running(rec, &token)?;

        // One stream per edge; a node with no outgoing edge gets one job output
        // stream of its stage's first output kind.
        let edge_streams: Vec<_> = graph
            .edges
            .iter()
            .map(|e| self.bus.register(e.kind, prov.clone()))
            .collect();
        let roots = graph
            .nodes
            .iter()
            .any(|n| !graph.edges.iter().any(|e| e.to == n.id));
        let iq = roots.then(|| self.bus.register(StreamKind::Iq, prov.clone()));
        let mut streams: Vec<StreamId> = edge_streams.iter().cloned().chain(iq.clone()).collect();
        let mut set = JoinSet::new();
        for (n, stage) in graph.nodes.iter().zip(stages) {
            let mut inputs = Vec::new();
            let mut outputs = Vec::new();
            for (e, sid) in graph.edges.iter().zip(&edge_streams) {
                if e.to == n.id {
                    inputs.push(self.bus.subscribe(sid).expect("registered"));
                }
                if e.from == n.id {
                    outputs.push(sid.clone());
                }
            }
            if inputs.is_empty() {
                let sid = iq.as_ref().expect("root nodes have the IQ stream");
                inputs.push(self.bus.subscribe(sid).expect("registered"));
            }
            if outputs.is_empty()
                && let Some(&k) = stage.spec().outputs.first()
            {
                let sid = self.bus.register(k, prov.clone());
                streams.push(sid.clone());
                outputs.push(sid);
            }
            let (params, bus, cancel) = (n.params.clone(), self.bus.clone(), token.clone());
            let ctx = ctx.clone();
            set.spawn(async move { stage.run(params, inputs, outputs, bus, cancel, ctx).await });
        }
        if let Some(sid) = iq {
            let rx = port.subscribe();
            set.spawn(forward_iq(
                self.bus.clone(),
                prov,
                sid,
                port.clone(),
                rx,
                token.clone(),
            ));
        }
        let h = tokio::spawn(supervise(
            self.clone(),
            id.clone(),
            port,
            set,
            token,
            streams,
        ));
        if let Some(e) = self.jobs.lock().unwrap().get_mut(&id) {
            e.done = Some(h);
        }
        Ok(())
    }

    /// Mark a leased job `Running`, persist it and enter it in the job table
    /// with `token`; the job's base provenance. On a failed write the lease
    /// is released and nothing is entered.
    fn record_running(
        &self,
        mut rec: JobRecord,
        token: &CancellationToken,
    ) -> Result<Provenance, NodeError> {
        rec.state = JobState::Running;
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
        self.jobs.lock().unwrap().insert(
            rec.id.clone(),
            JobEntry {
                record: rec,
                cancel: Some(token.clone()),
                done: None,
            },
        );
        Ok(prov)
    }

    /// Launch a `scan` job: take a time-shared lease on its whole band
    /// (`rec.tuning`), then run [`scan_loop`] under [`supervise`] with one
    /// `Event` output stream. The graph is ignored; the registered `fft`
    /// stage does the spectra.
    async fn launch_scan(
        self: &Arc<Self>,
        rec: JobRecord,
        port: Arc<dyn Port>,
    ) -> Result<(), NodeError> {
        let stage = self.registry.select("fft", &self.backends).ok_or_else(|| {
            GraphError::UnknownStage {
                node: NodeId("scan".into()),
                kind: "fft".into(),
                known: self.registry.kinds(),
            }
        })?;
        self.leases
            .acquire_shared(port.as_ref(), rec.id.clone(), rec.tuning)
            .await?;
        let (id, span) = (rec.id.clone(), rec.tuning);
        let token = self.shutdown.child_token();
        let prov = self.record_running(rec, &token)?;
        let out = self.bus.register(StreamKind::Event, prov.clone());
        let scan = Scan {
            id: id.clone(),
            steps: scan_steps(span, port.capabilities()),
            port: port.clone(),
            ctx: StageContext::new(prov.source.clone()),
            stage,
            prov,
            out: out.clone(),
            cancel: token.clone(),
        };
        let mut set = JoinSet::new();
        set.spawn(scan_loop(self.clone(), scan));
        let h = tokio::spawn(supervise(
            self.clone(),
            id.clone(),
            port,
            set,
            token,
            vec![out],
        ));
        if let Some(e) = self.jobs.lock().unwrap().get_mut(&id) {
            e.done = Some(h);
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

    /// Record how a job ended on its own, unless it was stopped meanwhile.
    /// A failed write keeps the new state in memory and is reported through
    /// `persist_errors`; the job will be resumed on the next `open`.
    fn finish(&self, id: &JobId, state: JobState) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(entry) = jobs.get_mut(id)
            && entry.record.state == JobState::Running
        {
            entry.record.state = state;
            entry.cancel = None;
            if let Err(e) = persist(&self.jobs_dir, &entry.record) {
                self.persist_errors
                    .lock()
                    .unwrap()
                    .push(format!("job {}: {e}", id.0));
            }
        }
    }
}

/// Await a job's stages. The first to end for any reason other than
/// cancellation stops the rest; `Failed` wins over a clean end. Then release
/// the lease, tell the port it is idle if that was its last holder, and
/// unregister the job's streams.
async fn supervise(
    inner: Arc<Inner>,
    id: JobId,
    port: Arc<dyn Port>,
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
    // After node shutdown the record must stay `Running` so the job resumes.
    if !inner.shutdown.is_cancelled() {
        inner.finish(
            &id,
            failure.map_or(JobState::Stopped, |reason| JobState::Failed { reason }),
        );
    }
    // The only place a started job's lease is released.
    inner.leases.release(&id);
    inner.leases.idle_if_unheld(port.as_ref()).await;
    for s in &streams {
        inner.bus.unregister(s);
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
    let faulted = |f: String| StageError::Failed(format!("source {}: {f}", port.id().0));
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
/// and resizing it to the memory budget when the rate changes (a retune).
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
            ring.resize(seconds);
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

/// What a running scan needs: its lease holder id, the steps it cycles
/// through, the `fft` stage, and its output stream.
struct Scan {
    id: JobId,
    port: Arc<dyn Port>,
    steps: Vec<Tuning>,
    stage: Arc<dyn Stage>,
    ctx: StageContext,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
}

/// The `scan` job's body, until cancelled: tune the source to each step in
/// turn (under the lease gate), wait the node's dwell, run the `fft` stage
/// over the step's last dwell of ring samples (skipped when the ring holds
/// less than one FFT of the step's band), and re-publish its rows on the
/// job's stream with channel `scan/<step index>` and the step's `center_hz`.
/// A failed tune or a port fault fails the job; the port is left at the last
/// step.
async fn scan_loop(inner: Arc<Inner>, s: Scan) -> Result<(), StageError> {
    let dwell = Duration::from_millis(inner.scan_dwell_ms);
    let faulted = |f: String| StageError::Failed(format!("source {}: {f}", s.port.id().0));
    let mut i = 0;
    loop {
        // A tune is never abandoned half way: cancellation is checked after it.
        let got = match inner
            .leases
            .retune_shared(s.port.as_ref(), &s.id, s.steps[i])
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
        let size = spectrum_size(s.steps[i].bandwidth_hz.0);
        if let Some(block) = inner.dwell_block(&s.prov.source, got.center_hz, dwell)
            && block.samples.len() >= size as usize
        {
            fft_step(&inner, &s, i, got.center_hz, block, size).await?;
        }
        i = (i + 1) % s.steps.len();
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
