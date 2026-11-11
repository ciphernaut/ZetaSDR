//! The `scan` job: time-shares one radio across a span wider than its capture.
//! A step loop ([`scan_loop`]) tunes the source to each step, waits on the ring
//! for the step's dwell of settled samples and hands them to one FFT task
//! ([`fft_task`]), which runs the `fft` stage over each step in order and
//! publishes its row on the scan's stream. There is no sleep: a step costs its
//! tune and the time its samples take to arrive. The look is the operator's: the
//! stored `fft` node's `dwell_ms` (the samples a step takes) and `average` (each
//! row the mean of its step's last sweeps, [`SweepAverage`]).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedMutexGuard, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::launch::{AfterGate, StageRunning, supervise};
use super::{Inner, JobOutput, NodeError, ring_seconds_for, source_fault_reason};
use crate::graph::{self, GraphError, NodeId};
use crate::job::JobRecord;
use crate::ops::{
    JobKind, SCAN_AVERAGE_MAX, SCAN_DWELL_MAX_MS, SCAN_ROWS_DWELL_MS, SCAN_WAIT_MARGIN_MS,
    SPECTRUM_ROWS_PER_S, ScanGrid, scan_dwell_within, scan_edge_mask_in_force, scan_grid,
    scan_mask_default, scan_notch_in_force, spectrum_size,
};
use crate::port::{IqBlock, Port, Tuning};
use crate::ring::IqRing;
use crate::spectrum::{STEP_DB, SweepAverage, dequantise, quantise_row, quantise_row_masked};
use crate::stage::{Stage, StageContext, StageError};
use crate::stream::{Frame, Payload};
use crate::types::{
    Capabilities, ChannelId, Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
};

impl Inner {
    /// Launch a `scan` job: take a time-shared lease on its whole band
    /// (`rec.tuning`), then run [`scan_loop`] under [`supervise`] with one
    /// `Event` output stream. The registered `fft` stage does the spectra; of
    /// the graph only the first `fft` node's `notch_hz` and `edge_mask` (its
    /// mask) and `dwell_ms` and `average` (its look: absent, the node's dwell
    /// and 1; out of range, clamped into it) are read. The first launch writes
    /// the mask into that node from the
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
        let caps = port.capabilities().clone();
        if !rec.graph.nodes.iter().any(|n| n.kind == "fft") {
            rec.graph = JobKind::Scan
                .expand(
                    &rec.source,
                    rec.tuning,
                    *caps.max_bandwidth_hz.value(),
                    None,
                    None,
                    None,
                    &self.modes(),
                )
                .map_err(|e| NodeError::Other(format!("scan graph: {e}")))?;
        }
        graph::validate(&rec.graph, &self.registry)?;
        let fft = rec
            .graph
            .nodes
            .iter_mut()
            .find(|n| n.kind == "fft")
            .expect("expanded above");
        if let Some(p) = fft.params.as_object_mut()
            && !p.contains_key("notch_hz")
        {
            let (notch, edge) = scan_mask_default(&caps);
            let w = scan_grid(rec.tuning, &caps).even[0].bandwidth_hz.0;
            p.insert("notch_hz".into(), notch.into());
            p.entry("edge_mask").or_insert(edge.into());
            p.insert("size".into(), spectrum_size(w).into());
        }
        // The stored node keeps what was configured; the rows and the context
        // get the values in force (see [`Layout`]).
        let mask = (
            fft.params["notch_hz"].as_u64().unwrap_or(0),
            fft.params["edge_mask"].as_f64().unwrap_or(0.0),
        );
        let layout = Layout::of(rec.tuning, &caps, mask);
        let look = Look {
            dwell_ms: fft.params["dwell_ms"]
                .as_u64()
                .map_or(self.scan_dwell_ms, |d| d.clamp(1, SCAN_DWELL_MAX_MS)),
            average: fft.params["average"]
                .as_u64()
                .map_or(1, |a| a.clamp(1, u64::from(SCAN_AVERAGE_MAX)) as u32),
        };
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
        let (id, span) = (rec.id.clone(), rec.tuning);
        // `job_context` tells a reader (the agent above all) how many steps a
        // sweep takes, how the odd sweeps shift and the mask in force, instead
        // of leaving it to guess from the rows.
        if !rec.context.is_object() {
            rec.context = serde_json::json!({});
        }
        if let Some(ctx) = rec.context.as_object_mut() {
            let ring = self.ring_capacity_samples(&rec.source, layout.rate_hz());
            ctx.insert("scan".into(), layout.context(look, ring).into());
        }
        let token = self.shutdown.child_token();
        let (prov, run, ended) = self.record_running(rec, &token)?;
        let out = self.bus.register_unfed(StreamKind::Event, prov.clone());
        let scan = Arc::new(Scan {
            id: id.clone(),
            span,
            layout,
            look,
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

    /// The most samples `source`'s ring holds at `rate_hz`: the ring's own
    /// capacity when it is at that rate, else what it will be sized to there.
    fn ring_capacity_samples(&self, source: &SourceId, rate_hz: u64) -> u64 {
        let ring = self.rings.lock().unwrap().get(source).cloned();
        if let Some(ring) = ring {
            let ring = ring.read().unwrap();
            if ring.sample_rate_hz().0 == rate_hz {
                return ring.capacity_samples() as u64;
            }
        }
        let seconds = ring_seconds_for(self.ring_budget_bytes, rate_hz, self.ring_seconds);
        IqRing::capacity_for(seconds, rate_hz) as u64
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

    /// Apply `f` to job `id`'s `job_context.scan` (an object, made when it is
    /// missing) and persist the record, all under one `jobs` lock: the sweep
    /// start's rewrite and the sweep's timing both write `scan`, and each reads
    /// what the other left, so neither loses the other's keys. Best effort: a
    /// job already gone has no context to keep.
    fn scan_context_update(
        &self,
        id: &JobId,
        f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(entry) = jobs.get_mut(id) else {
            return;
        };
        let ctx = &mut entry.record.context;
        if !ctx.is_object() {
            *ctx = serde_json::json!({});
        }
        let obj = ctx.as_object_mut().expect("context is an object");
        let mut scan = match obj.remove("scan") {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        f(&mut scan);
        obj.insert("scan".into(), scan.into());
        let _ = super::persist::persist(&self.jobs_dir, &entry.record);
    }

    /// Write `fields` into job `id`'s `job_context.scan`, keeping its other
    /// keys (a `dwell_ms_asked` the fields no longer carry is dropped).
    fn scan_context_rewrite(&self, id: &JobId, fields: serde_json::Map<String, serde_json::Value>) {
        self.scan_context_update(id, |scan| {
            scan.remove("dwell_ms_asked");
            scan.extend(fields);
        });
    }
}

/// The cost of one sweep's steps: the tune each took, the whole step, the
/// steps whose samples did not arrive and the time the hand-off waited on the
/// FFT task. [`SweepTiming::finish`] summarises and clears it. A sweep the
/// throttle does not write is [`SweepTiming::carry`]ed: its skips and slow tunes
/// stay counted for the next write, its durations are dropped.
#[derive(Default)]
struct SweepTiming {
    tunes: Vec<Duration>,
    steps: Vec<Duration>,
    skipped: u64,
    fft_wait: Duration,
    /// Slow tunes of the sweeps carried since the last write.
    slow: u64,
}

/// The median of `v` (its upper middle for an even count); zero when empty.
fn median(v: &[Duration]) -> Duration {
    let mut v = v.to_vec();
    v.sort();
    v.get(v.len() / 2).copied().unwrap_or_default()
}

/// `d` in milliseconds, to the microsecond.
fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e6).round() / 1e3
}

impl SweepTiming {
    /// One step's tune and its whole time (tune, wait for samples, hand-off),
    /// and whether its samples did not arrive.
    fn step(&mut self, tune: Duration, step: Duration, skipped: bool) {
        self.tunes.push(tune);
        self.steps.push(step);
        self.skipped += u64::from(skipped);
    }

    /// Time the hand-off waited for the FFT task to take a step.
    fn fft_wait(&mut self, d: Duration) {
        self.fft_wait += d;
    }

    /// Slow tunes of the sweep so far: over ten times its median tune,
    /// compared in microseconds.
    fn slow_tunes(&self) -> u64 {
        let med_us = median(&self.tunes).as_micros().max(1);
        self.tunes
            .iter()
            .filter(|d| d.as_micros() > 10 * med_us)
            .count() as u64
    }

    /// End a sweep that is not written: its skips and slow tunes are kept
    /// for the next write, the rest is dropped.
    fn carry(&mut self) {
        *self = SweepTiming {
            skipped: self.skipped,
            slow: self.slow + self.slow_tunes(),
            ..Default::default()
        };
    }

    /// The sweep's `context.scan.timing` (less `at_ns`, which the writer adds)
    /// for a sweep of `span_hz` that took `sweep`; the next sweep starts empty.
    /// `skipped` and `slow_tunes` count since the last write (this sweep and any
    /// carried before it); the other figures are this sweep's.
    fn finish(&mut self, span_hz: u64, sweep: Duration) -> serde_json::Value {
        let slow = self.slow_tunes() + self.slow;
        let t = std::mem::take(self);
        let med = median(&t.tunes);
        let sweep_ms = ms(sweep);
        let hz_per_s = if sweep_ms > 0.0 {
            span_hz as f64 * 1000.0 / sweep_ms
        } else {
            0.0
        };
        serde_json::json!({
            "sweep_ms": sweep_ms,
            "step_ms": ms(median(&t.steps)),
            "tune_ms": {
                "median": ms(med),
                "max": ms(t.tunes.iter().max().copied().unwrap_or_default()),
            },
            "slow_tunes": slow,
            "skipped": t.skipped,
            "fft_wait_ms": ms(t.fft_wait),
            "hz_per_s": hz_per_s,
        })
    }
}

/// The least time between two writes of `context.scan.timing`, ns.
const TIMING_EVERY_NS: u64 = 1_000_000_000;

/// A scan's steps and the mask they are masked with, built from the source's
/// capabilities at launch and again at the start of a sweep when the source's
/// capture (`max_bandwidth_hz`) or its usable fraction changed (a cap measured
/// after the scan started, [`scan_loop`]).
#[derive(Clone, Debug)]
struct Layout {
    /// The capabilities it was built from: `max_bandwidth_hz` and `usable_fraction`.
    capture_hz: u64,
    usable: f64,
    grid: ScanGrid,
    /// The notch and edge mask in force: no notch on a step under four notches,
    /// and on a grid that alternates the configured edge mask or the edges the
    /// usable fraction leaves unused, whichever is wider, else none (no odd
    /// sweep or neighbour fills the edges in).
    notch_hz: u64,
    edge_mask: f64,
    /// The configured `(notch_hz, edge_mask)`, from the scan's stored `fft` node.
    mask: (u64, f64),
}

impl Layout {
    fn of(span: Tuning, caps: &Capabilities, mask: (u64, f64)) -> Self {
        let grid = scan_grid(span, caps);
        let usable = *caps.usable_fraction.value();
        let w = grid.even[0].bandwidth_hz.0;
        Layout {
            capture_hz: caps.max_bandwidth_hz.value().0,
            usable,
            notch_hz: scan_notch_in_force(w, mask.0),
            edge_mask: scan_edge_mask_in_force(grid.alternates(), mask.1, usable),
            grid,
            mask,
        }
    }

    /// Whether `caps` would build another layout.
    fn stale(&self, caps: &Capabilities) -> bool {
        caps.max_bandwidth_hz.value().0 != self.capture_hz
            || *caps.usable_fraction.value() != self.usable
    }

    /// The capture each step asks for, Hz.
    fn rate_hz(&self) -> u64 {
        self.grid.even[0].bandwidth_hz.0
    }

    /// The scan's `job_context.scan` for this layout, `look` and a ring of
    /// `ring_samples` at the step's rate: `steps`, `step_hz` (the stride),
    /// `rate_hz` (the capture each step asks for), `dwell_ms` (in force on a
    /// step: whole frames, never under four, never more than the ring holds),
    /// `dwell_ms_asked` when that differs, `average`, `alternate` and `mask`.
    fn context(&self, look: Look, ring_samples: u64) -> serde_json::Map<String, serde_json::Value> {
        let g = &self.grid;
        let w = self.rate_hz();
        let dwell_asked_ms = look.dwell_ms;
        let (_, dwell_ms) = scan_dwell_within(dwell_asked_ms, w, spectrum_size(w), ring_samples);
        let alternate = if g.alternates() {
            serde_json::json!({"shift_hz": g.shift_hz(), "steps_shifted": g.odd.len()})
        } else {
            serde_json::Value::Null
        };
        let mut scan = serde_json::json!({
            "steps": g.even.len(),
            "step_hz": g.stride_hz,
            "rate_hz": w,
            "dwell_ms": dwell_ms,
            "average": look.average,
            "alternate": alternate,
            "mask": {"notch_hz": self.notch_hz, "edge_mask": self.edge_mask},
        });
        if dwell_ms != dwell_asked_ms {
            scan["dwell_ms_asked"] = dwell_asked_ms.into();
        }
        match scan {
            serde_json::Value::Object(m) => m,
            _ => unreachable!("built as an object"),
        }
    }
}

/// A scan's look, from its stored `fft` node: the dwell asked for (ms; each
/// step takes it in whole frames, [`scan_dwell_within`]) and how many sweeps
/// each row averages.
#[derive(Clone, Copy, Debug)]
struct Look {
    dwell_ms: u64,
    average: u32,
}

/// What a running scan needs: its lease holder id, its band, the layout it
/// launched with, its look, the `fft` stage, and its output stream.
struct Scan {
    id: JobId,
    port: Arc<dyn Port>,
    span: Tuning,
    layout: Layout,
    look: Look,
    stage: Arc<dyn Stage>,
    ctx: StageContext,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
}

/// One step's dwell, handed from [`scan_loop`] to [`fft_task`]: the sweep and
/// the step's index within that sweep's list, the layout it was stepped on (a
/// count of the layouts the scan has built, and whether its grid alternates),
/// the centre it was taken at, its samples, how many FFT frames they are and
/// the mask in force on it.
struct Step {
    sweep: u64,
    layout: u64,
    alternates: bool,
    index: usize,
    center: Hz,
    block: IqBlock,
    frames: u32,
    notch_hz: u64,
    edge_mask: f64,
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
/// At each sweep's start the source's capabilities are read again: when its
/// capture or usable fraction changed (a rate cap measured after the scan
/// started), the grid and mask are rebuilt from them and `job_context.scan`
/// rewritten, so that sweep steps at the new width.
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
    let mut layout = s.layout.clone();
    // How many times the layout was rebuilt: the FFT task starts its averages afresh.
    let mut generation = 0u64;
    // The centre and the end of the last block taken.
    let mut taken: Option<(Hz, Timestamp)> = None;
    let (mut sweep, mut i) = (0u64, 0usize);
    let mut timing = SweepTiming::default();
    let mut sweep_start = tokio::time::Instant::now();
    // When the timing was last written (ns since the epoch): at most once a second.
    let mut written_ns = 0u64;
    loop {
        // Checked before each tune, so a stopped scan never retunes again; a
        // tune is never abandoned half way, so it is checked after it too.
        if s.cancel.is_cancelled() {
            return Err(StageError::Cancelled);
        }
        if i == 0 {
            sweep_start = tokio::time::Instant::now();
        }
        let step_start = tokio::time::Instant::now();
        if i == 0 && layout.stale(s.port.capabilities()) {
            layout = Layout::of(s.span, s.port.capabilities(), layout.mask);
            generation += 1;
            let ring = inner.ring_capacity_samples(source, layout.rate_hz());
            inner.scan_context_rewrite(&s.id, layout.context(s.look, ring));
        }
        let steps = layout.grid.steps(sweep);
        let tune_start = tokio::time::Instant::now();
        let got = match inner
            .leases
            .retune_shared(s.port.as_ref(), &s.id, steps[i], &s.cancel)
            .await
        {
            Ok(Some(t)) => t,
            Ok(None) => return Err(StageError::Cancelled),
            Err(e) => return Err(faulted(e.to_string())),
        };
        let tune = tune_start.elapsed();
        let size = spectrum_size(steps[i].bandwidth_hz.0);
        let ring = inner.ring_capacity_samples(source, got.bandwidth_hz.0);
        let (samples, dwell_ms) =
            scan_dwell_within(s.look.dwell_ms, got.bandwidth_hz.0, size, ring);
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
        let skipped = block.is_none();
        if let Some(block) = block {
            taken = Some((got.center_hz, span_of(&block).end_ns));
            let step = Step {
                sweep,
                layout: generation,
                alternates: layout.grid.alternates(),
                index: i,
                center: got.center_hz,
                block,
                frames: u32::try_from(samples / u64::from(size)).unwrap_or(u32::MAX),
                notch_hz: layout.notch_hz,
                edge_mask: layout.edge_mask,
            };
            let waited = tokio::time::Instant::now();
            tokio::select! {
                _ = s.cancel.cancelled() => return Err(StageError::Cancelled),
                // A closed channel: the FFT task ended, and its end is the job's.
                sent = tx.send(step) => if sent.is_err() {
                    return Err(StageError::Cancelled);
                },
            }
            timing.fft_wait(waited.elapsed());
        }
        timing.step(tune, step_start.elapsed(), skipped);
        i += 1;
        if i == steps.len() {
            i = 0;
            sweep += 1;
            let at_ns = super::persist::now().0;
            if at_ns.saturating_sub(written_ns) >= TIMING_EVERY_NS {
                let mut t = timing.finish(s.span.bandwidth_hz.0, sweep_start.elapsed());
                t["at_ns"] = at_ns.into();
                written_ns = at_ns;
                inner.scan_context_update(&s.id, |scan| {
                    scan.insert("timing".into(), t);
                });
            } else {
                timing.carry();
            }
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
/// when cancelled or when the loop has ended (the channel closed). With an
/// `average` over 1 it keeps the averages: one per grid (even and odd sweeps
/// apart), started afresh when the layout changes.
async fn fft_task(
    inner: Arc<Inner>,
    s: Arc<Scan>,
    mut rx: mpsc::Receiver<Step>,
) -> Result<(), StageError> {
    let fresh = || [0, 1].map(|_| SweepAverage::new(s.look.average));
    let mut averages = (0u64, fresh());
    loop {
        let step = tokio::select! {
            _ = s.cancel.cancelled() => return Err(StageError::Cancelled),
            r = rx.recv() => match r {
                Some(step) => step,
                // The loop ended, and its end is the job's.
                None => return Err(StageError::Cancelled),
            },
        };
        if step.layout != averages.0 {
            averages = (step.layout, fresh());
        }
        let grid = usize::from(step.alternates && step.sweep % 2 == 1);
        let avg = (s.look.average > 1).then(|| &mut averages.1[grid]);
        fft_step(&inner, &s, step, avg).await?;
    }
}

/// A row's `masked` ranges (`[]` when it has none).
fn row_masked(masked: Option<&serde_json::Value>) -> Vec<[usize; 2]> {
    masked
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|r| Some([r[0].as_u64()? as usize, r[1].as_u64()? as usize]))
                .collect()
        })
        .unwrap_or_default()
}

/// Average row `o` (the `row`th of its step at `center`) into `avg` and give it
/// the mean: `floor_db` and `q` requantised from the mean over its `masked`, and
/// `sweeps`, how many rows were averaged. A row with no `q` or `floor_db` passes
/// as it is, with `sweeps: 1`; one at another `step_db` is requantised first.
fn average_row(
    avg: &mut SweepAverage,
    center: Hz,
    row: u32,
    o: &mut serde_json::Map<String, serde_json::Value>,
) {
    let q: Option<Vec<u8>> = o.get("q").and_then(|q| q.as_array()).and_then(|a| {
        a.iter()
            .map(|b| b.as_u64().map(|b| b.min(255) as u8))
            .collect()
    });
    let (Some(q), Some(floor)) = (q, o.get("floor_db").and_then(|f| f.as_f64())) else {
        o.insert("sweeps".into(), 1.into());
        return;
    };
    let step_db = o
        .get("step_db")
        .and_then(|f| f.as_f64())
        .unwrap_or(f64::from(STEP_DB)) as f32;
    let (floor, q) = if step_db == STEP_DB {
        (floor as f32, q)
    } else {
        quantise_row(&dequantise(floor as f32, step_db, &q))
    };
    let (db, sweeps) = avg.push((center.0, row), floor, &q);
    let masked = row_masked(o.get("masked"));
    let (floor, q) = quantise_row_masked(&db, &masked);
    o.insert("floor_db".into(), floor.into());
    o.insert("step_db".into(), STEP_DB.into());
    o.insert("q".into(), q.into());
    o.insert("sweeps".into(), sweeps.into());
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
/// stream (bus capacity) until the stage ends. Each row carries `sweeps`:
/// with `avg` (an `average` over 1) it is averaged into it ([`average_row`]),
/// else it passes as the stage made it with `sweeps: 1`.
async fn fft_step(
    inner: &Inner,
    s: &Scan,
    step: Step,
    mut avg: Option<&mut SweepAverage>,
) -> Result<(), StageError> {
    let Step {
        index,
        center,
        block,
        frames,
        notch_hz,
        edge_mask,
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
        "notch_hz": notch_hz,
        "edge_mask": edge_mask,
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
    let mut row = 0u32;
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
            match avg.as_deref_mut() {
                Some(avg) => average_row(avg, center, row, o),
                None => {
                    o.insert("sweeps".into(), 1.into());
                }
            }
            row += 1;
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

    fn ms_of(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_summary_takes_medians_and_counts_slow_tunes() {
        let mut t = SweepTiming::default();
        for (tune, step, skipped) in [
            (3, 25, false),
            (3, 25, false),
            (3, 30, true),
            (150, 180, false),
            (3, 25, false),
        ] {
            t.step(ms_of(tune), ms_of(step), skipped);
        }
        t.fft_wait(ms_of(4));
        t.fft_wait(ms_of(6));
        let v = t.finish(40_000_000, ms_of(285));
        assert_eq!(v["tune_ms"]["median"], 3.0, "{v}");
        assert_eq!(v["tune_ms"]["max"], 150.0, "{v}");
        assert_eq!(v["slow_tunes"], 1, "{v}");
        assert_eq!(v["step_ms"], 25.0, "{v}");
        assert_eq!(v["skipped"], 1, "{v}");
        assert_eq!(v["fft_wait_ms"], 10.0, "{v}");
        assert_eq!(v["sweep_ms"], 285.0, "{v}");
        // The summary cleared the sweep.
        let v = t.finish(1, ms_of(1));
        assert_eq!(
            (v["skipped"].as_u64(), v["slow_tunes"].as_u64()),
            (Some(0), Some(0))
        );
    }

    #[test]
    fn hz_per_s_is_the_span_over_the_sweep() {
        let mut t = SweepTiming::default();
        t.step(ms_of(1), ms_of(2), false);
        // 40 MHz in 250 ms is 160 MHz a second.
        let v = t.finish(40_000_000, ms_of(250));
        assert_eq!(v["hz_per_s"], 160_000_000.0, "{v}");
    }

    /// The sweep start's rewrite and a timing write each read what the other
    /// left: hammered from two threads, the last write of each survives.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sweep_start_rewrite_and_a_timing_write_cannot_lose_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = super::super::NodeConfig {
            source_positions: Default::default(),
            burst_policy: Default::default(),
            state_dir: dir.path().join("state"),
            ring_seconds: 1.0,
            ring_budget_bytes: u64::MAX,
            scan_dwell_ms: 20,
            backends: vec![crate::stage::Backend::Cpu],
            resume: false,
            bank_factory: None,
            modes_dir: None,
            sets_dir: None,
        };
        let node = super::super::Node::open(cfg, crate::stage::StageRegistry::new(), vec![])
            .await
            .unwrap();
        let inner = node.inner.clone();
        let id = JobId::new();
        let rec = JobRecord {
            format: crate::job::JOB_RECORD_FORMAT,
            id: id.clone(),
            kind: "scan".into(),
            graph: graph::Graph {
                id: graph::GraphId("g".into()),
                nodes: vec![],
                edges: vec![],
            },
            source: SourceId("a".into()),
            tuning: Tuning {
                center_hz: Hz(20_000_000),
                bandwidth_hz: Hz(40_000_000),
            },
            state: crate::job::JobState::Running,
            context: serde_json::json!({"note": "kept"}),
            created_ns: Timestamp(0),
            input: None,
            capture_rate_hz: None,
            elastic: false,
            operator_graph: false,
            owner: None,
        };
        inner.jobs.lock().unwrap().insert(
            id.clone(),
            super::super::JobEntry::loaded(rec, Timestamp(0)),
        );
        const N: u64 = 300;
        // How many rewrites of `steps` the first thread has finished.
        let done = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let a = {
            let (inner, id, done) = (inner.clone(), id.clone(), done.clone());
            std::thread::spawn(move || {
                for n in 0..N {
                    let mut f = serde_json::Map::new();
                    f.insert("steps".into(), n.into());
                    inner.scan_context_rewrite(&id, f);
                    done.store(n + 1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        };
        let b = {
            let (inner, id, done) = (inner.clone(), id.clone(), done.clone());
            std::thread::spawn(move || {
                for n in 0..N {
                    let finished = done.load(std::sync::atomic::Ordering::SeqCst);
                    inner.scan_context_update(&id, |scan| {
                        // Neither writer's earlier write may have been reverted.
                        if n > 0 {
                            assert_eq!(scan["timing"]["at_ns"], n - 1, "a timing write was lost");
                        }
                        if finished > 0 {
                            let steps = scan["steps"].as_u64().expect("steps");
                            assert!(steps >= finished - 1, "a rewrite was lost: {steps}");
                        }
                        scan.insert("timing".into(), serde_json::json!({"at_ns": n}));
                    });
                }
            })
        };
        a.join().unwrap();
        b.join().unwrap();
        let ctx = node.job_context(&id).unwrap();
        assert_eq!(ctx["scan"]["steps"], N - 1, "{ctx}");
        assert_eq!(ctx["scan"]["timing"]["at_ns"], N - 1, "{ctx}");
        assert_eq!(ctx["note"], "kept", "{ctx}");
    }
}
