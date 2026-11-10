//! The `scan` job: time-shares one radio across a span wider than its capture.
//! A step loop ([`scan_loop`]) tunes the source to each step, waits on the ring
//! for the step's dwell of settled samples and hands them to one FFT task
//! ([`fft_task`]), which runs the `fft` stage over each step in order and
//! publishes its row on the scan's stream. There is no sleep: a step costs its
//! tune and the time its samples take to arrive.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedMutexGuard, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::launch::{AfterGate, StageRunning, supervise};
use super::{Inner, JobOutput, NodeError, source_fault_reason};
use crate::graph::{self, GraphError, NodeId};
use crate::job::JobRecord;
use crate::ops::{
    JobKind, SCAN_ROWS_DWELL_MS, SCAN_WAIT_MARGIN_MS, SPECTRUM_ROWS_PER_S, ScanGrid,
    scan_dwell_samples, scan_edge_mask_in_force, scan_grid, scan_mask_default, scan_notch_in_force,
    spectrum_size,
};
use crate::port::{IqBlock, Port};
use crate::stage::{Stage, StageContext, StageError};
use crate::stream::{Frame, Payload};
use crate::types::{
    ChannelId, Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};

impl Inner {
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
    pub(super) async fn launch_scan(
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
                    &self.modes(),
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
        // The stored node keeps what was configured; the rows and the context
        // get the values in force: no notch on a step under four notches, no
        // edge mask on a grid that does not alternate (no odd sweep fills it in).
        let edge_mask = scan_edge_mask_in_force(
            grid.alternates(),
            fft.params["edge_mask"].as_f64().unwrap_or(0.0),
        );
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
        // The dwell in force on the first step: whole frames, never under four.
        let (_, dwell_ms) = scan_dwell_samples(self.scan_dwell_ms, w, spectrum_size(w));
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
            let mut scan = serde_json::json!({
                "steps": grid.even.len(),
                "step_hz": w,
                "dwell_ms": dwell_ms,
                "alternate": alternate,
                "mask": {"notch_hz": notch_hz, "edge_mask": edge_mask},
            });
            if dwell_ms != self.scan_dwell_ms {
                scan["dwell_ms_asked"] = self.scan_dwell_ms.into();
            }
            ctx.insert("scan".into(), scan);
        }
        let token = self.shutdown.child_token();
        let (prov, run, ended) = self.record_running(rec, &token)?;
        let out = self.bus.register_unfed(StreamKind::Event, prov.clone());
        let scan = Arc::new(Scan {
            id: id.clone(),
            grid,
            dwell_ms: self.scan_dwell_ms,
            notch_hz,
            edge_mask,
            port: port.clone(),
            ctx: StageContext::new(prov.source.clone()),
            stage,
            prov,
            out: out.clone(),
            cancel: token.clone(),
        });
        // The hand-off: one step waiting while the FFT task runs another.
        let (tx, rx) = mpsc::channel(1);
        let mut set = JoinSet::new();
        let (running, steps) = (
            StageRunning::new(self.stages.clone()),
            scan_loop(self.clone(), scan.clone(), tx),
        );
        set.spawn(async move {
            let _running = running;
            steps.await
        });
        let (running, ffts) = (
            StageRunning::new(self.stages.clone()),
            fft_task(self.clone(), scan, rx),
        );
        set.spawn(async move {
            let _running = running;
            ffts.await
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
        }
        Ok(())
    }

    /// The first `samples` of `source`'s ring at `center` after `after` (see
    /// [`crate::ring::IqRing::run_after`]), or `None` while it holds fewer.
    fn ring_run_after(
        &self,
        source: &SourceId,
        center: Hz,
        after: Option<Timestamp>,
        samples: usize,
    ) -> Option<IqBlock> {
        let ring = self.rings.lock().unwrap().get(source).cloned()?;
        let ring = ring.read().unwrap();
        ring.run_after(center, after, samples)
    }
}

/// What a running scan needs: its lease holder id, the grids its sweeps
/// alternate between, the dwell asked for (ms; each step takes it in whole
/// frames, [`scan_dwell_samples`]), the mask its `fft` runs apply (`notch_hz`
/// the notch in force), the `fft` stage, and its output stream.
struct Scan {
    id: JobId,
    port: Arc<dyn Port>,
    grid: ScanGrid,
    dwell_ms: u64,
    notch_hz: u64,
    edge_mask: f64,
    stage: Arc<dyn Stage>,
    ctx: StageContext,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
}

/// One step's dwell, handed from [`scan_loop`] to [`fft_task`]: the sweep and
/// the step's index within that sweep's list, the centre it was taken at, its
/// samples and how many FFT frames they are.
struct Step {
    #[expect(
        dead_code,
        reason = "a row's sweep is not published; kept for averaging by grid"
    )]
    sweep: u64,
    index: usize,
    center: Hz,
    block: IqBlock,
    frames: u32,
}

/// The `scan` job's step loop, until cancelled: tune the source to each step
/// of the sweep in turn (under the lease gate; even sweeps take the grid's
/// even steps, odd ones its odd steps), wait until the node's ring holds the
/// step's dwell of samples at the new centre ([`scan_dwell_samples`]; the
/// source delivers only settled samples after a tune, so the first ones at the
/// centre are taken) and hand exactly those to the FFT task. At a centre the
/// last step was also at (a one-step scan), the samples are the ones after
/// the last step's. When they have not arrived by the dwell plus
/// [`SCAN_WAIT_MARGIN_MS`] the step is skipped; samples lost inside a dwell
/// restart the ring's run and the wait goes on from there. A hand-off waits
/// while the FFT task still has one step queued.
/// Cancellation is checked before every tune and ends every wait. A failed
/// tune or a port fault fails the job; the port is left at the last step.
async fn scan_loop(
    inner: Arc<Inner>,
    s: Arc<Scan>,
    tx: mpsc::Sender<Step>,
) -> Result<(), StageError> {
    let faulted = |f: String| StageError::Failed(source_fault_reason(s.port.id(), &f));
    let source = &s.prov.source;
    let mut tick = inner.ring_tick(source);
    // The centre and the end of the last block taken.
    let mut taken: Option<(Hz, Timestamp)> = None;
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
        let size = spectrum_size(steps[i].bandwidth_hz.0);
        let (samples, dwell_ms) = scan_dwell_samples(s.dwell_ms, got.bandwidth_hz.0, size);
        let after = taken
            .filter(|(c, _)| *c == got.center_hz)
            .map(|(_, end)| end);
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(dwell_ms.saturating_add(SCAN_WAIT_MARGIN_MS));
        let block = tokio::select! {
            _ = s.cancel.cancelled() => return Err(StageError::Cancelled),
            b = wait_for_run(&inner, &mut tick, source, got.center_hz, after, samples, deadline) => b,
        };
        if let Some(f) = s.port.fault() {
            return Err(faulted(f));
        }
        if let Some(block) = block {
            taken = Some((got.center_hz, span_of(&block).end_ns));
            let step = Step {
                sweep,
                index: i,
                center: got.center_hz,
                block,
                frames: u32::try_from(samples / u64::from(size)).unwrap_or(u32::MAX),
            };
            tokio::select! {
                _ = s.cancel.cancelled() => return Err(StageError::Cancelled),
                // A closed channel: the FFT task ended, and its end is the job's.
                sent = tx.send(step) => if sent.is_err() {
                    return Err(StageError::Cancelled);
                },
            }
        }
        i += 1;
        if i == steps.len() {
            i = 0;
            sweep += 1;
        }
    }
}

/// The first `samples` of `source`'s ring at `center` after `after`, as soon
/// as the ring holds them: checked now and again at each ring tick, until
/// `deadline` (then `None`).
async fn wait_for_run(
    inner: &Inner,
    tick: &mut tokio::sync::watch::Receiver<u64>,
    source: &SourceId,
    center: Hz,
    after: Option<Timestamp>,
    samples: u64,
    deadline: tokio::time::Instant,
) -> Option<IqBlock> {
    let samples = usize::try_from(samples).ok()?;
    loop {
        // Marked seen before the ring is read, so a push in between is a change.
        tick.mark_unchanged();
        if let Some(block) = inner.ring_run_after(source, center, after, samples) {
            return Some(block);
        }
        match tokio::time::timeout_at(deadline, tick.changed()).await {
            Ok(Ok(())) => {}
            // The node keeps every tick's sender, so this is its end: nothing more comes.
            Ok(Err(_)) => {
                tokio::time::sleep_until(deadline).await;
                return None;
            }
            Err(_) => return None,
        }
    }
}

/// The span of signal time `block` covers.
fn span_of(block: &IqBlock) -> Span {
    let rate = block.sample_rate_hz.0.max(1) as u128;
    let ns = block.samples.len() as u128 * 1_000_000_000 / rate;
    Span {
        start_ns: block.t0,
        end_ns: Timestamp(block.t0.0 + ns as u64),
    }
}

/// The scan's FFT task: run [`fft_step`] over each step [`scan_loop`] hands
/// over, one at a time and in order, so the rows go out in step order. Ends
/// when cancelled or when the loop has ended (the channel closed).
async fn fft_task(
    inner: Arc<Inner>,
    s: Arc<Scan>,
    mut rx: mpsc::Receiver<Step>,
) -> Result<(), StageError> {
    loop {
        let step = tokio::select! {
            _ = s.cancel.cancelled() => return Err(StageError::Cancelled),
            r = rx.recv() => match r {
                Some(step) => step,
                // The loop ended, and its end is the job's.
                None => return Err(StageError::Cancelled),
            },
        };
        fft_step(&inner, &s, step).await?;
    }
}

/// `rows_per_s` of a row that is a whole dwell of `dwell_ms`:
/// `clamp(round(1000 / dwell_ms), 1, 60)`.
fn dwell_rows_per_s(dwell_ms: u64) -> u32 {
    let d = dwell_ms.max(1);
    ((1000 + d / 2) / d).clamp(1, 60) as u32
}

/// Run the scan's `fft` stage to completion over the step's block as a
/// one-shot IQ stream, then forward the rows it emitted to the scan's stream,
/// tagged with channel `scan/<step>` and `center_hz`. A dwell under
/// [`SCAN_ROWS_DWELL_MS`] is one row of all its frames (`row_frames`), whose
/// `rows_per_s` says `1000 / dwell_ms`; a longer one is cut into
/// `SPECTRUM_ROWS_PER_S` rows a second. Rows wait in the stage's output
/// stream (bus capacity) until the stage ends.
async fn fft_step(inner: &Inner, s: &Scan, step: Step) -> Result<(), StageError> {
    let Step {
        index,
        center,
        block,
        frames,
        ..
    } = step;
    // The dwell is `frames` whole frames.
    let size = block.samples.len() / frames.max(1) as usize;
    let rate = block.sample_rate_hz.0.max(1);
    let dwell_ms = (block.samples.len() as u64 * 1000 + rate / 2) / rate;
    let prov = Provenance {
        channel: Some(ChannelId(format!("scan/{index}"))),
        span: span_of(&block),
        ..s.prov.clone()
    };
    let bus = &inner.bus;
    let input = bus.register(StreamKind::Iq, prov.clone());
    let rx = bus.subscribe(&input).expect("registered");
    let output = bus.register_unfed(StreamKind::Event, prov.clone());
    let mut rows = bus.subscribe(&output).expect("registered");
    bus.publish(Frame::new(input.clone(), prov.clone(), Payload::Iq(block)));
    // Closing the input ends the stage once it has drained the block.
    bus.unregister(&input);
    let mut params = serde_json::json!({
        "size": size,
        "window": "hann",
        "rows_per_s": SPECTRUM_ROWS_PER_S,
        "notch_hz": s.notch_hz,
        "edge_mask": s.edge_mask,
    });
    if dwell_ms < SCAN_ROWS_DWELL_MS {
        params["row_frames"] = frames.into();
        params["rows_per_s"] = dwell_rows_per_s(dwell_ms).into();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dwell_row_says_its_rate() {
        assert_eq!(dwell_rows_per_s(20), 50);
        assert_eq!(dwell_rows_per_s(19), 53);
        assert_eq!(dwell_rows_per_s(341), 3);
        assert_eq!(dwell_rows_per_s(1), 60);
        assert_eq!(dwell_rows_per_s(0), 60);
        assert_eq!(dwell_rows_per_s(5000), 1);
    }
}
