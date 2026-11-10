//! The `scan` job: time-shares one radio across a span wider than its capture,
//! stepping the source across the span and publishing one `fft` stage's rows per
//! step (see [`scan_loop`]).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OwnedMutexGuard;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::launch::{AfterGate, StageRunning, supervise};
use super::{Inner, JobOutput, NodeError, source_fault_reason};
use crate::graph::{self, GraphError, NodeId};
use crate::job::JobRecord;
use crate::ops::{
    JobKind, SPECTRUM_ROWS_PER_S, ScanGrid, scan_edge_mask_in_force, scan_grid, scan_mask_default,
    scan_notch_in_force, spectrum_size,
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
        let out = self.bus.register_unfed(StreamKind::Event, prov.clone());
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
    let output = bus.register_unfed(StreamKind::Event, prov.clone());
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
