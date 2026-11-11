//! The `scan` job: time-shares one radio across a span wider than its capture.
//! A step loop ([`scan_loop`]) tunes the source to each step, waits on the ring
//! for the step's dwell of settled samples and hands them to one FFT task
//! ([`fft_task`]), which runs the `fft` stage over each step in order and
//! publishes its row on the scan's stream. There is no sleep: a step costs its
//! tune and the time its samples take to arrive. The look is the operator's: the
//! stored `fft` node's `dwell_ms` (the samples a step takes) and `average` (each
//! row the mean of its step's last sweeps, [`SweepAverage`]) and `bits` (the
//! sample depth the source's link runs at for the scan: held for the run and
//! put back before the lease goes, [`scan_run`]). A source that sweeps the span
//! for itself is asked to instead ([`sweep_loop`]): its rows are forwarded in
//! the same row shape, under a sweep claim and no lease (`method: source`).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedMutexGuard, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::launch::{AfterGate, StageRunning, supervise};
use super::{Inner, JobOutput, NodeError, ring_seconds_for, source_fault_reason};
use crate::graph::{self, GraphError, NodeId};
use crate::job::JobRecord;
use crate::lease::LeaseError;
use crate::ops::{
    JobKind, SCAN_AVERAGE_MAX, SCAN_BIN_HZ_STEPPED, SCAN_BIN_MAX_HZ, SCAN_BITS_STEPPED,
    SCAN_DWELL_MAX_MS, SCAN_ROWS_DWELL_MS, SCAN_WAIT_MARGIN_MS, SPECTRUM_BIN_TARGET_HZ,
    SPECTRUM_ROWS_PER_S, SWEEP_RETRY_MS, ScanGrid, ScanMethod, scan_average_budget_bytes,
    scan_average_in_force, scan_bits_refusal, scan_capture_hz, scan_dwell_within,
    scan_edge_mask_in_force, scan_grid_at, scan_mask_default, scan_notch_in_force,
    scan_rows_per_step, scan_sweep_refusal, spectrum_size,
};
use crate::port::{
    IqBlock, Port, PortError, SweepPlan, SweepRequest, SweepRow, SweepRows, SweepState, Tuning,
};
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
    /// and 1; out of range, clamped into it) and `bits` (a depth the source
    /// does not carry fails the launch, before the lease; absent, the source's
    /// own; [`scan_run`] holds the source at it) are read. The first launch writes
    /// the mask into that node from the
    /// source's capabilities ([`scan_mask_default`]); a resume or a `graph put`
    /// finds them and keeps them. A record with no `fft` node (persisted before
    /// the scan carried a mask) gets the kind's expansion first. The graph is
    /// validated before the lease, so a bad stored mask fails the launch there.
    /// The elastic jobs on the source are stopped: first, or with `gate` (held
    /// by the caller, see [`Inner::launch_gated`]) once the lease is granted,
    /// through `after`.
    ///
    /// Before any of that, a scan that is not made to step (the node's `method`
    /// `stepped`) and asks no `bits`, on a source whose `sweep` holds its band,
    /// asks the source to sweep it ([`Inner::start_sweep`]) for the node's
    /// `bin_hz` (absent: `SPECTRUM_BIN_TARGET_HZ` or the source's finest,
    /// whichever is coarser) and the look's dwell; granted, it runs as
    /// [`Inner::launch_self_sweep`] (no lease, nobody stopped, `after` left
    /// alone). The ask and what a refusal leads to are one hold of the source's
    /// gate (`gate`, else taken for the launch). Refused (or the span outside the
    /// sweep): a scan whose record says it ran as `source` (a resume, a restart
    /// under its id), whatever its stored `method`, waits for its sweep, asking
    /// again, never stepping; else with `method: source` the launch fails (`sweep
    /// on source <id>: <reason>`, or the span named); any other steps, its
    /// `job_context.scan.fallback` the source's reason, a conflict for its lease
    /// gaining it as an option.
    ///
    /// The holders `replace` names (a `job_start`'s; `gate` is held whenever it
    /// names any) are stopped under `gate` right before the stepped scan's lease,
    /// and only then: a sweep the source grants needs no room, so they run on.
    pub(super) async fn launch_scan(
        self: &Arc<Self>,
        rec: JobRecord,
        port: Arc<dyn Port>,
        gate: Option<&OwnedMutexGuard<()>>,
        replace: &[JobId],
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
        let bits = check_stored_bits(port.as_ref(), &rec.graph)?;
        let capture = capture_of(port.as_ref(), bits);
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
            let w = scan_grid_at(rec.tuning, &caps, capture).even[0]
                .bandwidth_hz
                .0;
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
        let layout = Layout::of(
            rec.tuning,
            &caps,
            capture,
            bits_in_force(port.as_ref(), bits),
            mask,
        );
        let look = Look {
            dwell_ms: fft.params["dwell_ms"]
                .as_u64()
                .map_or(self.scan_dwell_ms, |d| d.clamp(1, SCAN_DWELL_MAX_MS)),
            average: fft.params["average"]
                .as_u64()
                .map_or(1, |a| a.clamp(1, u64::from(SCAN_AVERAGE_MAX)) as u32),
        };
        let method = stored_method(&fft.params)?;
        let bin_hz = fft.params["bin_hz"]
            .as_u64()
            .map(|b| b.clamp(1, SCAN_BIN_MAX_HZ));
        // Read before this launch writes `scan`: a scan that ran as the source's own
        // sweep (resumed, or restarted under its id) stays one.
        let was_source = rec.context.pointer("/scan/method").and_then(|m| m.as_str())
            == Some(ScanMethod::Source.name());
        // A source that sweeps the span for itself is asked first, unless the scan is
        // made to step (`method: stepped`) or asks for IQ (`bits`): no lease, no stop
        // of the elastic jobs. The whole start decision (the sweep asked, then on a
        // refusal the stepped scan's lease) is one hold of the source's gate. A scan
        // on a source that cannot sweep, neither made `source` nor run as one, steps
        // as before without asking.
        let asks = method != Some(ScanMethod::Stepped)
            && bits.is_none()
            && (caps.sweep.is_some() || method == Some(ScanMethod::Source) || was_source);
        let own;
        let gate = match gate {
            Some(g) => Some(g),
            None if asks => {
                own = self.leases.gate_guard(port.id()).await;
                Some(&own)
            }
            None => None,
        };
        // Why the source refused at a first start, which steps instead.
        let mut fallback = None;
        if let (true, Some(g)) = (asks, gate) {
            let calibrated = caps.sweep.as_ref().is_some_and(|s| s.calibrated);
            let req = SweepRequest {
                span: rec.tuning,
                bin_hz: Hz(bin_hz.unwrap_or_else(|| {
                    caps.sweep
                        .as_ref()
                        .map_or(SPECTRUM_BIN_TARGET_HZ, |s| s.bin_hz_min.0)
                        .max(SPECTRUM_BIN_TARGET_HZ)
                })),
                dwell_ms: look.dwell_ms,
            };
            let unheld = scan_sweep_refusal(port.id(), &caps, rec.tuning);
            // A first start made `source` fails on a refusal; a restart of a scan
            // that ran as one waits below, whatever its stored method.
            let fails = method == Some(ScanMethod::Source) && !was_source;
            let refused = match &unheld {
                Some(why) if fails => {
                    return Err(NodeError::Other(why.clone()));
                }
                Some(why) => why.clone(),
                None => match self.start_sweep(port.as_ref(), &rec.id, req, g).await {
                    Ok(rows) => {
                        let start = SweepStart::Rows(rows);
                        return self.launch_self_sweep(rec, port, look, calibrated, start, req);
                    }
                    Err(e) if fails => {
                        return Err(NodeError::Other(format!(
                            "sweep on source {}: {}",
                            rec.source.0,
                            refusal_reason(&e)
                        )));
                    }
                    Err(e) => refusal_reason(&e),
                },
            };
            // A restart of a scan that ran as `source` never steps (it would take
            // the radio from the jobs beside it): it waits for its sweep instead.
            if was_source {
                self.leases.claim_sweep_locked(port.id(), rec.id.clone(), g);
                let start = SweepStart::Waiting(refused);
                return self.launch_self_sweep(rec, port, look, calibrated, start, req);
            }
            // A first start steps; a span outside the sweep is no refusal to report.
            if unheld.is_none() {
                fallback = Some(refused);
            }
        }
        match gate {
            Some(g) => {
                // The stepped scan needs the radio: the holders `replace` names stop
                // now, under the same hold as the lease.
                if !replace.is_empty() {
                    self.replace_holders(&rec.source, replace, g).await?;
                }
                self.leases
                    .acquire_shared_locked(port.as_ref(), rec.id.clone(), rec.tuning, g)
                    .map_err(|e| with_fallback(e, fallback.as_deref()))?;
                *after = AfterGate::StopElastic(rec.source.clone());
            }
            None => {
                debug_assert!(replace.is_empty(), "a replace is stopped under the gate");
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
            let budget = scan_average_budget_bytes(self.ring_budget_bytes);
            let mut scan = layout.context(look, ring, budget);
            if let Some(why) = fallback {
                scan.insert("fallback".into(), why.into());
            }
            ctx.insert("scan".into(), scan.into());
        }
        let token = self.shutdown.child_token();
        let (prov, run, ended) = self.record_running(rec, &token)?;
        let out = self.bus.register_unfed(StreamKind::Event, prov.clone());
        let scan = Arc::new(Scan {
            id: id.clone(),
            span,
            layout,
            look,
            bits,
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
            scan_run(self.clone(), scan.clone(), tx),
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

    /// Take job `id`'s sweep claim on `port` under its gate (`gate`, held), then ask
    /// the port to sweep `req`: its rows, or its refusal, the claim then dropped
    /// unless a live run of the id holds it. Taken before the call and under the
    /// gate, the claim is never taken after the job's release, and an idle never
    /// lands between it and the sweep's start. A call that has not returned after
    /// [`crate::ops::SWEEP_START_TIMEOUT_MS`] is dropped (ending the source's attempt) and
    /// read as a refusal, `sweep start timed out`: the gate is never held longer.
    async fn start_sweep(
        &self,
        port: &dyn Port,
        id: &JobId,
        req: SweepRequest,
        gate: &OwnedMutexGuard<()>,
    ) -> Result<SweepRows, PortError> {
        self.leases.claim_sweep_locked(port.id(), id.clone(), gate);
        let wait = *self.sweep_start_timeout.lock().unwrap();
        let rows = match tokio::time::timeout(wait, port.sweep(req)).await {
            Ok(rows) => rows,
            Err(_) => Err(PortError::Io("sweep start timed out".into())),
        };
        if rows.is_err() {
            self.release_unless_live(id);
        }
        rows
    }

    /// Run a scan the source sweeps for itself: record it running with
    /// [`sweep_task`] as its one task under [`supervise`] (handed the port, so its
    /// claim is released and the port idled at the end), `blind` (it reads no
    /// fault, so a heal does not wait for it), with `job_context.scan` from the
    /// source's plan ([`sweep_context`]), or, `start` waiting (a restart the
    /// source refused, the claim already taken), [`waiting_context`] until a
    /// sweep asked again as `req` is granted. It holds no lease and stops nobody.
    fn launch_self_sweep(
        self: &Arc<Self>,
        mut rec: JobRecord,
        port: Arc<dyn Port>,
        look: Look,
        calibrated: bool,
        start: SweepStart,
        req: SweepRequest,
    ) -> Result<(), NodeError> {
        let budget = scan_average_budget_bytes(self.ring_budget_bytes);
        if !rec.context.is_object() {
            rec.context = serde_json::json!({});
        }
        if let Some(ctx) = rec.context.as_object_mut() {
            let scan = match &start {
                SweepStart::Rows(rows) => {
                    let average = sweep_average(look, budget, &rows.plan);
                    sweep_context(&rows.plan, look, average, calibrated)
                }
                SweepStart::Waiting(why) => waiting_context(calibrated, why),
            };
            ctx.insert("scan".into(), scan.into());
        }
        let (id, span) = (rec.id.clone(), rec.tuning);
        let token = self.shutdown.child_token();
        let (prov, run, ended) = self.record_running(rec, &token)?;
        let out = self.bus.register_unfed(StreamKind::Event, prov.clone());
        let sweep = SelfSweep {
            id: id.clone(),
            span,
            look,
            budget,
            calibrated,
            port: port.clone(),
            req,
            prov,
            out: out.clone(),
            cancel: token.clone(),
        };
        let mut set = JoinSet::new();
        let (running, task) = (
            StageRunning::new(self.stages.clone()),
            sweep_task(self.clone(), sweep, start),
        );
        set.spawn(async move {
            let _running = running;
            task.await
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
            e.blind = true;
        }
        Ok(())
    }

    /// Refuse `graph` as the graph of a scan of `span` on `source` when its stored
    /// `fft` node asks what `job_start` refuses: `bits` its source does not carry,
    /// a `method` other than `stepped` or `source`, `method: "source"` on a span
    /// the source's sweep does not hold, a `bin_hz` with `method: "stepped"`, or a
    /// `bits` with `method: "source"` or a `bin_hz`;
    /// with `job_start`'s texts, so a `graph_put` leaves the running scan as it was.
    pub(super) fn check_scan_graph(
        &self,
        kind: &str,
        source: &SourceId,
        span: Tuning,
        graph: &graph::Graph,
    ) -> Result<(), NodeError> {
        if kind != JobKind::Scan.name() {
            return Ok(());
        }
        let Some(port) = self.ports.read().unwrap().get(source).cloned() else {
            return Ok(());
        };
        check_stored_bits(port.as_ref(), graph)?;
        let Some(params) = graph
            .nodes
            .iter()
            .find(|n| n.kind == "fft")
            .map(|n| &n.params)
        else {
            return Ok(());
        };
        let method = stored_method(params)?;
        if method == Some(ScanMethod::Source)
            && let Some(why) = scan_sweep_refusal(port.id(), port.capabilities(), span)
        {
            return Err(NodeError::Other(why));
        }
        let stored = |k: &str| params.get(k).is_some_and(|v| !v.is_null());
        if method == Some(ScanMethod::Stepped) && stored("bin_hz") {
            return Err(NodeError::Other(SCAN_BIN_HZ_STEPPED.into()));
        }
        if stored("bits") && (method == Some(ScanMethod::Source) || stored("bin_hz")) {
            return Err(NodeError::Other(SCAN_BITS_STEPPED.into()));
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
    /// keys (a `dwell_ms_asked`, `average_asked` or `bits` the fields no longer
    /// carry is dropped).
    fn scan_context_rewrite(&self, id: &JobId, fields: serde_json::Map<String, serde_json::Value>) {
        self.scan_context_update(id, |scan| {
            scan.remove("dwell_ms_asked");
            scan.remove("average_asked");
            scan.remove("bits");
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

/// The median of `v` (its lower middle for an even count, so one slow tune of
/// two counts as slow); zero when empty.
fn median(v: &[Duration]) -> Duration {
    let mut v = v.to_vec();
    v.sort();
    v.get(v.len().saturating_sub(1) / 2)
        .copied()
        .unwrap_or_default()
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
/// capture ([`capture_of`]: the cap of the depth in force, or the record's
/// figure), its usable fraction or its depth in force changed (a cap
/// measured after the scan started, [`scan_loop`]).
#[derive(Clone, Debug)]
struct Layout {
    /// What it was built from: the capture, `usable_fraction` and the depth in
    /// force ([`bits_in_force`]; `None` on a source without depths).
    capture_hz: u64,
    usable: f64,
    bits: Option<u8>,
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
    fn of(
        span: Tuning,
        caps: &Capabilities,
        capture_hz: u64,
        bits: Option<u8>,
        mask: (u64, f64),
    ) -> Self {
        let grid = scan_grid_at(span, caps, capture_hz);
        let usable = *caps.usable_fraction.value();
        let w = grid.even[0].bandwidth_hz.0;
        Layout {
            capture_hz,
            usable,
            bits,
            notch_hz: scan_notch_in_force(w, mask.0),
            edge_mask: scan_edge_mask_in_force(grid.alternates(), mask.1, usable),
            grid,
            mask,
        }
    }

    /// Whether `caps`, a capture of `capture_hz` and the depth `bits` would
    /// build another layout.
    fn stale(&self, caps: &Capabilities, capture_hz: u64, bits: Option<u8>) -> bool {
        capture_hz != self.capture_hz
            || *caps.usable_fraction.value() != self.usable
            || bits != self.bits
    }

    /// The capture each step asks for, Hz.
    fn rate_hz(&self) -> u64 {
        self.grid.even[0].bandwidth_hz.0
    }

    /// The `average` in force for `look` on this layout, with a ring of
    /// `ring_samples` at the step's rate and `budget_bytes` for the averages
    /// ([`scan_average_in_force`]): the grid's steps (both grids' when it
    /// alternates), the rows a step of the dwell in force and the step's bins.
    fn average_in_force(&self, look: Look, ring_samples: u64, budget_bytes: u64) -> u32 {
        let w = self.rate_hz();
        let size = spectrum_size(w);
        let (samples, dwell_ms) = scan_dwell_within(look.dwell_ms, w, size, ring_samples);
        let g = &self.grid;
        let steps = match g.alternates() {
            true => g.even.len() + g.odd.len(),
            false => g.even.len(),
        };
        scan_average_in_force(
            look.average,
            budget_bytes,
            steps as u64,
            scan_rows_per_step(samples, dwell_ms, w),
            u64::from(size),
        )
    }

    /// The scan's `job_context.scan` for this layout, `look`, a ring of
    /// `ring_samples` at the step's rate and `budget_bytes` for the averages:
    /// `method` (`stepped`), `steps`, `step_hz` (the stride), `rate_hz` (the
    /// capture each step asks for), `dwell_ms` (in force on a step: whole frames,
    /// never under four, never more than the ring holds), `dwell_ms_asked` when
    /// that differs, `average` (in force, [`Layout::average_in_force`]),
    /// `average_asked` when that differs, `bits` (the depth in force, on a
    /// source with depths), `alternate` and `mask`.
    fn context(
        &self,
        look: Look,
        ring_samples: u64,
        budget_bytes: u64,
    ) -> serde_json::Map<String, serde_json::Value> {
        let g = &self.grid;
        let w = self.rate_hz();
        let dwell_asked_ms = look.dwell_ms;
        let (_, dwell_ms) = scan_dwell_within(dwell_asked_ms, w, spectrum_size(w), ring_samples);
        let average = self.average_in_force(look, ring_samples, budget_bytes);
        let alternate = if g.alternates() {
            serde_json::json!({"shift_hz": g.shift_hz(), "steps_shifted": g.odd.len()})
        } else {
            serde_json::Value::Null
        };
        let mut scan = serde_json::json!({
            "method": "stepped",
            "steps": g.even.len(),
            "step_hz": g.stride_hz,
            "rate_hz": w,
            "dwell_ms": dwell_ms,
            "average": average,
            "alternate": alternate,
            "mask": {"notch_hz": self.notch_hz, "edge_mask": self.edge_mask},
        });
        if dwell_ms != dwell_asked_ms {
            scan["dwell_ms_asked"] = dwell_asked_ms.into();
        }
        if average != look.average {
            scan["average_asked"] = look.average.into();
        }
        if let Some(b) = self.bits {
            scan["bits"] = b.into();
        }
        match scan {
            serde_json::Value::Object(m) => m,
            _ => unreachable!("built as an object"),
        }
    }
}

/// A scan's look, from its stored `fft` node: the dwell asked for (ms; each
/// step takes it in whole frames, [`scan_dwell_within`]) and how many sweeps
/// each row is asked to average (at most what the averaging budget holds,
/// [`Layout::average_in_force`]).
#[derive(Clone, Copy, Debug)]
struct Look {
    dwell_ms: u64,
    average: u32,
}

/// What a running scan needs: its lease holder id, its band, the layout it
/// launched with, its look, the depth it asked for, the `fft` stage, and its
/// output stream.
struct Scan {
    id: JobId,
    port: Arc<dyn Port>,
    span: Tuning,
    layout: Layout,
    look: Look,
    /// The sample depth asked for (`None`: the source's own).
    bits: Option<u8>,
    stage: Arc<dyn Stage>,
    ctx: StageContext,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
}

/// One step's dwell, handed from [`scan_loop`] to [`fft_task`]: the sweep and
/// the step's index within that sweep's list, the layout it was stepped on (a
/// count of the layouts the scan has built, whether its grid alternates and
/// the `average` in force on it), the centre it was taken at, its samples, how
/// many FFT frames they are, the mask in force on it and the depth it was
/// taken at (`None` on a source without depths).
struct Step {
    sweep: u64,
    layout: u64,
    alternates: bool,
    average: u32,
    bits: Option<u8>,
    index: usize,
    center: Hz,
    block: IqBlock,
    frames: u32,
    notch_hz: u64,
    edge_mask: f64,
}

/// The `bits` stored in a scan graph's first `fft` node, checked against
/// `port`'s depths ([`scan_bits_refusal`]): `None` when absent or `null`.
fn check_stored_bits(port: &dyn Port, graph: &graph::Graph) -> Result<Option<u8>, NodeError> {
    let Some(v) = graph
        .nodes
        .iter()
        .find(|n| n.kind == "fft")
        .and_then(|n| n.params.get("bits"))
        .and_then(|b| b.as_u64())
    else {
        return Ok(None);
    };
    match scan_bits_refusal(port.id(), port.capabilities(), v) {
        Some(why) => Err(NodeError::Other(why)),
        None => Ok(u8::try_from(v).ok()),
    }
}

/// Whether the scan `rec` launches without taking its source's capture, whatever the
/// source answers: no `bits` stored, and `method: "source"` stored, or none stored
/// and its record saying it ran as `source` (a refusal then fails it or makes it
/// wait, [`Inner::launch_scan`]; it never steps).
pub(super) fn never_steps(rec: &JobRecord) -> bool {
    let params = rec
        .graph
        .nodes
        .iter()
        .find(|n| n.kind == "fft")
        .map(|n| &n.params);
    let stored = |k: &str| params.and_then(|p| p.get(k)).filter(|v| !v.is_null());
    let ran_as_source = rec.context.pointer("/scan/method").and_then(|m| m.as_str())
        == Some(ScanMethod::Source.name());
    let method = params.map(stored_method).and_then(Result::ok).flatten();
    stored("bits").is_none()
        && match method {
            Some(m) => m == ScanMethod::Source,
            None => ran_as_source && stored("method").is_none(),
        }
}

/// The `method` stored in a scan's `fft` node `params`: `None` when absent or `null`,
/// refused (as the `fft` stage's own check words it) when neither `stepped` nor
/// `source`.
fn stored_method(params: &serde_json::Value) -> Result<Option<ScanMethod>, NodeError> {
    match params.get("method") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|_| NodeError::Other("method must be stepped or source".into())),
    }
}

/// A source's refusal of a sweep as the reason a scan reports: the message of an
/// `unsupported` or `io` refusal, else the refusal as it reads.
fn refusal_reason(e: &PortError) -> String {
    match e {
        PortError::Unsupported(m) | PortError::Io(m) => m.clone(),
        e => e.to_string(),
    }
}

/// `e`, a stepped scan's lease refused after its source refused to sweep for itself
/// for `fallback`: a conflict gains the option `the source refused to sweep for
/// itself: <reason>`, so the caller learns both why the source would not sweep and
/// who holds the radio.
fn with_fallback(e: LeaseError, fallback: Option<&str>) -> LeaseError {
    match (e, fallback) {
        (
            LeaseError::Conflict {
                source,
                holders,
                current,
                mut options,
            },
            Some(why),
        ) => {
            options.push(format!("the source refused to sweep for itself: {why}"));
            LeaseError::Conflict {
                source,
                holders,
                current,
                options,
            }
        }
        (e, _) => e,
    }
}

/// The capture a scan at `bits` steps at on `port`: the cap of the depth in
/// force ([`Port::depth`], which a re-measure can move under the run's record,
/// and which the port bounds its tunes by) when it is that depth, or any depth
/// without `bits`; else the record's figure for `bits`, or its
/// `max_bandwidth_hz` without them (a source with one depth).
fn capture_of(port: &dyn Port, bits: Option<u8>) -> u64 {
    port.depth()
        .filter(|d| bits.is_none_or(|b| d.bits == b))
        .map(|d| d.max_bandwidth_hz.value().0)
        .unwrap_or_else(|| scan_capture_hz(port.capabilities(), bits))
}

/// The depth a scan asking for `bits` reports on `port`: `bits`, else the
/// port's depth in force; `None` on a source without depths.
fn bits_in_force(port: &dyn Port, bits: Option<u8>) -> Option<u8> {
    if port.capabilities().depths.is_empty() {
        return None;
    }
    bits.or_else(|| port.depth().map(|d| d.bits))
}

/// The scan's step task: with `bits` asked for and another depth in force, set
/// the source's depth to it first (the scan holds its time-shared lease, so it
/// is the source's sole holder and nothing else is tuned meanwhile), then run
/// [`scan_loop`]. However the loop ends (stopped, failed, the FFT task gone, the
/// node shutting down), the depth the port had before is set again before this
/// returns: the supervisor releases the lease only once every task of the job
/// has ended, so no job is granted the source until it is back at its own
/// depth. A depth that cannot be put back (the device gone) is logged; a
/// respawned worker opens at the source's own depth.
async fn scan_run(
    inner: Arc<Inner>,
    s: Arc<Scan>,
    tx: mpsc::Sender<Step>,
) -> Result<(), StageError> {
    // Only a scan that asked for a depth changes it, and only that one puts it back.
    let before = s.bits.and(s.port.depth().map(|d| d.bits));
    let set = match s.bits {
        Some(b) if before != Some(b) => s.port.set_depth(b).await.map(|_| ()).map_err(|e| {
            StageError::Failed(source_fault_reason(
                s.port.id(),
                &format!("set_depth {b}: {e}"),
            ))
        }),
        _ => Ok(()),
    };
    let ended = match set {
        Ok(()) => scan_loop(inner, s.clone(), tx).await,
        Err(e) => Err(e),
    };
    if let Some(b) = before
        && s.port.depth().map(|d| d.bits) != Some(b)
        && let Err(e) = s.port.set_depth(b).await
    {
        eprintln!(
            "zeta: scan {}: source {}: could not put its {b}-bit depth back: {e}",
            s.id.0,
            s.port.id().0
        );
    }
    ended
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
/// capture ([`capture_of`]), usable fraction or depth changed (a rate cap
/// measured after the scan started), the grid and mask are rebuilt from them
/// and `job_context.scan` rewritten, so that sweep steps at the new width.
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
    let budget = scan_average_budget_bytes(inner.ring_budget_bytes);
    let ring = inner.ring_capacity_samples(source, layout.rate_hz());
    let mut average = layout.average_in_force(s.look, ring, budget);
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
        if i == 0 {
            let caps = s.port.capabilities();
            let capture = capture_of(s.port.as_ref(), s.bits);
            let bits = bits_in_force(s.port.as_ref(), s.bits);
            if layout.stale(caps, capture, bits) {
                layout = Layout::of(s.span, caps, capture, bits, layout.mask);
                generation += 1;
                let ring = inner.ring_capacity_samples(source, layout.rate_hz());
                average = layout.average_in_force(s.look, ring, budget);
                inner.scan_context_rewrite(&s.id, layout.context(s.look, ring, budget));
            }
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
                average,
                bits: layout.bits,
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
/// `average` in force over 1 it keeps the averages: one per grid (even and odd
/// sweeps apart), of the step's `average`, started afresh when the layout
/// changes (which is when the average in force can change).
async fn fft_task(
    inner: Arc<Inner>,
    s: Arc<Scan>,
    mut rx: mpsc::Receiver<Step>,
) -> Result<(), StageError> {
    let fresh = |n: u32| [0, 1].map(|_| SweepAverage::new(n));
    let mut averages = (0u64, fresh(1));
    loop {
        let step = tokio::select! {
            _ = s.cancel.cancelled() => return Err(StageError::Cancelled),
            r = rx.recv() => match r {
                Some(step) => step,
                // The loop ended, and its end is the job's.
                None => return Err(StageError::Cancelled),
            },
        };
        if step.layout != averages.0 || averages.1[0].depth() != step.average {
            averages = (step.layout, fresh(step.average));
        }
        let grid = usize::from(step.alternates && step.sweep % 2 == 1);
        let avg = (step.average > 1).then(|| &mut averages.1[grid]);
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
/// stream (bus capacity) until the stage ends. Each row carries `bits`, the
/// depth it was taken at (on a source with depths), and `sweeps`:
/// with `avg` (an `average` in force over 1) it is averaged into it ([`average_row`]),
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
        bits,
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
            if let Some(b) = bits {
                o.insert("bits".into(), b.into());
            }
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

// The scan a source sweeps for itself (spec 4.7): the source's rows forwarded in
// the scan row shape. No lease, no tune, no FFT here.

/// What a self-sweep scan needs beside its rows (which carry the plan the source
/// chose): its id, its band, its look and the averaging budget (the `average` in
/// force follows from the plan, [`sweep_average`]), whether the source's levels
/// are `calibrated`, its port and the sweep it asks for (asked again while it
/// waits, [`wait_for_sweep`]), and its output stream.
struct SelfSweep {
    id: JobId,
    span: Tuning,
    look: Look,
    budget: u64,
    calibrated: bool,
    port: Arc<dyn Port>,
    req: SweepRequest,
    prov: Provenance,
    out: StreamId,
    cancel: CancellationToken,
}

/// How a self-sweep scan starts: with the source's rows, or `Waiting` (the reason)
/// for a restart the source refused, asking again ([`wait_for_sweep`]).
enum SweepStart {
    Rows(SweepRows),
    Waiting(String),
}

/// The `average` in force for `look` on the source's `plan` with `budget_bytes` for
/// the averages: one row a step, the plan's steps (twice when its odd sweeps
/// shift) and its bins ([`scan_average_in_force`]).
fn sweep_average(look: Look, budget_bytes: u64, plan: &SweepPlan) -> u32 {
    let grids = if plan.shift_hz.0 != 0 { 2 } else { 1 };
    scan_average_in_force(
        look.average,
        budget_bytes,
        u64::from(plan.steps) * grids,
        1,
        u64::from(plan.size),
    )
}

/// A self-sweep scan's `job_context.scan` while it waits for its source to grant a
/// sweep: `method` (`source`), `mask` (none), `calibrated` and `waiting` (`why`).
/// No `steps` or other key of a plan until one arrives ([`sweep_context`]).
fn waiting_context(calibrated: bool, why: &str) -> serde_json::Map<String, serde_json::Value> {
    let scan = serde_json::json!({
        "method": ScanMethod::Source.name(),
        "mask": {"notch_hz": 0, "edge_mask": 0.0},
        "calibrated": calibrated,
        "waiting": why,
    });
    match scan {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("built as an object"),
    }
}

/// The self-sweep scan's one task: [`sweep_loop`] over the rows `start` holds, or,
/// waiting, over the rows of the sweep [`wait_for_sweep`] is granted.
async fn sweep_task(inner: Arc<Inner>, s: SelfSweep, start: SweepStart) -> Result<(), StageError> {
    let rows = match start {
        SweepStart::Rows(rows) => rows,
        SweepStart::Waiting(why) => match wait_for_sweep(&inner, &s, why).await {
            Some(rows) => rows,
            None => return Err(StageError::Cancelled),
        },
    };
    sweep_loop(inner, s, rows).await
}

/// Ask the source again for the sweep it refused (`waiting`, the reason), after each
/// of [`SWEEP_RETRY_MS`] in turn, the last repeating, until it is granted (`None`
/// once cancelled): each ask under the source's gate ([`Inner::start_sweep`], the
/// scan's claim kept through a refusal), skipped with the reason while the source's
/// `sweep` does not hold the span. A new reason is written as `waiting`; granted,
/// the plan's keys ([`sweep_context`]) with `waiting: null`. It never steps.
async fn wait_for_sweep(inner: &Inner, s: &SelfSweep, mut waiting: String) -> Option<SweepRows> {
    let mut k = 0usize;
    loop {
        let ms = SWEEP_RETRY_MS[k.min(SWEEP_RETRY_MS.len() - 1)];
        k += 1;
        tokio::select! {
            _ = s.cancel.cancelled() => return None,
            _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
        }
        let asked = match scan_sweep_refusal(s.port.id(), s.port.capabilities(), s.span) {
            Some(why) => Err(why),
            None => {
                let gate = tokio::select! {
                    _ = s.cancel.cancelled() => return None,
                    g = inner.leases.gate_guard(s.port.id()) => g,
                };
                tokio::select! {
                    _ = s.cancel.cancelled() => return None,
                    r = inner.start_sweep(s.port.as_ref(), &s.id, s.req, &gate) => {
                        r.map_err(|e| refusal_reason(&e))
                    }
                }
            }
        };
        match asked {
            Ok(rows) => {
                let average = sweep_average(s.look, s.budget, &rows.plan);
                let fields = sweep_context(&rows.plan, s.look, average, s.calibrated);
                inner.scan_context_update(&s.id, |scan| scan.extend(fields));
                return Some(rows);
            }
            Err(why) if why != waiting => {
                waiting = why.clone();
                inner.scan_context_update(&s.id, |scan| {
                    scan.insert("waiting".into(), why.into());
                });
            }
            Err(_) => {}
        }
    }
}

/// A self-sweep scan's `job_context.scan` at its start: `method` (`source`), the
/// source's `plan` (`steps`, `step_hz`, `bin_hz`, `dwell_ms`, the dwell in force,
/// with `dwell_ms_asked` when `look`'s differs), `average` (in force) with
/// `average_asked` when that differs, `alternate` (the odd sweeps' shift, or
/// null), `mask` (none: the source masks its own rows), `calibrated`, and
/// `waiting` (null). No `rate_hz` and no `bits`: the scan takes no IQ.
fn sweep_context(
    plan: &SweepPlan,
    look: Look,
    average: u32,
    calibrated: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let alternate = match plan.shift_hz.0 {
        0 => serde_json::Value::Null,
        shift => serde_json::json!({"shift_hz": shift, "steps_shifted": plan.steps}),
    };
    let mut scan = serde_json::json!({
        "method": "source",
        "steps": plan.steps,
        "step_hz": plan.step_hz.0,
        "bin_hz": plan.bin_hz,
        "dwell_ms": plan.dwell_ms,
        "average": average,
        "alternate": alternate,
        "mask": {"notch_hz": 0, "edge_mask": 0.0},
        "calibrated": calibrated,
        "waiting": null,
    });
    if plan.dwell_ms != look.dwell_ms {
        scan["dwell_ms_asked"] = look.dwell_ms.into();
    }
    if average != look.average {
        scan["average_asked"] = look.average.into();
    }
    match scan {
        serde_json::Value::Object(m) => m,
        _ => unreachable!("built as an object"),
    }
}

/// A self-sweep's [`SweepTiming`] as its rows arrive: a row is a step of no tune
/// that took the time since the last row; when a row of the next sweep arrives,
/// each step the last sweep sent no row for is a skip, and the sweep (from the
/// last row of the one before it, or the loop's start, to its own last row) is
/// written as `job_context.scan.timing` at most once a second, else carried.
struct SweepClock {
    timing: SweepTiming,
    /// When the timing was last written (ns since the epoch).
    written_ns: u64,
    last_row: tokio::time::Instant,
    /// The sweep under way: its number, when it began and the steps it sent.
    sweep: Option<(u64, tokio::time::Instant, Vec<bool>)>,
    steps: usize,
}

impl SweepClock {
    fn new(steps: u32) -> Self {
        SweepClock {
            timing: SweepTiming::default(),
            written_ns: 0,
            last_row: tokio::time::Instant::now(),
            sweep: None,
            steps: usize::try_from(steps).unwrap_or(usize::MAX),
        }
    }

    /// Count a row of step `step` of sweep `sweep`, closing the sweep before it.
    fn row(&mut self, inner: &Inner, s: &SelfSweep, sweep: u64, step: u32) {
        let now = tokio::time::Instant::now();
        if let Some((_, start, sent)) = self.sweep.take_if(|(n, ..)| *n != sweep) {
            for _ in sent.iter().filter(|sent| !**sent) {
                self.timing.step(Duration::ZERO, Duration::ZERO, true);
            }
            let at_ns = super::persist::now().0;
            if at_ns.saturating_sub(self.written_ns) >= TIMING_EVERY_NS {
                let took = self.last_row - start;
                let mut t = self.timing.finish(s.span.bandwidth_hz.0, took);
                t["at_ns"] = at_ns.into();
                self.written_ns = at_ns;
                inner.scan_context_update(&s.id, |scan| {
                    scan.insert("timing".into(), t);
                });
            } else {
                self.timing.carry();
            }
        }
        let (steps, last_row) = (self.steps, self.last_row);
        let (_, _, sent) = self
            .sweep
            .get_or_insert_with(|| (sweep, last_row, vec![false; steps]));
        if let Some(sent) = usize::try_from(step).ok().and_then(|i| sent.get_mut(i)) {
            *sent = true;
        }
        self.timing.step(Duration::ZERO, now - self.last_row, false);
        self.last_row = now;
    }
}

/// A source's row as a scan row: `size`, `bin_hz`, `rows_per_s`, `center_hz`,
/// `floor_db` and `q` by [`quantise_row_masked`] over the row's `masked` (with
/// `avg`, an `average` in force over 1, from the mean of the step's last rows
/// instead), `step_db`, `masked` (when not empty), `sweeps` and, with
/// `calibrated`, `calibrated: true`.
fn sweep_row_event(
    row: &SweepRow,
    rows_per_s: u32,
    calibrated: bool,
    avg: Option<&mut SweepAverage>,
) -> serde_json::Value {
    let (mut floor_db, mut q) = quantise_row_masked(&row.db, &row.masked);
    let mut sweeps = 1;
    if let Some(avg) = avg {
        let (db, n) = avg.push((row.center_hz.0, 0), floor_db, &q);
        (floor_db, q) = quantise_row_masked(&db, &row.masked);
        sweeps = n;
    }
    let mut ev = serde_json::json!({
        "size": row.db.len(),
        "bin_hz": row.bin_hz,
        "rows_per_s": rows_per_s,
        "center_hz": row.center_hz.0,
        "floor_db": floor_db,
        "step_db": STEP_DB,
        "q": q,
        "sweeps": sweeps,
    });
    if !row.masked.is_empty() {
        ev["masked"] = serde_json::json!(row.masked);
    }
    if calibrated {
        ev["calibrated"] = true.into();
    }
    ev
}

/// The self-sweep scan's one task, until cancelled: forward each of the source's
/// rows on the scan's stream as a scan row ([`sweep_row_event`]), channel
/// `scan/<step>`, its span the row's, with `rows_per_s` the dwell's
/// ([`dwell_rows_per_s`]); with an `average` in force over 1 it keeps one
/// [`SweepAverage`] per grid, the odd sweeps apart when the plan shifts them, as
/// [`fft_task`] does. The source's state is written as `job_context.scan.waiting`
/// (the reason while it retries, else null) and each sweep's timing as `timing`
/// ([`SweepClock`]). The rows closing ends the job, failed as `sweep on source
/// <id>: <the reason>` (never `source <id>: …`: the source's IQ side is not at
/// fault). It never reads [`Port::fault`]; cancelled, it drops the rows first,
/// which ends the source's sweep.
async fn sweep_loop(inner: Arc<Inner>, s: SelfSweep, rows: SweepRows) -> Result<(), StageError> {
    let SweepRows {
        plan,
        rows: mut rx,
        mut state,
    } = rows;
    let rows_per_s = dwell_rows_per_s(plan.dwell_ms);
    let alternates = plan.shift_hz.0 != 0;
    let average = sweep_average(s.look, s.budget, &plan);
    let mut averages = [0, 1].map(|_| SweepAverage::new(average));
    let mut clock = SweepClock::new(plan.steps);
    let mut waiting = None;
    // A state the source set before the loop began is written too.
    state.mark_changed();
    let mut state_open = true;
    loop {
        tokio::select! {
            biased;
            _ = s.cancel.cancelled() => {
                drop(rx);
                return Err(StageError::Cancelled);
            }
            r = rx.recv() => {
                let Some(row) = r else {
                    let why = match &*state.borrow() {
                        SweepState::Ended(r) => r.clone(),
                        _ => "ended".to_string(),
                    };
                    return Err(StageError::Failed(format!(
                        "sweep on source {}: {why}",
                        s.prov.source.0
                    )));
                };
                clock.row(&inner, &s, row.sweep, row.step);
                let grid = usize::from(alternates && row.sweep % 2 == 1);
                let avg = (average > 1).then(|| &mut averages[grid]);
                let ev = sweep_row_event(&row, rows_per_s, s.calibrated, avg);
                let prov = Provenance {
                    channel: Some(ChannelId(format!("scan/{}", row.step))),
                    span: row.span,
                    ..s.prov.clone()
                };
                inner
                    .bus
                    .publish(Frame::new(s.out.clone(), prov, Payload::Event(ev)));
            }
            changed = state.changed(), if state_open => {
                if changed.is_err() {
                    // The source is done with the state; the rows closing says the rest.
                    state_open = false;
                    continue;
                }
                let now = match &*state.borrow_and_update() {
                    SweepState::Waiting(r) => Some(r.clone()),
                    SweepState::Running | SweepState::Ended(_) => None,
                };
                if now != waiting {
                    waiting = now.clone();
                    inner.scan_context_update(&s.id, |scan| {
                        scan.insert("waiting".into(), now.into());
                    });
                }
            }
        }
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

    /// Of two tunes the median is the lower, so one slow tune of a two-step
    /// sweep counts as slow.
    #[test]
    fn a_two_step_sweep_counts_its_slow_tune() {
        let mut t = SweepTiming::default();
        t.step(ms_of(3), ms_of(25), false);
        t.step(ms_of(150), ms_of(180), false);
        let v = t.finish(40_000_000, ms_of(205));
        assert_eq!(v["tune_ms"]["median"], 3.0, "{v}");
        assert_eq!(v["slow_tunes"], 1, "{v}");
        assert_eq!(v["step_ms"], 25.0, "{v}");
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
