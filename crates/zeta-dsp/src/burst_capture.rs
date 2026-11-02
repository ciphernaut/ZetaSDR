//! `burst_capture`: cut a channel's bursts into SigMF windows in the node's
//! [`BurstStore`] as they pass, one `capture` event each, and end once `bursts`
//! windows are written or `timeout_ms` has passed.
//!
//! The gate is layer 1's ([`crate::measure::burst_runs`]) run over a rolling envelope:
//! the mean power of each `BURST_ENVELOPE_MS` chunk is compared with the
//! `FLOOR_PERCENTILE`th-percentile chunk of the last `pre_ms + window_ms + post_ms`
//! (at least [`GATE_WARMUP_MS`] of them); a chunk more than `BURST_ON_DB` over that
//! floor is on, and a run ends after `BURST_MERGE_MS` of chunks off. While a run is
//! on, the floor is held at its value at the on-edge, so a long burst does not raise
//! its own floor and end itself.
//!
//! A window runs from `pre_ms` before the on-edge to `post_ms` after the off-edge. A
//! burst still on `window_ms` after its window started is cut there: that window takes
//! `post_ms` more, and the next starts at the cut, back to back, so each holds at most
//! `window_ms + post_ms`. A burst that starts inside an open window's `post_ms` joins
//! it. With `whole`, the gate decides nothing: windows of `window_ms` are written back
//! to back from the first sample. A retune, a gap in time or a lag drops the window
//! being cut and restarts the gate.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::bursts::{BurstStore, NewCapture};
use zeta_core::ops::BURST_CAPTURE_STAGE;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, JobId, Span, StreamId, StreamKind, Timestamp};

use crate::measure::{
    BURST_ENVELOPE_MS, BURST_MERGE_MS, BURST_ON_DB, FLOOR_PERCENTILE, measure_band,
};

/// `bursts` by default: windows one run writes.
pub const BURSTS_DEFAULT: u32 = 32;
/// `window_ms` by default: the longest stretch of one burst a window holds.
pub const WINDOW_MS_DEFAULT: u64 = 2_000;
/// `pre_ms` by default: the guard before the on-edge.
pub const PRE_MS_DEFAULT: u64 = 20;
/// `post_ms` by default: the guard after the off-edge.
pub const POST_MS_DEFAULT: u64 = 20;
/// `timeout_ms` by default: how long a run waits for its bursts.
pub const TIMEOUT_MS_DEFAULT: u64 = 120_000;
/// The gate decides nothing until the envelope holds this many ms: a floor needs
/// chunks to be a percentile of.
pub const GATE_WARMUP_MS: u64 = 100;

/// `burst_capture` parameters; every one optional.
#[derive(Clone, Debug, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BurstCaptureParams {
    /// Windows to write before the stage ends (at least 1).
    pub bursts: u32,
    /// The longest stretch of one burst a window holds, ms (at least 1).
    pub window_ms: u64,
    /// Guard before a burst's on-edge, ms.
    pub pre_ms: u64,
    /// Guard after a burst's off-edge, ms.
    pub post_ms: u64,
    /// The stage ends after this many ms (of signal or of wall clock, whichever comes
    /// first) with `timed_out` (at least 1).
    pub timeout_ms: u64,
    /// Write `bursts` windows of `window_ms` back to back, regardless of the gate.
    pub whole: bool,
    /// The channel's band, recorded with each capture and measured in its features;
    /// `null` for the channel's whole rate.
    pub bandwidth_hz: Option<u64>,
    /// The signal a `watch` captures (centre, width, SNR as detected), recorded with each
    /// capture; set by the watch, `null` for a hand capture.
    pub signal: Option<serde_json::Value>,
    /// The `watch` that started the capture, recorded with each capture.
    pub watch: Option<JobId>,
    /// The register's allocations the watch's identify found, recorded with each capture
    /// (`null`: none, recorded as `[]`).
    pub allocations: Option<serde_json::Value>,
}

impl Default for BurstCaptureParams {
    fn default() -> Self {
        Self {
            bursts: BURSTS_DEFAULT,
            window_ms: WINDOW_MS_DEFAULT,
            pre_ms: PRE_MS_DEFAULT,
            post_ms: POST_MS_DEFAULT,
            timeout_ms: TIMEOUT_MS_DEFAULT,
            whole: false,
            bandwidth_hz: None,
            signal: None,
            watch: None,
            allocations: None,
        }
    }
}

impl BurstCaptureParams {
    /// The fields the schema cannot bound, the first bad one named.
    fn check(&self) -> Result<(), String> {
        for (field, v) in [
            ("bursts", u64::from(self.bursts)),
            ("window_ms", self.window_ms),
            ("timeout_ms", self.timeout_ms),
        ] {
            if v == 0 {
                return Err(format!("{field}: must be at least 1"));
            }
        }
        Ok(())
    }
}

/// One window cut from the stream.
#[derive(Debug)]
struct Cut {
    start_ns: u64,
    end_ns: u64,
    rate: u64,
    center: u64,
    samples: Vec<Complex<f32>>,
}

/// The window being cut, in absolute sample indices.
#[derive(Debug)]
struct Open {
    start: u64,
    /// Where the burst is cut if still on.
    cut: u64,
    /// Where the window ends, once known.
    end: Option<u64>,
    /// `end` was set by the cut, not by an off-edge.
    cut_reached: bool,
}

/// The gate and the window cutter over one stream, block by block.
struct Gate {
    p: BurstCaptureParams,
    /// The stream's rate and centre; `None` before the first block and after a reset.
    tuning: Option<(u64, u64)>,
    /// Samples per envelope chunk.
    chunk: u64,
    /// Absolute index and time of the first sample since the last reset.
    base: (u64, u64),
    /// Absolute index of the next sample.
    next: u64,
    hist: VecDeque<Complex<f32>>,
    /// Absolute index of `hist`'s first sample.
    hist_start: u64,
    /// The chunk being summed: power and samples.
    partial: (f64, u64),
    env: VecDeque<f64>,
    floor: Option<f64>,
    /// The floor held over an on-run; `Some` while one is on.
    held: Option<f64>,
    off_chunks: u64,
    open: Option<Open>,
    bursts_seen: u64,
    /// Signal time seen, ns.
    elapsed_ns: u64,
}

impl Gate {
    fn new(p: BurstCaptureParams) -> Self {
        Self {
            p,
            tuning: None,
            chunk: 1,
            base: (0, 0),
            next: 0,
            hist: VecDeque::new(),
            hist_start: 0,
            partial: (0.0, 0),
            env: VecDeque::new(),
            floor: None,
            held: None,
            off_chunks: 0,
            open: None,
            bursts_seen: 0,
            elapsed_ns: 0,
        }
    }

    fn rate(&self) -> u64 {
        self.tuning.map_or(1, |t| t.0.max(1))
    }

    /// `ms` in samples at the stream's rate.
    fn samples(&self, ms: u64) -> u64 {
        self.rate() * ms / 1_000
    }

    fn time_of(&self, idx: u64) -> u64 {
        let (i0, t0) = self.base;
        t0 + ((idx - i0) as u128 * 1_000_000_000 / self.rate() as u128) as u64
    }

    /// Drop the stream state (the window being cut included) and start again at the
    /// next block.
    fn reset(&mut self) {
        self.tuning = None;
        self.hist.clear();
        self.partial = (0.0, 0);
        self.env.clear();
        self.floor = None;
        self.held = None;
        self.off_chunks = 0;
        self.open = None;
    }

    /// Feed one block; the windows it completes, in order.
    fn push(&mut self, block: &IqBlock) -> Vec<Cut> {
        let (rate, center) = (block.sample_rate_hz.0, block.center_hz.0);
        if rate == 0 {
            return Vec::new();
        }
        if let Some((r, c)) = self.tuning {
            let expected = self.time_of(self.next) as u128;
            let tolerance = 1_000_000_000u128.div_ceil(r as u128);
            let gap = (block.t0.0 as u128).abs_diff(expected) > tolerance;
            if (r, c) != (rate, center) || gap {
                self.reset();
            }
        }
        if self.tuning.is_none() {
            self.tuning = Some((rate, center));
            self.chunk = (rate * BURST_ENVELOPE_MS / 1_000).max(1);
            self.base = (self.next, block.t0.0);
            self.hist_start = self.next;
            if self.p.whole {
                self.open_whole(self.next);
            }
        }
        self.elapsed_ns += (block.samples.len() as u128 * 1_000_000_000 / rate as u128) as u64;
        self.hist.extend(block.samples.iter().copied());
        // The block's whole chunks, as (end index, mean power).
        let mut chunks = Vec::new();
        for (i, s) in block.samples.iter().enumerate() {
            self.partial.0 += s.norm_sqr() as f64;
            self.partial.1 += 1;
            if self.partial.1 == self.chunk {
                let end = self.next + i as u64 + 1;
                chunks.push((end, self.partial.0 / self.chunk as f64));
                self.partial = (0.0, 0);
            }
        }
        self.next += block.samples.len() as u64;
        let keep = self.samples(self.p.pre_ms + self.p.window_ms + self.p.post_ms);
        let keep_chunks = (keep / self.chunk).max(GATE_WARMUP_MS / BURST_ENVELOPE_MS);
        self.env.extend(chunks.iter().map(|c| c.1));
        while self.env.len() as u64 > keep_chunks {
            self.env.pop_front();
        }
        self.floor = (self.env.len() as u64 >= GATE_WARMUP_MS / BURST_ENVELOPE_MS).then(|| {
            let mut p: Vec<f64> = self.env.iter().copied().collect();
            p.sort_by(f64::total_cmp);
            p[p.len() * FLOOR_PERCENTILE / 100]
        });
        let mut cuts = Vec::new();
        for (end, power) in chunks {
            self.gate(end, power);
            self.advance(end, &mut cuts);
        }
        // Keep the guard and the window being cut.
        let mut from = self.next.saturating_sub(keep).max(self.hist_start);
        if let Some(o) = &self.open {
            from = from.min(o.start);
        }
        while self.hist_start < from {
            self.hist.pop_front();
            self.hist_start += 1;
        }
        cuts
    }

    /// The gate over the chunk ending at `end` of mean power `power`.
    fn gate(&mut self, end: u64, power: f64) {
        let Some(floor) = self.held.or(self.floor) else {
            return;
        };
        // A zero floor holds every chunk with power on; 0/0 is not on.
        let on = 10.0 * (power / floor).log10() > BURST_ON_DB;
        let start = end - self.chunk;
        match (self.held.is_some(), on) {
            (false, true) => {
                self.held = Some(floor);
                self.off_chunks = 0;
                self.bursts_seen += 1;
                if self.p.whole {
                    return;
                }
                let pre = self.samples(self.p.pre_ms);
                let window = self.samples(self.p.window_ms);
                match &mut self.open {
                    // A burst inside the post guard joins the window.
                    Some(o) if !o.cut_reached => o.end = None,
                    Some(_) => {}
                    None => {
                        let s = start.saturating_sub(pre).max(self.hist_start);
                        self.open = Some(Open {
                            start: s,
                            cut: s + window,
                            end: None,
                            cut_reached: false,
                        });
                    }
                }
            }
            (true, true) => self.off_chunks = 0,
            (true, false) => {
                self.off_chunks += 1;
                if self.off_chunks >= (BURST_MERGE_MS / BURST_ENVELOPE_MS).max(1) {
                    self.held = None;
                    let off = end - self.off_chunks * self.chunk;
                    let post = self.samples(self.p.post_ms);
                    if !self.p.whole
                        && let Some(o) = &mut self.open
                        && o.end.is_none()
                    {
                        o.end = Some((off + post).min(o.cut + post));
                    }
                }
            }
            (false, false) => {}
        }
    }

    /// Close every window complete at `now`, cutting a burst still on at its window's
    /// `cut` and continuing it in the next.
    fn advance(&mut self, now: u64, cuts: &mut Vec<Cut>) {
        let post = self.samples(self.p.post_ms);
        let window = self.samples(self.p.window_ms);
        loop {
            let Some(o) = &mut self.open else {
                return;
            };
            if o.end.is_none() && now >= o.cut {
                o.end = Some(o.cut + post);
                o.cut_reached = true;
            }
            let end = match o.end {
                Some(e) if now >= e => e,
                _ => return,
            };
            let (start, cut, cut_reached) = (o.start, o.cut, o.cut_reached);
            let (rate, center) = self.tuning.expect("a window has a stream");
            let from = (start - self.hist_start) as usize;
            let to = (end - self.hist_start) as usize;
            cuts.push(Cut {
                start_ns: self.time_of(start),
                end_ns: self.time_of(end),
                rate,
                center,
                samples: self.hist.range(from..to).copied().collect(),
            });
            self.open = None;
            if self.p.whole {
                self.open_whole(end);
            } else if cut_reached && self.held.is_some() {
                self.open = Some(Open {
                    start: cut,
                    cut: cut + window,
                    end: None,
                    cut_reached: false,
                });
            }
        }
    }

    /// The next `whole` window, from `start`.
    fn open_whole(&mut self, start: u64) {
        let end = start + self.samples(self.p.window_ms);
        self.open = Some(Open {
            start,
            cut: end,
            end: Some(end),
            cut_reached: true,
        });
    }
}

/// See the module docs.
pub struct BurstCaptureStage {
    spec: StageSpec,
}

impl BurstCaptureStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: BURST_CAPTURE_STAGE,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(BurstCaptureParams),
            },
        }
    }
}

impl Default for BurstCaptureStage {
    fn default() -> Self {
        Self::new()
    }
}

/// What the run has done: `job_context.capture`.
struct Tally {
    written: Vec<String>,
    bytes: u64,
    timed_out: bool,
}

impl Tally {
    fn note(&self, ctx: &StageContext, gate: &Gate) {
        ctx.note(serde_json::json!({"capture": {
            "written": self.written,
            "bytes": self.bytes,
            "bursts_seen": gate.bursts_seen,
            "timed_out": self.timed_out,
        }}));
    }
}

#[async_trait]
impl Stage for BurstCaptureStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: BurstCaptureParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        p.check()
    }

    async fn run(
        &self,
        params: serde_json::Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let failed = |m: String| StageError::Failed(format!("{BURST_CAPTURE_STAGE}: {m}"));
        let p: BurstCaptureParams =
            serde_json::from_value(params).map_err(|e| failed(format!("params: {e}")))?;
        p.check().map_err(failed)?;
        let (Some(out), true) = (outputs.first().cloned(), inputs.len() == 1) else {
            return Err(failed("expected 1 input and 1 output".into()));
        };
        let store = ctx
            .bursts
            .clone()
            .ok_or_else(|| failed("the node has no burst store".into()))?;
        let rx = &mut inputs[0];
        let (bursts, timeout_ms) = (p.bursts as usize, p.timeout_ms);
        let mut gate = Gate::new(p.clone());
        let mut tally = Tally {
            written: Vec::new(),
            bytes: 0,
            timed_out: false,
        };
        tally.note(&ctx, &gate);
        // A run outside a job (no job in the provenance) still writes one set.
        let own_job = JobId::new();
        let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
        tokio::pin!(deadline);
        loop {
            let frame = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                _ = &mut deadline => {
                    tally.timed_out = true;
                    tally.note(&ctx, &gate);
                    return Ok(());
                }
                r = rx.recv() => match r {
                    Ok(frame) => frame,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        crate::note_lag(BURST_CAPTURE_STAGE, n);
                        gate.reset();
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tally.note(&ctx, &gate);
                        return Ok(());
                    }
                },
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(failed("expects iq frames".into()));
            };
            let seen = gate.bursts_seen;
            let job = frame.provenance.job.clone().unwrap_or(own_job.clone());
            for cut in gate.push(block) {
                if tally.written.len() >= bursts {
                    break;
                }
                let n = tally.written.len() + 1;
                let (ev, id, bytes, span) = write(&store, &ctx, &p, &job, cut, n, bursts)
                    .await
                    .map_err(failed)?;
                tally.written.push(id);
                tally.bytes += bytes;
                let mut prov = frame.provenance.clone();
                prov.span = span;
                bus.publish(Frame::new(out.clone(), prov, Payload::Event(ev)));
                tally.note(&ctx, &gate);
            }
            if tally.written.len() >= bursts {
                tally.note(&ctx, &gate);
                return Ok(());
            }
            if gate.elapsed_ns >= timeout_ms.saturating_mul(1_000_000) {
                tally.timed_out = true;
                tally.note(&ctx, &gate);
                return Ok(());
            }
            if gate.bursts_seen != seen {
                tally.note(&ctx, &gate);
            }
        }
    }
}

/// Store `cut` (the `n`th of `bursts`) as one capture of `job`'s set: its `capture`
/// event, the record's id, its bytes and its span.
async fn write(
    store: &Arc<BurstStore>,
    ctx: &StageContext,
    p: &BurstCaptureParams,
    job: &JobId,
    cut: Cut,
    n: usize,
    bursts: usize,
) -> Result<(serde_json::Value, String, u64, Span), String> {
    let bw = p.bandwidth_hz.unwrap_or(cut.rate).min(cut.rate);
    let features = measure_band(&cut.samples, cut.rate, cut.center, bw);
    let features = serde_json::to_value(&features).map_err(|e| e.to_string())?;
    let new = NewCapture {
        source: ctx.source.clone(),
        set: job.clone(),
        center_hz: Hz(cut.center),
        bandwidth_hz: Hz(bw),
        sample_rate_hz: Hz(cut.rate),
        start_ns: cut.start_ns,
        end_ns: cut.end_ns,
        features: features.clone(),
        signal: p.signal.clone(),
        job: job.clone(),
        watch: p.watch.clone(),
        allocations: p
            .allocations
            .clone()
            .unwrap_or_else(|| serde_json::json!([])),
    };
    let store = store.clone();
    let samples = cut.samples;
    let rec = tokio::task::spawn_blocking(move || store.write(new, &samples))
        .await
        .map_err(|e| format!("write task: {e}"))?
        .map_err(|e| format!("write: {e}"))?;
    let ms = (rec.end_ns - rec.start_ns) as f64 / 1e6;
    let ev = crate::util::event(
        ctx,
        rec.start_ns,
        "capture",
        serde_json::json!({
            "id": rec.id,
            "start_ns": rec.start_ns,
            "end_ns": rec.end_ns,
            "bytes": rec.bytes,
            "center_hz": rec.center_hz.0,
            "bandwidth_hz": rec.bandwidth_hz.0,
            "features": features,
            "summary": format!(
                "capture {n}/{bursts}: {ms:.0} ms at {} Hz, {} bytes",
                rec.center_hz.0, rec.bytes
            ),
            "t_ns": rec.start_ns,
        }),
    );
    let span = Span {
        start_ns: Timestamp(rec.start_ns),
        end_ns: Timestamp(rec.end_ns),
    };
    Ok((ev, rec.id, rec.bytes, span))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use num_complex::Complex;
    use serde_json::Value;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::bursts::{BurstPolicy, BurstStore};
    use zeta_core::port::IqBlock;
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{
        Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp,
    };

    use super::BurstCaptureStage;
    use crate::synth::awgn;

    const RATE: u64 = 48_000;
    const CENTER: u64 = 144_800_000;
    /// One block of input: 100 ms.
    const BLOCK: usize = 4_800;
    /// The first sample's time: now, whole seconds, so the store's age rule keeps the
    /// windows.
    static T0: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        s * 1_000_000_000
    });

    /// `secs` of complex noise at 0.01 power (20 dB under a unit carrier).
    fn noise(secs: f64, seed: u64) -> Vec<Complex<f32>> {
        let mut n = vec![Complex::new(0.0f32, 0.0); (secs * RATE as f64) as usize];
        awgn(&mut n, 20.0, seed);
        n
    }

    /// A unit carrier 2 kHz above the centre over `[from_s, from_s + ms)` of `iq`.
    fn carrier(iq: &mut [Complex<f32>], from_s: f64, ms: f64) {
        let start = (from_s * RATE as f64) as usize;
        let n = (ms * RATE as f64 / 1_000.0) as usize;
        let end = (start + n).min(iq.len());
        for (i, s) in iq[start..end].iter_mut().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * 2_000.0 * i as f64 / RATE as f64;
            *s += Complex::from_polar(1.0, ph as f32);
        }
    }

    struct Run {
        result: Result<(), StageError>,
        events: Vec<Value>,
        context: Value,
        store: Arc<BurstStore>,
        _dir: tempfile::TempDir,
    }

    /// Run a `burst_capture` stage with `params` over `iq` in [`BLOCK`]s, the input left
    /// open, until the stage ends on its own (within 10 s).
    async fn run(params: Value, iq: &[Complex<f32>]) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let store = BurstStore::open(dir.path(), BurstPolicy::default()).unwrap();
        let prov = Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let bus = Arc::new(StreamBus::new(64));
        let out = bus.register(StreamKind::Event, prov.clone());
        let mut rx_out = bus.subscribe(&out).unwrap();
        let blocks = iq.len().div_ceil(BLOCK);
        let (tx, rx_in) = broadcast::channel(blocks + 1);
        let notes = Arc::new(Mutex::new(serde_json::json!({})));
        let sink = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_bursts(store.clone())
            .with_note(Arc::new(move |v| {
                let mut n = sink.lock().unwrap();
                for (k, x) in v.as_object().unwrap() {
                    n[k] = x.clone();
                }
            }));
        for (i, block) in iq.chunks(BLOCK).enumerate() {
            tx.send(Frame::new(
                StreamId::new(),
                prov.clone(),
                Payload::Iq(IqBlock {
                    t0: Timestamp(*T0 + (i * BLOCK) as u64 * 1_000_000_000 / RATE),
                    sample_rate_hz: Hz(RATE),
                    center_hz: Hz(CENTER),
                    samples: block.to_vec().into(),
                }),
            ))
            .unwrap();
        }
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            BurstCaptureStage::new().run(
                params,
                vec![rx_in],
                vec![out],
                bus,
                CancellationToken::new(),
                ctx,
            ),
        )
        .await
        .expect("the stage ends on its own");
        drop(tx);
        let mut events = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            let Payload::Event(v) = f.payload else {
                panic!("an event")
            };
            events.push(v);
        }
        let context = notes.lock().unwrap()["capture"].clone();
        Run {
            result,
            events,
            context,
            store,
            _dir: dir,
        }
    }

    fn ms(ev: &Value) -> f64 {
        (ev["end_ns"].as_u64().unwrap() - ev["start_ns"].as_u64().unwrap()) as f64 / 1e6
    }

    #[tokio::test]
    async fn each_burst_becomes_one_window_with_the_guards() {
        let mut iq = noise(3.2, 1);
        for s in [0.5, 1.5, 2.5] {
            carrier(&mut iq, s, 5.0);
        }
        let r = run(serde_json::json!({"bursts": 3}), &iq).await;
        assert!(matches!(r.result, Ok(())), "{:?}", r.result);
        assert_eq!(r.events.len(), 3, "{:?}", r.events);
        for (i, ev) in r.events.iter().enumerate() {
            assert_eq!(ev["protocol"], "capture");
            assert!((ms(ev) - 45.0).abs() <= 1.0, "window {i}: {} ms", ms(ev));
            let on_ns = *T0 + (500 + 1_000 * i as u64) * 1_000_000;
            let start = ev["start_ns"].as_u64().unwrap();
            assert!(start.abs_diff(on_ns - 20_000_000) <= 1_000_000, "{start}");
            assert_eq!(ev["t_ns"], ev["start_ns"]);
            let duty = ev["features"]["duty"].as_f64().unwrap();
            assert!((0.07..=0.15).contains(&duty), "duty {duty}");
            assert_eq!(ev["center_hz"], CENTER);
            assert!(ev["summary"].is_string());
            let rec = r.store.get(ev["id"].as_str().unwrap()).unwrap();
            assert_eq!(rec.set, JobId("job".into()));
            assert_eq!(rec.job, JobId("job".into()));
            assert_eq!(rec.bytes, ev["bytes"].as_u64().unwrap());
        }
        let ids: Vec<&Value> = r.events.iter().map(|e| &e["id"]).collect();
        let written: Vec<&Value> = r.context["written"].as_array().unwrap().iter().collect();
        assert_eq!(written, ids);
        assert_eq!(r.context["bursts_seen"], 3);
        assert_eq!(r.context["timed_out"], false);
        assert_eq!(r.store.list().len(), 3);
    }

    #[tokio::test]
    async fn a_burst_longer_than_the_window_is_cut_into_windows() {
        let mut iq = noise(6.0, 2);
        carrier(&mut iq, 0.5, 5_000.0);
        let r = run(serde_json::json!({"bursts": 2, "window_ms": 2_000}), &iq).await;
        assert!(matches!(r.result, Ok(())), "{:?}", r.result);
        assert_eq!(r.events.len(), 2, "{:?}", r.events);
        for ev in &r.events {
            assert!((ms(ev) - 2_020.0).abs() <= 1.0, "{} ms", ms(ev));
        }
        // Back to back: the second starts where the first's window_ms ends.
        let (a, b) = (&r.events[0], &r.events[1]);
        assert_eq!(
            b["start_ns"].as_u64().unwrap() - a["start_ns"].as_u64().unwrap(),
            2_000_000_000
        );
        assert_eq!(r.context["bursts_seen"], 1);
        assert_eq!(r.context["timed_out"], false);
    }

    #[tokio::test]
    async fn whole_writes_windows_regardless_of_the_gate() {
        let iq = noise(1.0, 3);
        let r = run(
            serde_json::json!({"bursts": 2, "window_ms": 200, "whole": true}),
            &iq,
        )
        .await;
        assert!(matches!(r.result, Ok(())), "{:?}", r.result);
        assert_eq!(r.events.len(), 2, "{:?}", r.events);
        assert_eq!(r.events[0]["start_ns"], *T0);
        for ev in &r.events {
            assert!((ms(ev) - 200.0).abs() <= 1.0, "{} ms", ms(ev));
        }
        assert_eq!(r.events[1]["start_ns"], *T0 + 200_000_000);
        assert_eq!(r.context["written"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_timeout_ends_the_stage_with_timed_out_true() {
        let iq = noise(1.0, 4);
        let r = run(serde_json::json!({"bursts": 5, "timeout_ms": 300}), &iq).await;
        assert!(matches!(r.result, Ok(())), "{:?}", r.result);
        assert!(r.events.is_empty(), "{:?}", r.events);
        assert_eq!(r.context["timed_out"], true);
        assert_eq!(r.context["written"], serde_json::json!([]));
        assert_eq!(r.context["bursts_seen"], 0);
        assert!(r.store.list().is_empty());
    }

    #[test]
    fn bad_params_are_refused_naming_the_field() {
        let st = BurstCaptureStage::new();
        assert!(st.check_params(&serde_json::json!({})).is_ok());
        for field in ["bursts", "window_ms", "timeout_ms"] {
            let e = st.check_params(&serde_json::json!({field: 0})).unwrap_err();
            assert!(e.contains(field), "{e}");
        }
    }
}
