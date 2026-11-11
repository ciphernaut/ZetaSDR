//! The `scan` job: time-shares one radio across a span wider than its capture.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::bank::{BankFactory, ChannelBank};
use zeta_core::graph::{Graph, GraphError, Node as GraphNode};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{
    JobKind, Op, OpError, OpResult, ScanMethod, scan_dwell_samples, spectrum_row_boundary,
};
use zeta_core::port::{
    IqBlock, IqReceiver, Port, PortError, SWEEP_ROWS_CAPACITY, SweepPlan, SweepRequest, SweepRow,
    SweepRows, SweepState, Tuning,
};
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageRegistry, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{
    Capabilities, Claim, Depth, Hz, JobId, SampleFormat, SourceId, Span, StreamId, StreamKind,
    SweepCaps, Timestamp,
};

const MHZ: u64 = 1_000_000;

/// The tone offset the port serves at `center_hz`, so each step has its own.
fn tone_hz(center_hz: u64) -> f64 {
    (center_hz / 100) as f64
}

/// Cadence of the port's blocks.
const BLOCK_MS: u64 = 5;

/// Signal time in one block of a [`Delivery::Fast`] port: more than a 200 ms dwell,
/// delivered every `BLOCK_MS`.
const FAST_BLOCK_MS: u64 = 250;

/// Capture width of a [`ScanPort::fast`] port: narrow, so its long blocks stay cheap
/// (and a 200 ms dwell is 50 whole frames of 1024).
const FAST_RATE_HZ: u64 = 256_000;

/// How a [`ScanPort`] delivers its blocks while tuned, one every `BLOCK_MS`.
#[derive(Clone, Copy, Debug)]
enum Delivery {
    /// `BLOCK_MS` of samples each, in real time.
    Real,
    /// `FAST_BLOCK_MS` of samples each: a whole dwell arrives at once.
    Fast,
    /// As `Real` for this many blocks, then none at all.
    Stalling(u64),
    /// As `Real`, but every third block starts 1 ms late: samples lost, a gap in the ring.
    Gapping,
}

impl Delivery {
    fn block_ms(self) -> u64 {
        match self {
            Delivery::Fast => FAST_BLOCK_MS,
            _ => BLOCK_MS,
        }
    }
}

/// Samples in one `block_ms` block at `rate_hz`.
fn block_len(rate_hz: u64, block_ms: u64) -> usize {
    (rate_hz * block_ms / 1_000) as usize
}

/// A 16 MHz-wide port that records every `tune` and, while tuned, sends a
/// block every `BLOCK_MS` at its current band, carrying a tone at [`tone_hz`]
/// of that band's centre, as its [`Delivery`] says. Block timestamps advance by
/// exactly the block's signal time, so the ring sees one contiguous run per
/// step and a dwell reads several blocks (but where `Gapping` skips). With
/// `fail_tune`, that tune (1-based) and every later one fails (only that one
/// with `fail_once`). Its record can be
/// replaced once ([`ScanPort::replace_caps`]), as a worker port's is by a
/// respawn's hello. One made [`ScanPort::with_depths`] carries two sample
/// depths, changes depth as a Soapy source does (the tuning forgotten), bounds a
/// tune by the depth in force and logs every `set_depth` and tune in order;
/// the cap of its depth in force can drop under the record's after a given
/// tune ([`ScanPort::drop_cap_after`]), as a re-measure's does.
struct ScanPort {
    id: SourceId,
    caps: Capabilities,
    later: OnceLock<Capabilities>,
    cur: Mutex<Option<Tuning>>,
    tunes: Mutex<Vec<(Tuning, Instant)>>,
    fail_tune: Option<usize>,
    fail_once: bool,
    /// Whether a `fail_once` tune has failed.
    failed: AtomicBool,
    /// The depth in force (bits), on a port with depths.
    depth: Mutex<Option<u8>>,
    /// After this many tunes, the cap of the depth in force becomes this (Hz).
    cap_drop: Mutex<Option<(usize, u64)>>,
    /// The cap of the depth in force once it dropped; the record keeps its own.
    live_cap: Mutex<Option<u64>>,
    /// Every `set_depth` and tune, in order.
    log: Mutex<Vec<Call>>,
    /// The `t0` of every block a `Gapping` port started late.
    gaps: Mutex<Vec<u64>>,
    /// While set, no block is sent ([`ScanPort::hold`]).
    held: AtomicBool,
    tx: broadcast::Sender<IqBlock>,
    /// How its own sweep goes, on a port made [`ScanPort::sweeping`].
    script: Option<SweepScript>,
    /// Every sweep asked for, in order.
    requests: Mutex<Vec<SweepRequest>>,
    /// Sweeps whose rows are not dropped yet.
    live: Arc<AtomicUsize>,
    /// Rows a full channel refused.
    dropped: Arc<AtomicUsize>,
    /// How many times the port was idled.
    idles: AtomicUsize,
    /// What its sweep is refused with now: the script's `refuse`, then
    /// [`ScanPort::set_refuse`]'s.
    refuse: Mutex<Option<String>>,
}

/// How a [`ScanPort::sweeping`] port's own sweep goes: it refuses with `refuse`;
/// after `wait_after` rows it says `Waiting("socket lost")` for 300 ms and sends
/// nothing meanwhile; after `end_after` rows it says `Ended("gone")` and closes;
/// it sends no row for step `skip_step`; with `burst` it sends [`SWEEP_BURST`] rows
/// at once (more than its channel holds), then pauses, so a reader cannot keep up
/// and cannot be starved; with `depths` the port also carries [`two_depths`]. The
/// first row comes after `first_row_ms` (default 5); with `hang` the call never
/// returns, and with `start_ms` it returns (or refuses) only after that long. Each
/// row's span is the next `SWEEP_ROW_MS` after the last one's.
#[derive(Clone, Debug, Default)]
struct SweepScript {
    refuse: Option<String>,
    wait_after: Option<usize>,
    end_after: Option<usize>,
    skip_step: Option<u32>,
    burst: bool,
    depths: bool,
    first_row_ms: Option<u64>,
    hang: bool,
    start_ms: Option<u64>,
}

/// Steps and bins of a [`ScanPort::sweeping`] port's plan.
const SWEEP_STEPS: u32 = 4;
const SWEEP_BINS: u32 = 1024;
/// The cadence of its rows.
const SWEEP_ROW_MS: u64 = 5;
/// The rows a `burst` sends back to back: twice what the channel holds.
const SWEEP_BURST: usize = 2 * SWEEP_ROWS_CAPACITY;

/// A call on a [`ScanPort`], as its log keeps it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Call {
    SetDepth(u8),
    Tune(Tuning),
}

/// The depths of a [`ScanPort::with_depths`] port: 16 bits lossless at 16 MHz
/// (its own) and 8 bits lossy at 32 MHz.
fn two_depths() -> Vec<Depth> {
    vec![
        Depth {
            bits: 16,
            lossless: true,
            max_bandwidth_hz: Claim::Measured(Hz(16 * MHZ)),
        },
        Depth {
            bits: 8,
            lossless: false,
            max_bandwidth_hz: Claim::Measured(Hz(32 * MHZ)),
        },
    ]
}

/// One `block_ms` block's samples at `t`: a whole number of tone cycles for
/// every step centre used here (8, 24 and 40 MHz at 16 MHz), so blocks join
/// without a phase jump.
fn block_samples(t: Tuning, block_ms: u64) -> Arc<[Complex<f32>]> {
    let rate = t.bandwidth_hz.0;
    let w = 2.0 * std::f64::consts::PI * tone_hz(t.center_hz.0) / rate as f64;
    (0..block_len(rate, block_ms))
        .map(|n| {
            let ph = w * n as f64;
            Complex::new(ph.cos() as f32, ph.sin() as f32)
        })
        .collect()
}

impl ScanPort {
    fn new() -> Arc<Self> {
        Self::with_caps(false, false)
    }

    /// A port whose capabilities say `remote` and `push` as given.
    fn with_caps(remote: bool, push: bool) -> Arc<Self> {
        Self::build(None, remote, push, Delivery::Real, 16 * MHZ)
    }

    fn with_fail_tune(fail_tune: Option<usize>) -> Arc<Self> {
        Self::build(fail_tune, false, false, Delivery::Real, 16 * MHZ)
    }

    /// A `FAST_RATE_HZ`-wide port whose every block holds `FAST_BLOCK_MS` of samples.
    fn fast() -> Arc<Self> {
        Self::build(None, false, false, Delivery::Fast, FAST_RATE_HZ)
    }

    /// Stop sending blocks while `on`, as a port that has stalled; they resume when it is off.
    fn hold(&self, on: bool) {
        self.held.store(on, Ordering::SeqCst);
    }

    /// A port that sends `after_blocks` blocks, then stops delivering.
    fn stalling(after_blocks: u64) -> Arc<Self> {
        Self::build(
            None,
            false,
            false,
            Delivery::Stalling(after_blocks),
            16 * MHZ,
        )
    }

    /// A port whose every third block skips 1 ms of `t0`.
    fn gapping() -> Arc<Self> {
        Self::build(None, false, false, Delivery::Gapping, 16 * MHZ)
    }

    /// A 16 MHz port at 16 bits that also carries 8 bits at 32 MHz
    /// ([`two_depths`]); with `fail_tune`, that tune (1-based) fails, once.
    fn with_depths(fail_tune: Option<usize>) -> Arc<Self> {
        Self::build_with(fail_tune, false, false, Delivery::Real, 16 * MHZ, |me| {
            me.caps.depths = two_depths();
            me.caps.max_bandwidth_hz = Claim::Measured(Hz(16 * MHZ));
            *me.depth.get_mut().unwrap() = Some(16);
            me.fail_once = true;
        })
    }

    /// A port that sweeps for itself across its whole tune range, as `script`
    /// says: `bin_hz_min` 1 kHz, `calibrated`. Its plan is [`SWEEP_STEPS`] steps
    /// of [`SWEEP_BINS`] bins, a row every [`SWEEP_ROW_MS`], step by step, each
    /// step's bins at one level (`-100 + 10 * step` dB), bins 0 and 1 masked.
    fn sweeping(script: SweepScript) -> Arc<Self> {
        Self::build_with(None, false, false, Delivery::Real, 16 * MHZ, |me| {
            me.caps.sweep = Some(SweepCaps {
                min_hz: Hz(MHZ),
                max_hz: Hz(6_000 * MHZ),
                bin_hz_min: Hz(1000),
                rows_per_s_max: 200,
                calibrated: true,
            });
            if script.depths {
                me.caps.depths = two_depths();
                me.caps.max_bandwidth_hz = Claim::Measured(Hz(16 * MHZ));
                *me.depth.get_mut().unwrap() = Some(16);
            }
            *me.refuse.get_mut().unwrap() = script.refuse.clone();
            me.script = Some(script);
        })
    }

    /// Every sweep asked for so far.
    fn requests(&self) -> Vec<SweepRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Sweeps whose rows are not dropped yet.
    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Rows a full channel refused.
    fn dropped(&self) -> usize {
        self.dropped.load(Ordering::SeqCst)
    }

    /// How many times the port was idled.
    fn idles(&self) -> usize {
        self.idles.load(Ordering::SeqCst)
    }

    /// Refuse every sweep asked for from now on with `why` (`None`: grant them).
    fn set_refuse(&self, why: Option<&str>) {
        *self.refuse.lock().unwrap() = why.map(Into::into);
    }

    /// Every `set_depth` and tune so far, in order.
    fn calls(&self) -> Vec<Call> {
        self.log.lock().unwrap().clone()
    }

    fn build(
        fail_tune: Option<usize>,
        remote: bool,
        push: bool,
        delivery: Delivery,
        max_bandwidth_hz: u64,
    ) -> Arc<Self> {
        Self::build_with(fail_tune, remote, push, delivery, max_bandwidth_hz, |_| {})
    }

    /// [`ScanPort::build`], the port changed by `f` before it starts sending.
    fn build_with(
        fail_tune: Option<usize>,
        remote: bool,
        push: bool,
        delivery: Delivery,
        max_bandwidth_hz: u64,
        f: impl FnOnce(&mut Self),
    ) -> Arc<Self> {
        let mut p = Self {
            id: SourceId("radio0".into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(MHZ)),
                tune_max_hz: Claim::Asserted(Hz(6_000 * MHZ)),
                max_bandwidth_hz: Claim::Asserted(Hz(max_bandwidth_hz)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote,
                push,
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
            },
            later: OnceLock::new(),
            cur: Mutex::new(None),
            tunes: Mutex::new(Vec::new()),
            fail_tune,
            fail_once: false,
            failed: AtomicBool::new(false),
            depth: Mutex::new(None),
            cap_drop: Mutex::new(None),
            live_cap: Mutex::new(None),
            log: Mutex::new(Vec::new()),
            gaps: Mutex::new(Vec::new()),
            held: AtomicBool::new(false),
            tx: broadcast::channel(16).0,
            script: None,
            requests: Mutex::new(Vec::new()),
            live: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
            idles: AtomicUsize::new(0),
            refuse: Mutex::new(None),
        };
        f(&mut p);
        let p = Arc::new(p);
        let weak = Arc::downgrade(&p);
        tokio::spawn(async move {
            let base = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            let block_ms = delivery.block_ms();
            let mut tick = tokio::time::interval(Duration::from_millis(BLOCK_MS));
            let mut cache: Vec<(Tuning, Arc<[Complex<f32>]>)> = Vec::new();
            let mut sent = 0u64;
            for k in 0u64.. {
                tick.tick().await;
                let Some(p) = weak.upgrade() else { return };
                let Some(t) = p.current() else { continue };
                if matches!(delivery, Delivery::Stalling(n) if sent >= n) {
                    continue;
                }
                if p.held.load(Ordering::SeqCst) {
                    continue;
                }
                let samples = match cache.iter().find(|(c, _)| *c == t) {
                    Some((_, s)) => s.clone(),
                    None => {
                        let s = block_samples(t, block_ms);
                        cache.push((t, s.clone()));
                        s
                    }
                };
                let mut t0 = base + k * block_ms * 1_000_000;
                if matches!(delivery, Delivery::Gapping) {
                    t0 += (k / 3) * 1_000_000;
                    if k > 0 && k % 3 == 0 {
                        p.gaps.lock().unwrap().push(t0);
                    }
                }
                let _ = p.tx.send(IqBlock {
                    t0: Timestamp(t0),
                    sample_rate_hz: t.bandwidth_hz,
                    center_hz: t.center_hz,
                    samples,
                });
                sent += 1;
            }
        });
        p
    }

    /// Replace the port's record with its own changed by `f`, once.
    fn replace_caps(&self, f: impl FnOnce(&mut Capabilities)) {
        let mut caps = self.caps.clone();
        f(&mut caps);
        self.later.set(caps).expect("replaced once");
    }

    fn tunes(&self) -> Vec<Tuning> {
        self.tunes.lock().unwrap().iter().map(|(t, _)| *t).collect()
    }

    /// Once `n` tunes have been made, the cap of the depth in force is
    /// `hz` (set inside the `n`th tune, so no sweep sees half of it); the
    /// record is not changed.
    fn drop_cap_after(&self, n: usize, hz: u64) {
        *self.cap_drop.lock().unwrap() = Some((n, hz));
    }

    /// When each tune was asked for.
    fn tuned_at(&self) -> Vec<Instant> {
        self.tunes
            .lock()
            .unwrap()
            .iter()
            .map(|(_, at)| *at)
            .collect()
    }
}

#[async_trait]
impl Port for ScanPort {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        self.later.get().unwrap_or(&self.caps)
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let n = self.tunes.lock().unwrap().len() + 1;
        let fails = match self.fail_tune {
            Some(f) if self.fail_once => n == f && !self.failed.swap(true, Ordering::SeqCst),
            Some(f) => n >= f,
            None => false,
        };
        if fails {
            return Err(PortError::Io(format!("tune {n} refused")));
        }
        if let Some(d) = self.depth()
            && t.bandwidth_hz.0 > d.max_bandwidth_hz.value().0
        {
            return Err(PortError::OutOfRange {
                field: "bandwidth_hz",
                value: t.bandwidth_hz.0,
                min: 1,
                max: d.max_bandwidth_hz.value().0,
                suggested: None,
            });
        }
        self.tunes.lock().unwrap().push((t, Instant::now()));
        self.log.lock().unwrap().push(Call::Tune(t));
        *self.cur.lock().unwrap() = Some(t);
        if let Some((after, hz)) = *self.cap_drop.lock().unwrap()
            && n >= after
        {
            *self.live_cap.lock().unwrap() = Some(hz);
        }
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
    fn depth(&self) -> Option<Depth> {
        let bits = (*self.depth.lock().unwrap())?;
        let mut d = self.caps.depths.iter().find(|d| d.bits == bits).cloned()?;
        if let Some(hz) = *self.live_cap.lock().unwrap() {
            d.max_bandwidth_hz = Claim::Measured(Hz(hz));
        }
        Some(d)
    }
    async fn set_depth(&self, bits: u8) -> Result<Depth, PortError> {
        let Some(d) = self.caps.depths.iter().find(|d| d.bits == bits).cloned() else {
            return Err(PortError::Unsupported(format!("no {bits}-bit depth")));
        };
        // As a Soapy source: the stream closes and the tuning is forgotten.
        *self.depth.lock().unwrap() = Some(bits);
        *self.cur.lock().unwrap() = None;
        self.log.lock().unwrap().push(Call::SetDepth(bits));
        Ok(d)
    }
    async fn idle(&self) {
        self.idles.fetch_add(1, Ordering::SeqCst);
    }
    async fn sweep(&self, req: SweepRequest) -> Result<SweepRows, PortError> {
        let Some(script) = self.script.clone() else {
            return Err(PortError::Unsupported(
                "source does not sweep for itself".into(),
            ));
        };
        self.requests.lock().unwrap().push(req);
        if let Some(ms) = script.start_ms {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
        let refuse = self.refuse.lock().unwrap().clone();
        if let Some(why) = refuse {
            return Err(PortError::Unsupported(why));
        }
        if script.hang {
            std::future::pending::<()>().await;
        }
        let step_hz = req.span.bandwidth_hz.0 / u64::from(SWEEP_STEPS);
        let bin_hz = step_hz as f64 / f64::from(SWEEP_BINS);
        let lo = req.span.center_hz.0 - req.span.bandwidth_hz.0 / 2;
        let plan = SweepPlan {
            steps: SWEEP_STEPS,
            step_hz: Hz(step_hz),
            size: SWEEP_BINS,
            bin_hz,
            dwell_ms: req.dwell_ms,
            shift_hz: Hz(0),
        };
        let (tx, rows) = tokio::sync::mpsc::channel(SWEEP_ROWS_CAPACITY);
        let (state_tx, state) = tokio::sync::watch::channel(SweepState::Running);
        let (live, dropped) = (self.live.clone(), self.dropped.clone());
        live.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let pause = |ms: u64| {
                let tx = tx.clone();
                async move {
                    tokio::select! {
                        _ = tx.closed() => false,
                        _ = tokio::time::sleep(Duration::from_millis(ms)) => true,
                    }
                }
            };
            let base = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            let (mut sent, mut waited) = (0usize, false);
            let mut first = Some(script.first_row_ms.unwrap_or(SWEEP_ROW_MS));
            'sweeps: for sweep in 0u64.. {
                for step in 0..SWEEP_STEPS {
                    if script.skip_step == Some(step) {
                        continue;
                    }
                    if script.wait_after == Some(sent) && !waited {
                        waited = true;
                        state_tx.send_replace(SweepState::Waiting("socket lost".into()));
                        if !pause(300).await {
                            break 'sweeps;
                        }
                        state_tx.send_replace(SweepState::Running);
                    }
                    if script.end_after == Some(sent) {
                        state_tx.send_replace(SweepState::Ended("gone".into()));
                        break 'sweeps;
                    }
                    let ms = first.take().unwrap_or(SWEEP_ROW_MS);
                    let in_burst = script.burst && !sent.is_multiple_of(SWEEP_BURST);
                    if !in_burst && !pause(ms).await {
                        break 'sweeps;
                    }
                    let end_ns = base + (sent as u64 + 1) * SWEEP_ROW_MS * 1_000_000;
                    let row = SweepRow {
                        sweep,
                        step,
                        center_hz: Hz(lo + step_hz * u64::from(step) + step_hz / 2),
                        bin_hz,
                        db: vec![-100.0 + 10.0 * step as f32; SWEEP_BINS as usize],
                        masked: vec![[0, 2]],
                        span: Span {
                            start_ns: Timestamp(end_ns - SWEEP_ROW_MS * 1_000_000),
                            end_ns: Timestamp(end_ns),
                        },
                    };
                    match tx.try_send(row) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            dropped.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break 'sweeps,
                    }
                    sent += 1;
                }
            }
            drop(tx);
            drop(state_tx);
            live.fetch_sub(1, Ordering::SeqCst);
        });
        Ok(SweepRows { plan, rows, state })
    }
}

/// Stands in for the dsp `fft` stage (core cannot depend on it). It cuts each
/// input block into rows as the real stage does: `row_frames` frames of `size`
/// each when the params give it, else closing row `k` at the first frame ending
/// at or after [`spectrum_row_boundary`]; a partial row at the block's end is dropped. One event per row with
/// the params' `size`, `rows_per_s`, `row_frames`, `notch_hz` and `edge_mask`,
/// the block's `center_hz`, the row's sample count and the tone it carries (mean
/// phase step), and a quantised row of [`FAKE_BINS`] bins at [`FAKE_DB`] dB
/// (`floor_db` [`FAKE_FLOOR_DB`], `step_db` 0.5); the frame's span is the row's. Its params check refuses an
/// `edge_mask` outside `0..0.5` naming the field, as the real stage does. The
/// slow one sleeps `delay` before each run.
struct FakeFft {
    spec: StageSpec,
    delay: Duration,
}

/// Bins of a [`FakeFft`] row's `q`, each at `FAKE_DB` dB above a floor of
/// `FAKE_FLOOR_DB` (q 20 at 0.5 dB a step).
const FAKE_BINS: usize = 16;
const FAKE_FLOOR_DB: f64 = -100.0;
const FAKE_DB: f64 = -90.0;

#[async_trait]
impl Stage for FakeFft {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    fn check_params(&self, params: &Value) -> Result<(), String> {
        match params.get("edge_mask").and_then(Value::as_f64) {
            Some(e) if !(0.0..0.5).contains(&e) => Err("edge_mask must be in 0..0.5".into()),
            _ => Ok(()),
        }
    }
    async fn run(
        &self,
        params: Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        tokio::select! {
            _ = cancel.cancelled() => return Err(StageError::Cancelled),
            _ = tokio::time::sleep(self.delay) => {}
        }
        let size = params["size"].as_u64().expect("a size") as usize;
        let rx = &mut inputs[0];
        loop {
            let frame = tokio::select! {
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(f) => f,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            };
            let Payload::Iq(b) = &frame.payload else {
                continue;
            };
            let rate = b.sample_rate_hz.0;
            let rows_per_s = params["rows_per_s"].as_u64().unwrap_or(20);
            // Where each row ends, in samples: `row_frames` frames each, else (as the real
            // stage) row `k` closes at the first frame ending at or after its boundary.
            let frames = b.samples.len() / size;
            let mut ends = Vec::new();
            let mut row = 0u64;
            for f in 1..=frames {
                let end = (f * size) as u64;
                let closes = match params["row_frames"].as_u64() {
                    Some(n) => (f as u64).is_multiple_of(n),
                    None => end >= spectrum_row_boundary(row, rate, rows_per_s),
                };
                if closes {
                    ends.push(end as usize);
                    row += 1;
                    while spectrum_row_boundary(row, rate, rows_per_s) <= end {
                        row += 1;
                    }
                }
            }
            let ns = |n: usize| (n as u128 * 1_000_000_000 / rate as u128) as u64;
            let mut from = 0;
            for end in ends {
                let s = &b.samples[from..end];
                let acc: Complex<f64> = s
                    .windows(2)
                    .map(|w| {
                        let p = w[1] * w[0].conj();
                        Complex::new(p.re as f64, p.im as f64)
                    })
                    .sum();
                let tone = acc.arg() * rate as f64 / (2.0 * std::f64::consts::PI);
                let ev = serde_json::json!({
                    "size": params["size"],
                    "rows_per_s": params["rows_per_s"],
                    "row_frames": params["row_frames"],
                    "notch_hz": params["notch_hz"].clone(),
                    "edge_mask": params["edge_mask"].clone(),
                    "center_hz": b.center_hz.0,
                    "samples": s.len(),
                    "tone_hz": tone,
                    "floor_db": FAKE_FLOOR_DB,
                    "step_db": 0.5,
                    "q": vec![((FAKE_DB - FAKE_FLOOR_DB) / 0.5) as u8; FAKE_BINS],
                });
                let mut prov = frame.provenance.clone();
                prov.span = Span {
                    start_ns: Timestamp(b.t0.0 + ns(from)),
                    end_ns: Timestamp(b.t0.0 + ns(end)),
                };
                bus.publish(Frame::new(outputs[0].clone(), prov, Payload::Event(ev)));
                from = end;
            }
        }
    }
}

fn registry() -> StageRegistry {
    registry_with(Duration::ZERO)
}

/// A registry whose `fft` sleeps `delay` before each run.
fn registry_with(delay: Duration) -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(FakeFft {
        spec: StageSpec {
            kind: "fft",
            inputs: vec![StreamKind::Iq],
            outputs: vec![StreamKind::Event],
            params_schema: schemars::json_schema!({}),
        },
        delay,
    }));
    r
}

fn cfg_dwell(dir: &Path, scan_dwell_ms: u64) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        backends: vec![Backend::Cpu],
        resume: true,
        scan_dwell_ms,
        bank_factory: None,
        modes_dir: None,
    }
}

fn start(kind: JobKind, center_hz: u64, bandwidth_hz: u64) -> Op {
    Op::JobStart {
        kind,
        source: SourceId("radio0".into()),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        elastic: false,
        replace: vec![],
    }
}

/// A scan's `job_start` of `bandwidth_hz` about `center_hz` with a look.
fn start_look(
    center_hz: u64,
    bandwidth_hz: u64,
    dwell_ms: Option<u64>,
    average: Option<u32>,
) -> Op {
    let mut op = start(JobKind::Scan, center_hz, bandwidth_hz);
    if let Op::JobStart {
        dwell_ms: d,
        average: a,
        ..
    } = &mut op
    {
        (*d, *a) = (dwell_ms, average);
    }
    op
}

/// `op` (a `job_start`) with `method` and `bin_hz`.
fn with_method(mut op: Op, method: Option<ScanMethod>, bin_hz: Option<u64>) -> Op {
    if let Op::JobStart {
        method: m,
        bin_hz: b,
        ..
    } = &mut op
    {
        (*m, *b) = (method, bin_hz);
    }
    op
}

/// Start a scan with a look; its id and a receiver on its event stream.
async fn start_scan_look(
    node: &Node,
    (center_hz, bandwidth_hz): (u64, u64),
    dwell_ms: Option<u64>,
    average: Option<u32>,
) -> (JobId, broadcast::Receiver<Frame>) {
    let id = match node
        .apply(start_look(center_hz, bandwidth_hz, dwell_ms, average))
        .await
    {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    };
    let rx = event_rx(node, &id);
    (id, rx)
}

async fn open() -> (tempfile::TempDir, Node, Arc<ScanPort>) {
    open_with(ScanPort::new()).await
}

async fn open_with(port: Arc<ScanPort>) -> (tempfile::TempDir, Node, Arc<ScanPort>) {
    open_dwell(port, 20).await
}

async fn open_dwell(
    port: Arc<ScanPort>,
    dwell_ms: u64,
) -> (tempfile::TempDir, Node, Arc<ScanPort>) {
    open_full(port, dwell_ms, registry()).await
}

async fn open_full(
    port: Arc<ScanPort>,
    dwell_ms: u64,
    registry: StageRegistry,
) -> (tempfile::TempDir, Node, Arc<ScanPort>) {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg_dwell(dir.path(), dwell_ms),
        registry,
        vec![port.clone()],
    )
    .await
    .unwrap();
    (dir, node, port)
}

/// Start a scan over 0..40 MHz; its id and a receiver on its event stream.
async fn start_scan(node: &Node) -> (JobId, broadcast::Receiver<Frame>) {
    start_span(node, 20 * MHZ, 40 * MHZ).await
}

/// Start a scan of `bandwidth_hz` about `center_hz`; its id and a receiver on its event stream.
async fn start_span(
    node: &Node,
    center_hz: u64,
    bandwidth_hz: u64,
) -> (JobId, broadcast::Receiver<Frame>) {
    let id = match node
        .apply(start(JobKind::Scan, center_hz, bandwidth_hz))
        .await
    {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    };
    let rx = event_rx(node, &id);
    (id, rx)
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The next `n` rows on `rx` (lagged frames skipped), failing after `within`.
async fn rows(rx: &mut broadcast::Receiver<Frame>, n: usize, within: Duration) -> Vec<Frame> {
    let deadline = tokio::time::Instant::now() + within;
    let mut got = Vec::new();
    while got.len() < n {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(f)) => got.push(f),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(e)) => panic!("the scan's stream ended: {e}"),
            Err(_) => panic!("{} rows, wanted {n}", got.len()),
        }
    }
    got
}

/// A row's event.
fn ev(f: &Frame) -> &Value {
    match &f.payload {
        Payload::Event(v) => v,
        other => panic!("scan stream carries events, got {other:?}"),
    }
}

/// A row's channel, `scan/<step>`.
fn channel(f: &Frame) -> &str {
    &f.provenance.channel.as_ref().expect("a step channel").0
}

/// A receiver on job `id`'s event stream.
fn event_rx(node: &Node, id: &JobId) -> broadcast::Receiver<Frame> {
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.kind == StreamKind::Event && s.provenance.job.as_ref() == Some(id))
        .expect("the job has an event stream");
    node.bus().subscribe(&stream.id).unwrap()
}

/// Wait until `port` has been tuned at least `n` times, failing after `within`.
async fn tunes_at_least(port: &ScanPort, n: usize, within: Duration) {
    let deadline = tokio::time::Instant::now() + within;
    while port.tunes().len() < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} tunes, wanted {n}",
            port.tunes().len()
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// The graph's first `fft` node.
fn fft_node(g: &Graph) -> &GraphNode {
    g.nodes
        .iter()
        .find(|n| n.kind == "fft")
        .expect("an fft node")
}

/// Job `id`'s graph, through `graph_get`.
async fn graph_of(node: &Node, id: &JobId) -> Graph {
    match node.apply(Op::GraphGet { job: id.clone() }).await {
        Ok(OpResult::Graph(g)) => g,
        other => panic!("expected a graph, got {other:?}"),
    }
}

fn source_info(r: OpResult) -> (Option<Tuning>, Vec<JobId>) {
    match r {
        OpResult::Sources(v) => (v[0].current, v[0].holders.clone()),
        other => panic!("expected sources, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_steps_the_port_across_the_span_and_emits_rows() {
    let (_dir, node, port) = open().await;
    let (id, mut rx) = start_scan(&node).await;
    // 0..40 MHz in 16 MHz steps; the first capture is kept above the 1 MHz tune floor.
    // Even sweeps step 9, 24, 40 MHz, odd ones 9, 20, 36 MHz.
    let centres = [9 * MHZ, 24 * MHZ, 40 * MHZ, 20 * MHZ, 36 * MHZ];
    let mut seen = [0usize; 5];
    let mut most = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Ok(f)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        let Payload::Event(ev) = &f.payload else {
            panic!("scan stream carries events")
        };
        assert_eq!(f.provenance.job.as_ref(), Some(&id));
        let c = ev["center_hz"].as_u64().unwrap();
        let i = centres.iter().position(|&x| x == c).expect("a step centre");
        // spectrum_size(16 MHz) = 4096.
        assert_eq!(ev["size"], 4096);
        assert!(ev["samples"].as_u64().unwrap() >= 4096, "{ev}");
        most = most.max(ev["samples"].as_u64().unwrap());
        let tone = ev["tone_hz"].as_f64().unwrap();
        assert!((tone - tone_hz(c)).abs() < 1_000.0, "step {i}: {ev}");
        seen[i] += 1;
    }
    assert!(seen[..3].iter().all(|&n| n >= 2), "rows per step {seen:?}");
    // The ring held a contiguous run, so a dwell read spanned several blocks.
    let one = block_len(16 * MHZ, BLOCK_MS) as u64;
    eprintln!("largest dwell read {most} samples, one block {one}");
    assert!(
        most >= 2 * one,
        "largest dwell read {most} samples, one block {one}"
    );

    let tunes = port.tunes();
    assert!(tunes.len() >= 6, "{} tunes", tunes.len());
    for t in &tunes {
        assert_eq!(t.bandwidth_hz.0, 16 * MHZ);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn job_start_during_scan_is_a_conflict_naming_it() {
    let (_dir, node, _port) = open().await;
    let (id, _rx) = start_scan(&node).await;
    // Inside the span, and inside the step the port happens to be at: still refused.
    for op in [
        start(JobKind::Spectrum, 8 * MHZ, MHZ),
        start(JobKind::FmDemod, 24 * MHZ, 12_500),
        Op::SourceEnsure {
            source: SourceId("radio0".into()),
            center_hz: Hz(8 * MHZ),
            bandwidth_hz: Hz(MHZ),
            replace: vec![],
        },
    ] {
        match node.apply(op).await {
            Err(OpError::Conflict { holders, .. }) => assert_eq!(holders, vec![id.clone()]),
            other => panic!("expected a conflict naming the scan, got {other:?}"),
        }
    }
    let (_, holders) = source_info(node.apply(Op::SourceList {}).await.unwrap());
    assert_eq!(holders, vec![id]);
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_stop_mid_sweep_releases() {
    let (_dir, node, port) = open().await;
    let (id, _rx) = start_scan(&node).await;
    tokio::time::sleep(Duration::from_millis(130)).await;
    node.apply(Op::JobStop { job: id }).await.unwrap();
    let (current, holders) = source_info(node.apply(Op::SourceList {}).await.unwrap());
    assert!(holders.is_empty());
    let last = *port.tunes().last().unwrap();
    assert_eq!(current, Some(last));
    // No retune after the stop.
    let n = port.tunes().len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(port.tunes().len(), n);
    // The radio is free again.
    assert!(matches!(
        node.apply(start(JobKind::Spectrum, 100 * MHZ, MHZ)).await,
        Ok(OpResult::Job(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn tune_fault_releases_the_scan_lease() {
    let (_dir, node, port) = open_with(ScanPort::with_fail_tune(Some(2))).await;
    let (id, _rx) = start_scan(&node).await;
    let mut state = JobState::Running;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        state = node.job_list()[0].state.clone();
        if state != JobState::Running {
            break;
        }
    }
    match state {
        JobState::Failed { reason } => {
            assert!(reason.contains("tune 2 refused"), "{reason}");
            assert!(reason.contains("radio0"), "{reason}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(port.tunes().len(), 1);
    let (_, holders) = source_info(node.apply(Op::SourceList {}).await.unwrap());
    assert!(holders.is_empty(), "{holders:?}");
    assert!(node.lease_holders(&SourceId("radio0".into())).is_empty());
    assert!(
        node.bus()
            .list()
            .iter()
            .all(|s| s.provenance.job.as_ref() != Some(&id)),
        "the failed scan's streams are gone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_list_stays_flat_across_sweeps() {
    let (_dir, node, port) = open_dwell(ScanPort::new(), 100).await;
    let (_id, _rx) = start_scan(&node).await;
    // The first count is taken as the scan starts, inside its first 100 ms
    // dwell, before any step's fft has run. One step's fft runs beside the
    // next step's wait, so after the sixth tune (the last step of one even and
    // one odd sweep, three steps each) the count comes back to it once that
    // fft has ended, well inside a dwell; per-step streams left registered
    // never would.
    let before = node.bus().list().len();
    until("six tunes", Duration::from_secs(10), || {
        port.tunes().len() >= 6
    })
    .await;
    until(
        "the stream count comes back to the first one",
        Duration::from_secs(5),
        || node.bus().list().len() == before,
    )
    .await;
}

/// Counts `make` calls and declines every band.
#[derive(Default)]
struct CountingFactory(std::sync::atomic::AtomicUsize);

impl BankFactory for CountingFactory {
    fn make(&self, _: Hz, _: Hz) -> Option<Arc<dyn ChannelBank>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        None
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_bank_is_made_per_step_while_a_scan_holds_the_source() {
    let (_dir, node, port) = open().await;
    let factory = Arc::new(CountingFactory::default());
    node.set_bank_factory(factory.clone());
    let makes = || factory.0.load(std::sync::atomic::Ordering::SeqCst);
    let (id, _rx) = start_scan(&node).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    // Six tunes: one even and one odd sweep of three steps each.
    while port.tunes().len() < 6 {
        assert!(tokio::time::Instant::now() < deadline, "scan stalled");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(makes(), 0, "banks made during two sweeps");
    node.job_stop(&id).await.unwrap();
    // The port stays at the last step; its next block makes that band's bank.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while makes() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no bank after the scan"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(makes(), 1);
}

/// A scan's context says how many steps a sweep takes, so a reader need not
/// count rows.
#[tokio::test(flavor = "multi_thread")]
async fn scan_context_carries_its_step_count() {
    let (_dir, node, _port) = open().await;
    let (id, _rx) = start_scan(&node).await;
    let ctx = node.job_context(&id).expect("the scan has a context");
    assert_eq!(ctx["scan"]["steps"], 3, "{ctx}");
    // radio0's whole 16 MHz is usable: the stride is the capture.
    assert_eq!(ctx["scan"]["step_hz"], 16 * MHZ, "{ctx}");
    assert_eq!(ctx["scan"]["rate_hz"], 16 * MHZ, "{ctx}");
    // The harness opens the node with a 20 ms dwell: 78 frames of 4096 at 16 MHz, 19.968 ms.
    assert_eq!(ctx["scan"]["dwell_ms"], 20, "{ctx}");
    assert!(ctx["scan"].get("dwell_ms_asked").is_none(), "{ctx}");
    node.job_stop(&id).await.unwrap();
    // On a 256 kHz step a 1 ms dwell is under four frames of 1024: 16 ms (4096 samples) is the dwell.
    let (_dir, node, _port) = open_dwell(ScanPort::fast(), 1).await;
    let (id, _rx) = start_span(&node, 100 * MHZ, FAST_RATE_HZ).await;
    let ctx = node.job_context(&id).expect("the scan has a context");
    assert_eq!(ctx["scan"]["dwell_ms"], 16, "{ctx}");
    assert_eq!(ctx["scan"]["dwell_ms_asked"], 1, "{ctx}");
    node.job_stop(&id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_alternates_its_grid_between_sweeps() {
    // 0..40 MHz on radio0: even 9, 24, 40 MHz; odd 9, 20, 36 MHz (4 clamped up to 9,
    // 52 dropped: its band misses the span).
    let (_dir, node, port) = open().await;
    let (id, mut rx) = start_scan(&node).await;
    tunes_at_least(&port, 10, Duration::from_secs(5)).await;
    let want = [9, 24, 40, 9, 20, 36, 9, 24, 40, 9].map(|m| m * MHZ);
    let got: Vec<u64> = port
        .tunes()
        .iter()
        .take(10)
        .map(|t| t.center_hz.0)
        .collect();
    assert_eq!(got, want);
    assert!(port.tunes().iter().all(|t| t.bandwidth_hz.0 == 16 * MHZ));
    // Each row's channel is its index within its own sweep's list.
    let mut seen = std::collections::HashMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Ok(f)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        let Payload::Event(ev) = &f.payload else {
            panic!("events")
        };
        let c = ev["center_hz"].as_u64().unwrap();
        let ch = f.provenance.channel.as_ref().unwrap().0.clone();
        seen.insert(c, ch);
    }
    for (c, ch) in [
        (9, "scan/0"),
        (24, "scan/1"),
        (40, "scan/2"),
        (20, "scan/1"),
        (36, "scan/2"),
    ] {
        assert_eq!(
            seen.get(&(c * MHZ)).map(String::as_str),
            Some(ch),
            "{seen:?}"
        );
    }
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["steps"], 3);
    assert_eq!(ctx["scan"]["step_hz"], 16 * MHZ);
    assert_eq!(
        ctx["scan"]["alternate"],
        serde_json::json!({"shift_hz": 4 * MHZ, "steps_shifted": 3})
    );
    node.job_stop(&id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_mask_defaults_follow_the_capabilities() {
    // radio0 is local hardware (remote false, push false) and passes 0.9 of its capture
    // flat here: the notch and the unused edges are masked on a 16 MHz step.
    let port = ScanPort::new();
    port.replace_caps(|c| c.usable_fraction = Claim::Asserted(0.9));
    let (_dir, node, _port) = open_with(port).await;
    let (id, mut rx) = start_scan(&node).await;
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(
        ctx["scan"]["mask"],
        serde_json::json!({"notch_hz": 50_000, "edge_mask": 0.05})
    );
    let Payload::Event(ev) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap()
        .payload
    else {
        panic!()
    };
    assert_eq!(ev["notch_hz"], 50_000);
    assert_eq!(ev["edge_mask"], 0.05);
    let g = graph_of(&node, &id).await;
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(fft_node(&g).params["notch_hz"], 50_000);
    assert_eq!(fft_node(&g).params["edge_mask"], 0.05);
    assert_eq!(fft_node(&g).params["size"], 4096);
    node.job_stop(&id).await.unwrap();
    // A remote source whose whole capture is usable gets none.
    let (_dir, node, _port) = open_with(ScanPort::with_caps(true, false)).await;
    let (id, mut rx) = start_scan(&node).await;
    assert_eq!(
        node.job_context(&id).unwrap()["scan"]["mask"],
        serde_json::json!({"notch_hz": 0, "edge_mask": 0.0})
    );
    let Payload::Event(ev) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap()
        .payload
    else {
        panic!()
    };
    assert_eq!(ev["notch_hz"], 0);
    assert_eq!(ev["edge_mask"], 0.0);
    node.job_stop(&id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_one_step_scan_does_not_alternate() {
    // One step: the 50 kHz notch is in force (1 MHz is wider than four notches; the unit test
    // covers a step too narrow for it). At 0.9 usable a 900 kHz span asks for 1 MHz, so the
    // unused edges it masks lie outside the span; with no odd sweep or neighbour to fill a
    // wider mask in, a wider stored one is not applied.
    let port = ScanPort::new();
    port.replace_caps(|c| c.usable_fraction = Claim::Asserted(0.9));
    let (_dir, node, port) = open_with(port).await;
    let id = match node.apply(start(JobKind::Scan, 100 * MHZ, 900_000)).await {
        Ok(OpResult::Job(id)) => id,
        o => panic!("{o:?}"),
    };
    let mut rx = event_rx(&node, &id);
    for f in rows(&mut rx, 2, Duration::from_secs(3)).await {
        assert_eq!(ev(&f)["edge_mask"], 0.05, "{}", ev(&f));
    }
    tunes_at_least(&port, 3, Duration::from_secs(3)).await;
    let step = Tuning {
        center_hz: Hz(100 * MHZ),
        bandwidth_hz: Hz(MHZ),
    };
    assert!(
        port.tunes().iter().all(|t| *t == step),
        "{:?}",
        port.tunes()
    );
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["steps"], 1);
    assert_eq!(ctx["scan"]["step_hz"], 900_000, "{ctx}");
    assert_eq!(ctx["scan"]["rate_hz"], MHZ, "{ctx}");
    assert!(ctx["scan"]["alternate"].is_null(), "{ctx}");
    assert_eq!(
        ctx["scan"]["mask"],
        serde_json::json!({"notch_hz": 50_000, "edge_mask": 0.05})
    );
    let mut g = graph_of(&node, &id).await;
    let stored = fft_node(&g);
    assert_eq!(stored.params["notch_hz"], 50_000);
    assert_eq!(stored.params["edge_mask"], 0.05);
    g.nodes[0].params["edge_mask"] = serde_json::json!(0.2);
    assert!(matches!(
        node.apply(Op::GraphPut {
            job: id.clone(),
            graph: g
        })
        .await,
        Ok(OpResult::Ok)
    ));
    assert_eq!(
        node.job_context(&id).unwrap()["scan"]["mask"]["edge_mask"],
        0.05
    );
    node.job_stop(&id).await.unwrap();
}

/// A scan whose stored `fft` node has an edge mask narrower than the edges the
/// source's usable fraction leaves unused (one stored before the fraction, or
/// a `graph put`) still masks those edges; the stored node keeps its value.
#[tokio::test(flavor = "multi_thread")]
async fn a_scan_stored_with_a_narrower_edge_mask_masks_the_unused_edges() {
    let port = ScanPort::new();
    port.replace_caps(|c| c.usable_fraction = Claim::Asserted(0.9));
    let (_dir, node, port) = open_with(port).await;
    let (id, _rx) = start_scan(&node).await;
    let mut g = graph_of(&node, &id).await;
    g.nodes[0].params["edge_mask"] = serde_json::json!(0.02);
    assert!(matches!(
        node.apply(Op::GraphPut {
            job: id.clone(),
            graph: g
        })
        .await,
        Ok(OpResult::Ok)
    ));
    assert_eq!(
        fft_node(&graph_of(&node, &id).await).params["edge_mask"],
        0.02
    );
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(
        ctx["scan"]["mask"],
        serde_json::json!({"notch_hz": 50_000, "edge_mask": 0.05})
    );
    // 16 MHz captures 14.4 MHz apart over 0..40 MHz: 9 (7.2 clamped up), 21.6, 36 MHz.
    assert_eq!(ctx["scan"]["step_hz"], 14_400_000, "{ctx}");
    assert_eq!(ctx["scan"]["rate_hz"], 16 * MHZ, "{ctx}");
    // The restarted scan's rows (a fresh event stream) carry the wider mask.
    let mut rx = event_rx(&node, &id);
    for f in rows(&mut rx, 3, Duration::from_secs(3)).await {
        assert_eq!(ev(&f)["edge_mask"], 0.05, "{}", ev(&f));
    }
    assert!(port.tunes().iter().all(|t| t.bandwidth_hz.0 == 16 * MHZ));
    node.job_stop(&id).await.unwrap();
}

/// A scan started before the source's rate cap was measured sweeps at the
/// asserted width; the sweep after the cap is measured steps at the new one.
#[tokio::test(flavor = "multi_thread")]
async fn the_grid_follows_a_cap_measured_after_the_scan_started() {
    let (_dir, node, port) = open().await;
    let (id, _rx) = start_scan(&node).await;
    assert_eq!(node.job_context(&id).unwrap()["scan"]["steps"], 3);
    tunes_at_least(&port, 2, Duration::from_secs(3)).await;
    port.replace_caps(|c| c.max_bandwidth_hz = Claim::Measured(Hz(20 * MHZ)));
    until("a 20 MHz step", Duration::from_secs(5), || {
        port.tunes().iter().any(|t| t.bandwidth_hz.0 == 20 * MHZ)
    })
    .await;
    let tunes = port.tunes();
    let first = tunes
        .iter()
        .position(|t| t.bandwidth_hz.0 == 20 * MHZ)
        .unwrap();
    // The sweep in progress ended at the asserted width (three steps a sweep, even or odd);
    // every tune after is at the new one, on its grid: even 11 (10 clamped up), 30 MHz; odd
    // 11 (5 clamped up), 25, 45 MHz.
    assert!(first >= 3 && first % 3 == 0, "{tunes:?}");
    assert!(tunes[..first].iter().all(|t| t.bandwidth_hz.0 == 16 * MHZ));
    for t in &tunes[first..] {
        assert_eq!(t.bandwidth_hz.0, 20 * MHZ, "{tunes:?}");
        assert!(
            [11, 30, 25, 45].map(|m| m * MHZ).contains(&t.center_hz.0),
            "{tunes:?}"
        );
    }
    until("the context's new grid", Duration::from_secs(3), || {
        node.job_context(&id).unwrap()["scan"]["steps"] == 2
    })
    .await;
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["step_hz"], 20 * MHZ, "{ctx}");
    assert_eq!(ctx["scan"]["rate_hz"], 20 * MHZ, "{ctx}");
    assert_eq!(
        ctx["scan"]["alternate"],
        serde_json::json!({"shift_hz": 5 * MHZ, "steps_shifted": 3}),
        "{ctx}"
    );
    node.job_stop(&id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_put_on_a_scan_changes_its_mask() {
    let (_dir, node, _port) = open().await;
    let (id, _rx) = start_scan(&node).await;
    let mut g = graph_of(&node, &id).await;
    g.nodes[0].params["notch_hz"] = serde_json::json!(1_000_000);
    g.nodes[0].params["edge_mask"] = serde_json::json!(0.25);
    assert!(matches!(
        node.apply(Op::GraphPut {
            job: id.clone(),
            graph: g.clone()
        })
        .await,
        Ok(OpResult::Ok)
    ));
    assert_eq!(
        node.job_list().iter().find(|r| r.id == id).unwrap().state,
        JobState::Running
    );
    assert_eq!(
        node.job_context(&id).unwrap()["scan"]["mask"],
        serde_json::json!({"notch_hz": 1_000_000, "edge_mask": 0.25})
    );
    // The restarted scan's rows carry the new params (a fresh event stream: look it up again).
    let mut rx = event_rx(&node, &id);
    let Payload::Event(ev) = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
        .payload
    else {
        panic!()
    };
    assert_eq!(ev["notch_hz"], 1_000_000);
    assert_eq!(ev["edge_mask"], 0.25);
    // A bad mask is refused naming the field; the scan runs on with its mask.
    g.nodes[0].params["edge_mask"] = serde_json::json!(0.6);
    match node
        .apply(Op::GraphPut {
            job: id.clone(),
            graph: g,
        })
        .await
    {
        Err(OpError::Graph {
            detail: GraphError::BadParams { reason, .. },
        }) => assert!(reason.contains("edge_mask"), "{reason}"),
        other => panic!("expected a bad-params refusal, got {other:?}"),
    }
    assert_eq!(
        node.job_list().iter().find(|r| r.id == id).unwrap().state,
        JobState::Running
    );
    assert_eq!(
        node.job_context(&id).unwrap()["scan"]["mask"]["edge_mask"],
        0.25
    );
    node.job_stop(&id).await.unwrap();
}

/// A port that hands over a whole dwell at once is stepped as fast as it does:
/// the scan does not sleep the dwell away.
#[tokio::test(flavor = "multi_thread")]
async fn a_port_that_delivers_fast_is_stepped_fast() {
    let (_dir, node, port) = open_dwell(ScanPort::fast(), 200).await;
    // Three 256 kHz steps; a 200 ms dwell (50 frames of 1024) slept on each would take 2 s for ten.
    let (id, _rx) = start_span(&node, 100 * MHZ, 3 * FAST_RATE_HZ).await;
    tunes_at_least(&port, 10, Duration::from_secs(1)).await;
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["steps"], 3, "{ctx}");
    assert_eq!(ctx["scan"]["dwell_ms"], 200, "{ctx}");
    node.job_stop(&id).await.unwrap();
}

/// A port that stops delivering costs each step its wait (the dwell and the
/// margin), then the step is skipped: the scan keeps stepping and keeps running.
#[tokio::test(flavor = "multi_thread")]
async fn a_port_that_stops_delivering_is_skipped_not_hung() {
    let (_dir, node, port) = open_dwell(ScanPort::stalling(20), 20).await;
    let (id, _rx) = start_scan(&node).await;
    // 20 blocks are 100 ms of samples; the steps after them wait 520 ms each.
    until(
        "tunes after the port stalled",
        Duration::from_secs(15),
        || port.tunes().len() >= 8,
    )
    .await;
    assert_eq!(
        node.job_list().iter().find(|r| r.id == id).unwrap().state,
        JobState::Running
    );
    let at = port.tuned_at();
    let last = at[at.len() - 1] - at[at.len() - 2];
    assert!(
        last >= Duration::from_millis(500),
        "a step with no samples waited only {last:?}"
    );
    node.job_stop(&id).await.unwrap();
}

/// A slow fft does not reorder the rows: one step's fft at a time, in step order.
#[tokio::test(flavor = "multi_thread")]
async fn rows_come_out_in_step_order_with_a_slow_fft() {
    let (_dir, node, _port) = open_full(
        ScanPort::new(),
        20,
        registry_with(Duration::from_millis(50)),
    )
    .await;
    let (id, mut rx) = start_scan(&node).await;
    let got = rows(&mut rx, 6, Duration::from_secs(10)).await;
    let channels: Vec<&str> = got.iter().map(channel).collect();
    assert_eq!(
        channels,
        ["scan/0", "scan/1", "scan/2", "scan/0", "scan/1", "scan/2"]
    );
    // Even then odd: 9, 24, 40 MHz, then 9, 20, 36 MHz.
    let centres: Vec<u64> = got
        .iter()
        .map(|f| ev(f)["center_hz"].as_u64().unwrap())
        .collect();
    assert_eq!(centres, [9, 24, 40, 9, 20, 36].map(|m| m * MHZ));
    // Stopped while a step's fft sleeps: the step loop and the fft task both end,
    // and the lease goes.
    assert_eq!(node.debug_stage_count(), 2);
    node.job_stop(&id).await.unwrap();
    until("the scan's tasks end", Duration::from_secs(5), || {
        node.debug_stage_count() == 0
    })
    .await;
    assert!(node.lease_holders(&SourceId("radio0".into())).is_empty());
}

/// A dwell under 100 ms is one row a step: the fft stage is told to close a
/// row on the dwell's frames, and the row says `1000 / dwell_ms` rows a second.
#[tokio::test(flavor = "multi_thread")]
async fn a_short_dwell_publishes_one_row_a_step() {
    let (_dir, node, _port) = open_dwell(ScanPort::new(), 20).await;
    let (id, mut rx) = start_scan(&node).await;
    let got = rows(&mut rx, 9, Duration::from_secs(10)).await;
    for f in &got {
        let ev = ev(f);
        assert_eq!(ev["rows_per_s"], 50, "{ev}");
        // 78 frames of 4096 at 16 MHz.
        assert_eq!(ev["row_frames"], 78, "{ev}");
        assert_eq!(ev["samples"], 78 * 4096, "{ev}");
    }
    // A step with two rows would show its channel twice in a row.
    for w in got.windows(2) {
        assert_ne!(channel(&w[0]), channel(&w[1]), "two rows from one step");
    }
    node.job_stop(&id).await.unwrap();
}

/// Check that a scan with a `dwell_ms` dwell (100 ms or more) on the 16 MHz port
/// publishes `per_step` rows of about 50 ms for each step, 20 rows a second
/// within the step: two steps' rows, each step's on one channel, contiguous,
/// together spanning the whole effective dwell (no frame left in a partial row).
async fn whole_rows_per_step(dwell_ms: u64, per_step: usize) {
    let (_dir, node, _port) = open_dwell(ScanPort::new(), dwell_ms).await;
    let (id, mut rx) = start_scan(&node).await;
    assert_eq!(node.job_context(&id).unwrap()["scan"]["dwell_ms"], dwell_ms);
    let (dwell, _) = scan_dwell_samples(dwell_ms, 16 * MHZ, 4096);
    let got = rows(&mut rx, 2 * per_step, Duration::from_secs(20)).await;
    for f in &got {
        let ev = ev(f);
        assert_eq!(ev["rows_per_s"], 20, "{ev}");
        assert!(ev["row_frames"].is_null(), "{ev}");
        let span = &f.provenance.span;
        let ms = (span.end_ns.0 - span.start_ns.0) as f64 / 1e6;
        assert!((45.0..=55.0).contains(&ms), "a {ms} ms row");
    }
    let steps: Vec<&[Frame]> = got.chunks(per_step).collect();
    assert_ne!(channel(&steps[0][0]), channel(&steps[1][0]));
    for step in steps {
        assert!(
            step.iter().all(|f| channel(f) == channel(&step[0])),
            "a step's {per_step} rows on one channel"
        );
        for w in step.windows(2) {
            assert_eq!(w[1].provenance.span.start_ns, w[0].provenance.span.end_ns);
        }
        let samples: u64 = step
            .iter()
            .map(|f| ev(f)["samples"].as_u64().unwrap())
            .sum();
        assert_eq!(samples, dwell, "the step's rows hold its whole dwell");
    }
    node.job_stop(&id).await.unwrap();
}

/// A dwell of 100 ms or more is cut as a spectrum is, 20 rows a second within
/// the step: two 50 ms rows from a 100 ms dwell.
#[tokio::test(flavor = "multi_thread")]
async fn a_dwell_of_100_ms_keeps_20_rows_a_second() {
    whole_rows_per_step(100, 2).await;
}

/// A 250 ms dwell is five rows a step, and no sample the step waited for is dropped.
#[tokio::test(flavor = "multi_thread")]
async fn a_dwell_of_250_ms_is_five_whole_rows() {
    whole_rows_per_step(250, 5).await;
}

/// A scan of one step retunes to the same centre, which keeps the ring: each
/// step must still take samples newer than the last step's.
#[tokio::test(flavor = "multi_thread")]
async fn a_one_step_scan_takes_fresh_samples_each_step() {
    let (_dir, node, port) = open().await;
    let (id, mut rx) = start_span(&node, 100 * MHZ, MHZ).await;
    let got = rows(&mut rx, 8, Duration::from_secs(10)).await;
    assert!(port.tunes().len() >= 8, "{} tunes", port.tunes().len());
    for w in got.windows(2) {
        let (a, b) = (&w[0].provenance.span, &w[1].provenance.span);
        assert!(
            b.start_ns.0 >= a.end_ns.0,
            "a row starting at {} overlaps the one before, ending at {}",
            b.start_ns.0,
            a.end_ns.0
        );
    }
    node.job_stop(&id).await.unwrap();
}

/// Samples lost inside a dwell restart the ring's run, and the step's wait goes
/// on from the new run within its timeout: rows still come, none across a gap.
#[tokio::test(flavor = "multi_thread")]
async fn a_gap_inside_a_dwell_restarts_the_wait() {
    // A 10 ms dwell (39 frames of 4096) fits in the port's 15 ms runs, not in what is
    // left of a run the tune lands in.
    let (_dir, node, port) = open_dwell(ScanPort::gapping(), 10).await;
    let (id, mut rx) = start_scan(&node).await;
    let got = rows(&mut rx, 9, Duration::from_secs(10)).await;
    let gaps = port.gaps.lock().unwrap().clone();
    assert!(!gaps.is_empty());
    for f in &got {
        let (span, ev) = (&f.provenance.span, ev(f));
        assert_eq!(ev["samples"], 39 * 4096, "{ev}");
        assert!(
            !gaps
                .iter()
                .any(|&g| span.start_ns.0 < g && g < span.end_ns.0),
            "a row across a gap: {span:?}"
        );
    }
    // One row a step and no step skipped for its gap: the channels go round 0, 1, 2.
    let step = |f: &Frame| channel(f)["scan/".len()..].parse::<usize>().unwrap();
    for w in got.windows(2) {
        assert_eq!(
            step(&w[1]),
            (step(&w[0]) + 1) % 3,
            "a step skipped or doubled"
        );
    }
    node.job_stop(&id).await.unwrap();
}

/// `context.scan.timing` of job `id`, once a sweep has written it.
fn timing(node: &Node, id: &JobId) -> Option<Value> {
    node.job_context(id)
        .map(|c| c["scan"]["timing"].clone())
        .filter(Value::is_object)
}

/// The record says what the last whole sweep cost, every key of it.
#[tokio::test(flavor = "multi_thread")]
async fn the_scan_record_fills_its_timing_after_a_sweep() {
    let (_dir, node, _port) = open().await;
    let (id, _rx) = start_scan(&node).await;
    until("a sweep's timing", Duration::from_secs(10), || {
        timing(&node, &id).is_some_and(|t| t["sweep_ms"].as_f64().unwrap_or(0.0) > 0.0)
    })
    .await;
    let t = timing(&node, &id).unwrap();
    for k in [
        "at_ns",
        "sweep_ms",
        "step_ms",
        "tune_ms",
        "slow_tunes",
        "skipped",
        "fft_wait_ms",
        "hz_per_s",
    ] {
        assert!(!t[k].is_null(), "no {k}: {t}");
    }
    assert!(!t["tune_ms"]["median"].is_null() && !t["tune_ms"]["max"].is_null());
    assert_eq!(t["skipped"], 0, "{t}");
    assert!(t["hz_per_s"].as_f64().unwrap() > 0.0, "{t}");
    assert_eq!(node.job_context(&id).unwrap()["scan"]["method"], "stepped");
    node.job_stop(&id).await.unwrap();
}

/// A step whose samples never arrive is counted, not only waited out.
#[tokio::test(flavor = "multi_thread")]
async fn a_port_that_stops_delivering_counts_its_skips() {
    let (_dir, node, _port) = open_dwell(ScanPort::stalling(20), 20).await;
    let (id, _rx) = start_scan(&node).await;
    until("a skipped step", Duration::from_secs(20), || {
        timing(&node, &id).is_some_and(|t| t["skipped"].as_u64().unwrap_or(0) >= 1)
    })
    .await;
    node.job_stop(&id).await.unwrap();
}

/// A one-step scan sweeps many times a second; its record is written at most
/// once a second, compared by `at_ns` (the figures of a fake repeat).
#[tokio::test(flavor = "multi_thread")]
async fn the_timing_is_written_at_most_once_a_second() {
    let (_dir, node, _port) = open_with(ScanPort::fast()).await;
    let (id, _rx) = start_span(&node, 100 * MHZ, FAST_RATE_HZ).await;
    until("a timing", Duration::from_secs(10), || {
        timing(&node, &id).is_some()
    })
    .await;
    let mut seen: Vec<u64> = vec![];
    let end = tokio::time::Instant::now() + Duration::from_millis(2500);
    while tokio::time::Instant::now() < end {
        if let Some(at) = timing(&node, &id).and_then(|t| t["at_ns"].as_u64())
            && seen.last() != Some(&at)
        {
            seen.push(at);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen.len() >= 2, "{seen:?}");
    for w in seen.windows(2) {
        assert!(w[1] - w[0] >= 1_000_000_000, "{seen:?}");
    }
    node.job_stop(&id).await.unwrap();
}

/// A skip in a sweep the throttle does not write still shows at the next write:
/// the one-step sweeps between two writes add their skips up.
///
/// The port is held right after a write: once the ring's backlog is used up
/// one step waits out its 520 ms timeout, a skip in a sweep that ends inside
/// the second the throttle holds the record back. That step is found from the
/// port's own tune times (two tunes 450 ms apart), never from the gaps between
/// this test's polls; the samples come back then. The write that reports the
/// skip must be of a later, quick sweep (`sweep_ms` under the timeout): a
/// write of the skipped sweep itself (the hold came late) shows nothing of the
/// carry, so the hold is tried again, up to five times, and the test fails if
/// none shows it.
#[tokio::test(flavor = "multi_thread")]
async fn a_skip_in_an_unwritten_sweep_shows_at_the_next_write() {
    let (_dir, node, port) = open_with(ScanPort::fast()).await;
    let (id, _rx) = start_span(&node, 100 * MHZ, FAST_RATE_HZ).await;
    let at = || timing(&node, &id).and_then(|t| t["at_ns"].as_u64());
    until("a timing", Duration::from_secs(10), || at().is_some()).await;
    let first = at().unwrap();
    let quiet = Duration::from_millis(450);
    let mut tries = Vec::new();
    for _ in 0..5 {
        let last = at().unwrap();
        until("the next write", Duration::from_secs(10), || {
            at() > Some(last)
        })
        .await;
        let written = at().unwrap();
        let from = port.tunes().len().saturating_sub(1);
        port.hold(true);
        until("a step given up on", Duration::from_secs(10), || {
            port.tuned_at()[from..]
                .windows(2)
                .any(|w| w[1].duration_since(w[0]) >= quiet)
        })
        .await;
        port.hold(false);
        // The first write after the hold that counts a skip; at most one a
        // second, so a poll every 20 ms sees it.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut report = None;
        while report.is_none() && tokio::time::Instant::now() < deadline {
            report = timing(&node, &id).filter(|t| {
                t["at_ns"].as_u64() > Some(written) && t["skipped"].as_u64().unwrap_or(0) >= 1
            });
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let carried = report.as_ref().is_some_and(|t| {
            t["sweep_ms"]
                .as_f64()
                .is_some_and(|ms| ms < quiet.as_secs_f64() * 1e3)
        });
        if carried {
            let t = report.unwrap();
            assert!(t["at_ns"].as_u64().unwrap() > first, "{t}");
            node.job_stop(&id).await.unwrap();
            return;
        }
        tries.push(report);
    }
    panic!("no skip was carried to a later write: {tries:?}");
}

/// `job_start`'s `dwell_ms` and `average` are written into the scan's `fft` node
/// and shown in `job_context.scan`; without them the scan takes the node's dwell
/// and 1, and its rows pass as the stage made them, with `sweeps: 1`.
#[tokio::test(flavor = "multi_thread")]
async fn job_start_takes_dwell_ms_and_average_and_the_record_shows_them() {
    let (_dir, node, _port) = open().await;
    let (id, _rx) = start_scan_look(&node, (20 * MHZ, 40 * MHZ), Some(40), Some(4)).await;
    let fft = fft_node(&graph_of(&node, &id).await).params.clone();
    assert_eq!(
        (fft["dwell_ms"].as_u64(), fft["average"].as_u64()),
        (Some(40), Some(4)),
        "{fft}"
    );
    let ctx = node.job_context(&id).unwrap();
    // 156 frames of 4096 at 16 MHz: 39.94 ms.
    assert_eq!(ctx["scan"]["dwell_ms"], 40, "{ctx}");
    assert!(ctx["scan"].get("dwell_ms_asked").is_none(), "{ctx}");
    assert_eq!(ctx["scan"]["average"], 4, "{ctx}");
    // Within the averaging budget: the average asked is the one in force.
    assert!(ctx["scan"].get("average_asked").is_none(), "{ctx}");
    node.job_stop(&id).await.unwrap();

    let (id, mut rx) = start_scan_look(&node, (20 * MHZ, 40 * MHZ), None, None).await;
    let fft = fft_node(&graph_of(&node, &id).await).params.clone();
    assert!(
        fft.get("dwell_ms").is_none() && fft.get("average").is_none(),
        "{fft}"
    );
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["dwell_ms"], 20, "{ctx}");
    assert_eq!(ctx["scan"]["average"], 1, "{ctx}");
    for f in rows(&mut rx, 6, Duration::from_secs(10)).await {
        let ev = ev(&f);
        assert_eq!(ev["sweeps"], 1, "{ev}");
        assert_eq!(ev["floor_db"], FAKE_FLOOR_DB, "{ev}");
        assert_eq!(ev["samples"], 78 * 4096, "{ev}");
    }
    node.job_stop(&id).await.unwrap();
}

/// With `average: 4` each row is the mean of its step's last four sweeps of its
/// grid: a centre's rows count `sweeps` 1, 2, 3, 4, 4, the even and odd sweeps
/// apart (9 MHz is a step of both grids), requantised from the mean.
#[tokio::test(flavor = "multi_thread")]
async fn averaged_rows_count_their_sweeps_per_grid() {
    let (_dir, node, _port) = open().await;
    let (id, mut rx) = start_scan_look(&node, (20 * MHZ, 40 * MHZ), None, Some(4)).await;
    // Even sweeps step 9, 24, 40 MHz, odd ones 9, 20, 36 MHz.
    let mut by_centre: std::collections::BTreeMap<u64, Vec<u64>> = Default::default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while by_centre.get(&(9 * MHZ)).is_none_or(|v| v.len() < 10)
        || [20, 24, 36, 40]
            .iter()
            .any(|m| by_centre.get(&(m * MHZ)).is_none_or(|v| v.len() < 5))
    {
        let f = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(f)) => f,
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => panic!("{n} rows lost"),
            other => panic!("rows so far {by_centre:?}: {other:?}"),
        };
        let ev = ev(&f);
        // The fake's rows are flat at -90 dB; their mean is requantised: the floor is
        // 5 dB under it and every bin 10 steps up.
        assert_eq!(ev["floor_db"], -95.0, "{ev}");
        assert_eq!(ev["q"], serde_json::json!(vec![10; FAKE_BINS]), "{ev}");
        let c = ev["center_hz"].as_u64().unwrap();
        by_centre
            .entry(c)
            .or_default()
            .push(ev["sweeps"].as_u64().unwrap());
    }
    for m in [20, 24, 36, 40] {
        assert_eq!(
            by_centre[&(m * MHZ)][..5],
            [1, 2, 3, 4, 4],
            "{m} MHz: {by_centre:?}"
        );
    }
    // One even and one odd step a sweep pair: each grid counts its own.
    assert_eq!(
        by_centre[&(9 * MHZ)][..10],
        [1, 1, 2, 2, 3, 3, 4, 4, 4, 4],
        "{by_centre:?}"
    );
    node.job_stop(&id).await.unwrap();
}

/// The next row's event on `rx`, failing at `deadline`.
async fn next_row(rx: &mut broadcast::Receiver<Frame>, deadline: tokio::time::Instant) -> Value {
    match tokio::time::timeout_at(deadline, rx.recv()).await {
        Ok(Ok(f)) => ev(&f).clone(),
        other => panic!("{other:?}"),
    }
}

/// A new layout (here the usable fraction measured after the scan started: the
/// same width, a new stride) starts the averages afresh, even at a centre both
/// layouts step.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_layout_starts_the_averages_afresh() {
    let (_dir, node, port) = open().await;
    let (id, mut rx) = start_scan_look(&node, (20 * MHZ, 40 * MHZ), None, Some(4)).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let ev = next_row(&mut rx, deadline).await;
        if ev["center_hz"] == 9 * MHZ && ev["sweeps"] == 4 {
            break;
        }
    }
    // 16 MHz captures 14.4 MHz apart: 9, 21.6 and 36 MHz, the edges masked at 0.05.
    port.replace_caps(|c| c.usable_fraction = Claim::Asserted(0.9));
    let first = loop {
        let ev = next_row(&mut rx, deadline).await;
        if ev["edge_mask"] == 0.05 && ev["center_hz"] == 9 * MHZ {
            break ev;
        }
    };
    assert_eq!(first["sweeps"], 1, "{first}");
    node.job_stop(&id).await.unwrap();
}

/// `graph_put` on the stored `fft` node changes a running scan's look: the scan
/// restarts with the new dwell and average.
#[tokio::test(flavor = "multi_thread")]
async fn graph_put_changes_dwell_and_average_on_a_running_scan() {
    let (_dir, node, _port) = open().await;
    let (id, _rx) = start_scan(&node).await;
    let mut g = graph_of(&node, &id).await;
    let fft = g.nodes.iter_mut().find(|n| n.kind == "fft").unwrap();
    fft.params["dwell_ms"] = serde_json::json!(40);
    fft.params["average"] = serde_json::json!(2);
    assert!(matches!(
        node.apply(Op::GraphPut {
            job: id.clone(),
            graph: g
        })
        .await,
        Ok(OpResult::Ok)
    ));
    assert_eq!(
        node.job_list().iter().find(|r| r.id == id).unwrap().state,
        JobState::Running
    );
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(
        (
            ctx["scan"]["dwell_ms"].as_u64(),
            ctx["scan"]["average"].as_u64()
        ),
        (Some(40), Some(2)),
        "{ctx}"
    );
    // The restarted scan's rows (a fresh event stream) take 40 ms and average two sweeps.
    let mut rx = event_rx(&node, &id);
    let got = rows(&mut rx, 9, Duration::from_secs(10)).await;
    for f in &got {
        assert_eq!(ev(f)["samples"], 156 * 4096, "{}", ev(f));
    }
    assert!(
        got.iter().any(|f| ev(f)["sweeps"] == 2),
        "no row averaged two sweeps"
    );
    assert!(got.iter().all(|f| {
        ev(f)["sweeps"]
            .as_u64()
            .is_some_and(|n| (1..=2).contains(&n))
    }));
    node.job_stop(&id).await.unwrap();
}

/// A `dwell_ms` under four FFT frames is raised to them; the context says the
/// dwell in force and the one asked for.
#[tokio::test(flavor = "multi_thread")]
async fn a_dwell_under_four_frames_is_raised_and_the_asked_one_reported() {
    let (_dir, node, _port) = open_with(ScanPort::fast()).await;
    // On a 256 kHz step a 1 ms dwell is under four frames of 1024: 16 ms (4096 samples).
    let (id, mut rx) = start_scan_look(&node, (100 * MHZ, FAST_RATE_HZ), Some(1), None).await;
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["dwell_ms"], 16, "{ctx}");
    assert_eq!(ctx["scan"]["dwell_ms_asked"], 1, "{ctx}");
    for f in rows(&mut rx, 3, Duration::from_secs(10)).await {
        assert_eq!(ev(&f)["samples"], 4096, "{}", ev(&f));
    }
    node.job_stop(&id).await.unwrap();
}

/// A dwell longer than the source's ring holds could never be read from it:
/// it is capped to the ring (here 1 s), in whole rows, and the asked one reported.
#[tokio::test(flavor = "multi_thread")]
async fn a_dwell_longer_than_the_ring_is_capped_to_it() {
    let (_dir, node, _port) = open_with(ScanPort::fast()).await;
    let (id, mut rx) = start_scan_look(&node, (100 * MHZ, FAST_RATE_HZ), Some(5000), None).await;
    let ctx = node.job_context(&id).unwrap();
    let dwell = ctx["scan"]["dwell_ms"].as_u64().unwrap();
    assert!((900..=1000).contains(&dwell), "{ctx}");
    assert_eq!(ctx["scan"]["dwell_ms_asked"], 5000, "{ctx}");
    // A whole step's rows arrive: 20 rows a second within the step.
    let got = rows(&mut rx, 25, Duration::from_secs(20)).await;
    let samples: u64 = got[..20]
        .iter()
        .map(|f| ev(f)["samples"].as_u64().unwrap())
        .sum();
    assert!(samples <= FAST_RATE_HZ, "{samples} samples in a 1 s ring");
    node.job_stop(&id).await.unwrap();
}

/// A scan stored before the look existed (its `fft` node and its context
/// without `dwell_ms` or `average`) resumes at the node's dwell and 1.
#[tokio::test(flavor = "multi_thread")]
async fn a_scan_stored_before_the_look_resumes_at_the_defaults() {
    use zeta_core::graph::{GraphId, NodeId};
    use zeta_core::job::{JOB_RECORD_FORMAT, JobRecord};
    let dir = tempfile::tempdir().unwrap();
    let id = JobId::new();
    let rec = JobRecord {
        format: JOB_RECORD_FORMAT,
        id: id.clone(),
        kind: "scan".into(),
        graph: Graph {
            id: GraphId::new(),
            nodes: vec![GraphNode {
                id: NodeId::new(),
                kind: "fft".into(),
                params: serde_json::json!({
                    "size": 4096, "window": "hann", "rows_per_s": 20,
                    "notch_hz": 50_000, "edge_mask": 0.0,
                }),
            }],
            edges: vec![],
        },
        source: SourceId("radio0".into()),
        tuning: Tuning {
            center_hz: Hz(20 * MHZ),
            bandwidth_hz: Hz(40 * MHZ),
        },
        state: JobState::Running,
        context: serde_json::json!({"scan": {"steps": 3, "dwell_ms": 20}}),
        created_ns: Timestamp(1),
        input: None,
        capture_rate_hz: None,
        elastic: false,
        operator_graph: false,
        owner: None,
    };
    let jobs = dir.path().join("state").join("jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(
        jobs.join(format!("{}.json", id.0)),
        serde_json::to_vec(&rec).unwrap(),
    )
    .unwrap();
    let node = Node::open(cfg_dwell(dir.path(), 20), registry(), vec![ScanPort::new()])
        .await
        .unwrap();
    assert_eq!(
        node.job_list().iter().find(|r| r.id == id).unwrap().state,
        JobState::Running
    );
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["dwell_ms"], 20, "{ctx}");
    assert_eq!(ctx["scan"]["average"], 1, "{ctx}");
    let mut rx = event_rx(&node, &id);
    for f in rows(&mut rx, 3, Duration::from_secs(10)).await {
        assert_eq!(ev(&f)["sweeps"], 1, "{}", ev(&f));
        assert_eq!(ev(&f)["samples"], 78 * 4096, "{}", ev(&f));
    }
    node.job_stop(&id).await.unwrap();
}

/// The averages may hold an eighth of the node's ring budget: an `average` that
/// would hold more is lowered to what fits, the context shows both, and no row
/// counts more sweeps than the average in force.
#[tokio::test(flavor = "multi_thread")]
async fn an_average_over_its_budget_is_lowered_to_fit() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = cfg_dwell(dir.path(), 20);
    // 2500 bytes for the averages: one 256 kHz step's row is 1024 bins, so two sweeps fit.
    cfg.ring_budget_bytes = 20_000;
    let node = Node::open(cfg, registry(), vec![ScanPort::fast()])
        .await
        .unwrap();
    let (id, mut rx) = start_scan_look(&node, (100 * MHZ, FAST_RATE_HZ), None, Some(64)).await;
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["steps"], 1, "{ctx}");
    assert_eq!(ctx["scan"]["average"], 2, "{ctx}");
    assert_eq!(ctx["scan"]["average_asked"], 64, "{ctx}");
    let sweeps: Vec<u64> = rows(&mut rx, 6, Duration::from_secs(10))
        .await
        .iter()
        .map(|f| ev(f)["sweeps"].as_u64().unwrap())
        .collect();
    assert_eq!(sweeps, [1, 2, 2, 2, 2, 2]);
    node.job_stop(&id).await.unwrap();
}

/// `op` (a `job_start`) asking for `bits`.
fn with_bits(mut op: Op, b: Option<u8>) -> Op {
    if let Op::JobStart { bits, .. } = &mut op {
        *bits = b;
    }
    op
}

/// Start a scan of `bandwidth_hz` about `center_hz` at `bits`; its id and a
/// receiver on its event stream.
async fn start_bits(
    node: &Node,
    (center_hz, bandwidth_hz): (u64, u64),
    bits: Option<u8>,
) -> (JobId, broadcast::Receiver<Frame>) {
    let op = with_bits(start(JobKind::Scan, center_hz, bandwidth_hz), bits);
    let id = match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    };
    let rx = event_rx(node, &id);
    (id, rx)
}

/// The tunes in `calls`.
fn tunes_in(calls: &[Call]) -> Vec<Tuning> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Tune(t) => Some(*t),
            Call::SetDepth(_) => None,
        })
        .collect()
}

/// The source's depth is back at its own (16 bits) before anything else tuned
/// it: the last `set_depth` in `calls` is 16, every call after it is a tune
/// within the 16-bit cap, and there is at least one (the next job's).
fn restored_before_the_next_tune(calls: &[Call]) {
    let at = calls
        .iter()
        .rposition(|c| matches!(c, Call::SetDepth(_)))
        .expect("a set_depth");
    assert_eq!(calls[at], Call::SetDepth(16), "{calls:?}");
    let after = tunes_in(&calls[at + 1..]);
    assert!(!after.is_empty(), "no tune after the restore: {calls:?}");
    assert_eq!(after.len(), calls.len() - at - 1, "{calls:?}");
    assert!(
        after.iter().all(|t| t.bandwidth_hz.0 <= 16 * MHZ),
        "{calls:?}"
    );
}

/// A scan at `bits: 8` holds the source at 8 bits for its run, before its first
/// tune, steps at that depth's 32 MHz, and says so in `job_context.scan` and on
/// every row; its stop puts the source's 16 bits back.
#[tokio::test(flavor = "multi_thread")]
async fn a_scan_at_a_lossy_depth_steps_at_that_depths_width_and_says_so() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(None)).await;
    let (id, mut rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), Some(8)).await;
    let fft = fft_node(&graph_of(&node, &id).await).params.clone();
    assert_eq!(fft["bits"], 8, "{fft}");
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["bits"], 8, "{ctx}");
    assert_eq!(ctx["scan"]["rate_hz"], 32 * MHZ, "{ctx}");
    // 40 MHz at 32 MHz a step: two steps, not the three of 16 MHz.
    assert_eq!(ctx["scan"]["steps"], 2, "{ctx}");
    for f in rows(&mut rx, 6, Duration::from_secs(10)).await {
        assert_eq!(ev(&f)["bits"], 8, "{}", ev(&f));
        assert_eq!(ev(&f)["size"], 8192, "{}", ev(&f));
    }
    let calls = port.calls();
    assert_eq!(calls[0], Call::SetDepth(8), "{calls:?}");
    let tunes = tunes_in(&calls);
    assert!(tunes.len() >= 2, "{calls:?}");
    assert!(
        tunes.iter().all(|t| t.bandwidth_hz.0 == 32 * MHZ),
        "{calls:?}"
    );
    node.job_stop(&id).await.unwrap();
    assert_eq!(port.calls().last(), Some(&Call::SetDepth(16)));
    assert_eq!(port.depth().map(|d| d.bits), Some(16));
}

/// A scan that asks for no depth leaves the source at its own and reports it;
/// on a source with one depth there is none to report.
#[tokio::test(flavor = "multi_thread")]
async fn a_scan_without_bits_reports_the_sources_own_depth() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(None)).await;
    let (id, mut rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), None).await;
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["bits"], 16, "{ctx}");
    assert_eq!(ctx["scan"]["rate_hz"], 16 * MHZ, "{ctx}");
    for f in rows(&mut rx, 4, Duration::from_secs(10)).await {
        assert_eq!(ev(&f)["bits"], 16, "{}", ev(&f));
    }
    node.job_stop(&id).await.unwrap();
    let calls = port.calls();
    assert!(
        !calls.iter().any(|c| matches!(c, Call::SetDepth(_))),
        "{calls:?}"
    );

    let (_dir, node, _port) = open().await;
    let (id, mut rx) = start_scan(&node).await;
    let ctx = node.job_context(&id).unwrap();
    assert!(ctx["scan"].get("bits").is_none(), "{ctx}");
    for f in rows(&mut rx, 2, Duration::from_secs(10)).await {
        assert!(ev(&f).get("bits").is_none(), "{}", ev(&f));
    }
    node.job_stop(&id).await.unwrap();
}

/// A re-measure lowers the cap of the depth in force under the run's record
/// (which keeps its 16 MHz): a scan without `bits` plans from the live cap, so
/// the sweep after the drop steps at the new 10 MHz and no tune is refused, and
/// a scan launched after it steps at 10 MHz from its first tune.
#[tokio::test(flavor = "multi_thread")]
async fn a_default_scan_follows_a_depth_cap_that_drops_under_the_record() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(None)).await;
    // 40 MHz at 16 MHz is three steps: the cap drops inside the first sweep's last tune.
    port.drop_cap_after(3, 10 * MHZ);
    let (id, _rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), None).await;
    until(
        "eight tunes at the new width",
        Duration::from_secs(10),
        || {
            port.tunes()
                .iter()
                .filter(|t| t.bandwidth_hz.0 == 10 * MHZ)
                .count()
                >= 8
        },
    )
    .await;
    assert_eq!(
        node.job_list().iter().find(|r| r.id == id).unwrap().state,
        JobState::Running
    );
    let tunes = port.tunes();
    assert!(
        tunes[..3].iter().all(|t| t.bandwidth_hz.0 == 16 * MHZ),
        "{tunes:?}"
    );
    assert!(
        tunes[3..].iter().all(|t| t.bandwidth_hz.0 == 10 * MHZ),
        "{tunes:?}"
    );
    until("the context's new width", Duration::from_secs(3), || {
        node.job_context(&id).unwrap()["scan"]["rate_hz"] == 10 * MHZ
    })
    .await;
    assert_eq!(
        port.capabilities().max_bandwidth_hz.value().0,
        16 * MHZ,
        "the record keeps its figure"
    );
    node.job_stop(&id).await.unwrap();

    let before = port.tunes().len();
    let (id, _rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), None).await;
    assert_eq!(
        node.job_context(&id).unwrap()["scan"]["rate_hz"],
        10 * MHZ,
        "planned at launch from the live cap"
    );
    tunes_at_least(&port, before + 4, Duration::from_secs(10)).await;
    assert_eq!(
        node.job_list().iter().find(|r| r.id == id).unwrap().state,
        JobState::Running
    );
    let tunes = port.tunes();
    assert!(
        tunes[before..].iter().all(|t| t.bandwidth_hz.0 == 10 * MHZ),
        "{tunes:?}"
    );
    node.job_stop(&id).await.unwrap();
}

/// A lossy scan is granted only as its source's sole holder: on a held source
/// it is a conflict naming the holders, and the depth is never touched.
#[tokio::test(flavor = "multi_thread")]
async fn a_lossy_scan_on_a_held_source_is_a_conflict_naming_the_holders() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(None)).await;
    let held = match node.apply(start(JobKind::Spectrum, 100 * MHZ, MHZ)).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    };
    let op = with_bits(start(JobKind::Scan, 20 * MHZ, 40 * MHZ), Some(8));
    match node.apply(op).await {
        Err(OpError::Conflict { holders, .. }) => assert_eq!(holders, vec![held.clone()]),
        other => panic!("expected a conflict naming the holder, got {other:?}"),
    }
    let calls = port.calls();
    assert!(
        !calls.iter().any(|c| matches!(c, Call::SetDepth(_))),
        "{calls:?}"
    );
    assert_eq!(port.depth().map(|d| d.bits), Some(16));
    assert_eq!(node.job_list().len(), 1);
    node.job_stop(&held).await.unwrap();
}

/// While a lossy scan runs, a job started on its source is the usual conflict
/// naming the scan.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_started_under_a_lossy_scan_is_a_conflict_naming_it() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(None)).await;
    let (id, _rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), Some(8)).await;
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    for op in [
        start(JobKind::Spectrum, 17 * MHZ, MHZ),
        start(JobKind::FmDemod, 24 * MHZ, 12_500),
        with_bits(start(JobKind::Scan, 100 * MHZ, 40 * MHZ), Some(8)),
    ] {
        match node.apply(op).await {
            Err(OpError::Conflict { holders, .. }) => assert_eq!(holders, vec![id.clone()]),
            other => panic!("expected a conflict naming the scan, got {other:?}"),
        }
    }
    assert_eq!(port.depth().map(|d| d.bits), Some(8));
    node.job_stop(&id).await.unwrap();
}

/// A stopped lossy scan puts the source's depth back before its lease goes: a
/// `job_start` sent as soon as `job_stop` returns, and one that stops the scan
/// itself (`replace`), each tune at the source's own depth.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_scan_restores_the_depth_before_the_next_job_is_granted() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(None)).await;
    let (id, _rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), Some(8)).await;
    tunes_at_least(&port, 3, Duration::from_secs(5)).await;
    node.apply(Op::JobStop { job: id }).await.unwrap();
    let next = match node.apply(start(JobKind::Spectrum, 100 * MHZ, MHZ)).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    };
    restored_before_the_next_tune(&port.calls());
    node.job_stop(&next).await.unwrap();

    // The waiting request stops the scan itself.
    let (id, _rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), Some(8)).await;
    let n = port.tunes().len();
    tunes_at_least(&port, n + 3, Duration::from_secs(5)).await;
    let mut op = start(JobKind::Spectrum, 100 * MHZ, MHZ);
    if let Op::JobStart { replace, .. } = &mut op {
        *replace = vec![id.clone()];
    }
    let next = match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    };
    restored_before_the_next_tune(&port.calls());
    let scan = node.job_list().into_iter().find(|r| r.id == id).unwrap();
    assert_eq!(scan.state, JobState::Stopped);
    node.job_stop(&next).await.unwrap();
}

/// A lossy scan that fails (a tune fault) puts the depth back too, before its
/// lease goes: a request retried all the while is granted only after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_scan_also_restores_the_depth() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(Some(2))).await;
    let (id, _rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), Some(8)).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let next = loop {
        match node.apply(start(JobKind::Spectrum, 100 * MHZ, MHZ)).await {
            Ok(OpResult::Job(next)) => break next,
            Err(OpError::Conflict { holders, .. }) => assert_eq!(holders, vec![id.clone()]),
            other => panic!("expected a job or a conflict naming the scan, got {other:?}"),
        }
        assert!(tokio::time::Instant::now() < deadline, "never granted");
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    match node
        .job_list()
        .into_iter()
        .find(|r| r.id == id)
        .unwrap()
        .state
    {
        JobState::Failed { reason } => assert!(reason.contains("tune 2 refused"), "{reason}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    let calls = port.calls();
    assert_eq!(calls[0], Call::SetDepth(8), "{calls:?}");
    assert!(
        matches!(calls[1], Call::Tune(t) if t.bandwidth_hz.0 == 32 * MHZ),
        "{calls:?}"
    );
    assert_eq!(calls[2], Call::SetDepth(16), "{calls:?}");
    restored_before_the_next_tune(&calls);
    node.job_stop(&next).await.unwrap();
}

/// `bits` the source's link does not carry is refused, naming the depths it
/// does, before anything is leased or changed.
#[tokio::test(flavor = "multi_thread")]
async fn bits_the_source_does_not_carry_is_refused_naming_its_depths() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(None)).await;
    let op = with_bits(start(JobKind::Scan, 20 * MHZ, 40 * MHZ), Some(12));
    match node.apply(op).await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "bits 12: source radio0 carries 16, 8")
        }
        other => panic!("expected Other, got {other:?}"),
    }
    assert!(node.job_list().is_empty());
    assert!(port.calls().is_empty(), "{:?}", port.calls());
}

/// `bits` on a source with one sample depth is refused.
#[tokio::test(flavor = "multi_thread")]
async fn bits_on_a_source_with_one_depth_is_refused() {
    let (_dir, node, port) = open().await;
    let op = with_bits(start(JobKind::Scan, 20 * MHZ, 40 * MHZ), Some(8));
    match node.apply(op).await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "bits 8: source radio0 has one sample depth")
        }
        other => panic!("expected Other, got {other:?}"),
    }
    assert!(node.job_list().is_empty());
    assert!(port.tunes().is_empty());
}

/// `graph_put` on the stored `fft` node's `bits` changes a running scan's depth:
/// the scan restarts at it; `null` takes it back to the source's own; a depth the
/// source does not carry is refused and the scan runs on as it was.
#[tokio::test(flavor = "multi_thread")]
async fn graph_put_changes_bits_on_a_running_scan() {
    let (_dir, node, port) = open_with(ScanPort::with_depths(None)).await;
    let (id, _rx) = start_bits(&node, (20 * MHZ, 40 * MHZ), None).await;
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    let put = |bits: Value| {
        let (node, id) = (&node, id.clone());
        async move {
            let mut g = graph_of(node, &id).await;
            let fft = g.nodes.iter_mut().find(|n| n.kind == "fft").unwrap();
            fft.params["bits"] = bits;
            node.apply(Op::GraphPut { job: id, graph: g }).await
        }
    };
    assert!(matches!(put(8.into()).await, Ok(OpResult::Ok)));
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(
        (
            ctx["scan"]["bits"].as_u64(),
            ctx["scan"]["rate_hz"].as_u64()
        ),
        (Some(8), Some(32 * MHZ)),
        "{ctx}"
    );
    let mut rx = event_rx(&node, &id);
    for f in rows(&mut rx, 3, Duration::from_secs(10)).await {
        assert_eq!(ev(&f)["bits"], 8, "{}", ev(&f));
    }
    assert!(port.calls().contains(&Call::SetDepth(8)));

    match put(12.into()).await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "bits 12: source radio0 carries 16, 8")
        }
        other => panic!("expected Other, got {other:?}"),
    }
    let rec = node.job_list().into_iter().find(|r| r.id == id).unwrap();
    assert_eq!(rec.state, JobState::Running);
    assert_eq!(node.job_context(&id).unwrap()["scan"]["bits"], 8);

    assert!(matches!(put(Value::Null).await, Ok(OpResult::Ok)));
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(
        (
            ctx["scan"]["bits"].as_u64(),
            ctx["scan"]["rate_hz"].as_u64()
        ),
        (Some(16), Some(16 * MHZ)),
        "{ctx}"
    );
    let mut rx = event_rx(&node, &id);
    for f in rows(&mut rx, 3, Duration::from_secs(10)).await {
        assert_eq!(ev(&f)["bits"], 16, "{}", ev(&f));
    }
    node.job_stop(&id).await.unwrap();
    restored_before_the_next_tune(&port.calls());
}

// The scan on a source that sweeps for itself (plan 36.2): the source's rows in the
// scan row shape, under a sweep claim and no lease.

/// The span the self-sweep tests scan: 80..120 MHz, inside a
/// [`ScanPort::sweeping`] port's sweep; its steps centre on 85, 95, 105, 115 MHz.
const SWEEP_SPAN: (u64, u64) = (100 * MHZ, 40 * MHZ);

/// The centre of a [`ScanPort::sweeping`] port's step `step` over [`SWEEP_SPAN`].
fn sweep_centre(step: u64) -> u64 {
    85 * MHZ + step * 10 * MHZ
}

fn radio0() -> SourceId {
    SourceId("radio0".into())
}

/// Job `id`'s `job_context.scan`.
fn scan_ctx(node: &Node, id: &JobId) -> Value {
    node.job_context(id).expect("a context")["scan"].clone()
}

fn job_state(node: &Node, id: &JobId) -> Option<JobState> {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .map(|r| r.state)
}

/// `op` (a `job_start`) made elastic.
fn elastic(mut op: Op) -> Op {
    if let Op::JobStart { elastic, .. } = &mut op {
        *elastic = true;
    }
    op
}

async fn start_job(node: &Node, op: Op) -> JobId {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    }
}

async fn radio0_info(node: &Node) -> zeta_core::ops::SourceInfo {
    match node.apply(Op::SourceList {}).await {
        Ok(OpResult::Sources(v)) => v.into_iter().find(|i| i.id == radio0()).unwrap(),
        other => panic!("expected sources, got {other:?}"),
    }
}

/// A row's step, from its channel `scan/<step>`.
fn step_of(f: &Frame) -> u64 {
    channel(f)
        .strip_prefix("scan/")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("a step channel: {}", channel(f)))
}

/// The source is asked to sweep and its rows come out as a scan's: on `scan/<step>`
/// channels in order, quantised over the unmasked bins, with `sweeps` and
/// `calibrated`; the radio is never tuned and the record says `method: source`.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_sweeping_port_is_asked_and_its_rows_come_out_in_the_scan_row_shape() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let (id, mut rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    let got = rows(&mut rx, 12, Duration::from_secs(10)).await;
    let first = got
        .iter()
        .position(|f| step_of(f) == 0)
        .expect("a row of step 0");
    let sweep = &got[first..first + 4];
    for (step, f) in sweep.iter().enumerate() {
        let step = step as u64;
        assert_eq!(channel(f), format!("scan/{step}"));
        assert_eq!(f.provenance.job.as_ref(), Some(&id));
        let ev = ev(f);
        assert_eq!(ev["size"], 1024, "{ev}");
        assert_eq!(ev["q"].as_array().map(Vec::len), Some(1024), "{ev}");
        assert_eq!(ev["step_db"], 0.5, "{ev}");
        assert_eq!(ev["masked"], serde_json::json!([[0, 2]]), "{ev}");
        assert_eq!(ev["sweeps"], 1, "{ev}");
        assert_eq!(ev["calibrated"], true, "{ev}");
        assert_eq!(ev["center_hz"], sweep_centre(step), "{ev}");
        assert_eq!(ev["bin_hz"], 10.0 * MHZ as f64 / 1024.0, "{ev}");
        // The node's 20 ms dwell, passed to the source and kept: 50 rows a second.
        assert_eq!(ev["rows_per_s"], 50, "{ev}");
        // Each step's bins sit at one level: the floor 5 dB under it, every bin 10 up.
        assert_eq!(ev["floor_db"], -105.0 + 10.0 * step as f64, "{ev}");
        assert_eq!(ev["q"][500], 10, "{ev}");
    }
    assert!(port.tunes().is_empty(), "{:?}", port.tunes());
    assert_eq!(port.requests().len(), 1);
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "source", "{ctx}");
    assert_eq!(ctx["steps"], 4, "{ctx}");
    assert_eq!(ctx["step_hz"], 10 * MHZ, "{ctx}");
    assert_eq!(ctx["bin_hz"], 10.0 * MHZ as f64 / 1024.0, "{ctx}");
    assert_eq!(ctx["dwell_ms"], 20, "{ctx}");
    assert_eq!(ctx["average"], 1, "{ctx}");
    assert!(ctx["alternate"].is_null(), "{ctx}");
    assert_eq!(
        ctx["mask"],
        serde_json::json!({"notch_hz": 0, "edge_mask": 0.0}),
        "{ctx}"
    );
    assert_eq!(ctx["calibrated"], true, "{ctx}");
    assert!(ctx["waiting"].is_null(), "{ctx}");
    for k in ["rate_hz", "bits", "dwell_ms_asked", "average_asked"] {
        assert!(ctx.get(k).is_none(), "{k}: {ctx}");
    }
    node.job_stop(&id).await.unwrap();
}

/// A self-sweep takes no lease: it starts beside a rigid job holding the source, and
/// an elastic job on the source is not stopped for it; it is listed in `sweeps`.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_sweep_takes_no_lease_and_runs_beside_a_demodulator() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let rigid = start_job(&node, start(JobKind::Spectrum, 100 * MHZ, MHZ)).await;
    let follower = start_job(&node, elastic(start(JobKind::Spectrum, 100 * MHZ, MHZ))).await;
    let (scan, _rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    let info = radio0_info(&node).await;
    assert_eq!(info.holders, vec![rigid.clone()]);
    assert_eq!(info.sweeps, vec![scan.clone()]);
    assert_eq!(info.elastic, vec![follower.clone()]);
    // Whatever the stop of the elastic jobs would have taken, it is not taken.
    tokio::time::sleep(Duration::from_millis(200)).await;
    for id in [&rigid, &follower, &scan] {
        assert_eq!(
            job_state(&node, id),
            Some(JobState::Running),
            "job {}",
            id.0
        );
    }
    let band = Tuning {
        center_hz: Hz(100 * MHZ),
        bandwidth_hz: Hz(MHZ),
    };
    assert!(
        port.tunes().iter().all(|t| *t == band),
        "{:?}",
        port.tunes()
    );
    node.job_stop(&scan).await.unwrap();
}

/// A stopped self-sweep drops its rows (ending the source's sweep) and its claim.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_self_sweep_drops_the_rows_and_the_claim() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let (id, mut rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    assert_eq!(port.live(), 1);
    assert_eq!(node.lease_sweepers(&radio0()), vec![id.clone()]);
    node.job_stop(&id).await.unwrap();
    until("the source's sweep ends", Duration::from_secs(5), || {
        port.live() == 0
    })
    .await;
    assert!(node.lease_sweepers(&radio0()).is_empty());
    assert!(radio0_info(&node).await.sweeps.is_empty());
    // Nothing holds the source any more: it is idled.
    assert!(port.idles() >= 1);
}

/// A self-sweep stopped before its source sent a row leaves no claim, and the
/// source, held by nothing, is idled.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_sweep_stopped_while_its_sweep_is_still_starting_leaves_no_claim() {
    let script = SweepScript {
        first_row_ms: Some(60_000),
        ..Default::default()
    };
    let (_dir, node, port) = open_with(ScanPort::sweeping(script)).await;
    let (id, _rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    assert_eq!(node.lease_sweepers(&radio0()), vec![id.clone()]);
    assert_eq!(port.idles(), 0);
    node.job_stop(&id).await.unwrap();
    assert!(node.lease_sweepers(&radio0()).is_empty());
    assert_eq!(port.idles(), 1);
    until("the source's sweep ends", Duration::from_secs(5), || {
        port.live() == 0
    })
    .await;
}

/// `dwell_ms` is the source's to apply and `average` the node's: the request carries
/// the dwell and the target bin (6 kHz over the source's 1 kHz floor), and with
/// `average: 4` a centre's rows count `sweeps` 1, 2, 3, 4, 4.
#[tokio::test(flavor = "multi_thread")]
async fn dwell_ms_goes_to_the_source_and_average_is_the_nodes() {
    let script = SweepScript {
        // Time to subscribe before the first row.
        first_row_ms: Some(300),
        ..Default::default()
    };
    let (_dir, node, port) = open_with(ScanPort::sweeping(script)).await;
    let (id, mut rx) = start_scan_look(&node, SWEEP_SPAN, Some(200), Some(4)).await;
    let req = port.requests();
    assert_eq!(req.len(), 1);
    assert_eq!(req[0].dwell_ms, 200);
    assert_eq!(req[0].bin_hz, Hz(6000));
    assert_eq!(
        req[0].span,
        Tuning {
            center_hz: Hz(SWEEP_SPAN.0),
            bandwidth_hz: Hz(SWEEP_SPAN.1),
        }
    );
    let mut sweeps = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while sweeps.len() < 5 {
        let ev = next_row(&mut rx, deadline).await;
        if ev["center_hz"] == sweep_centre(1) {
            assert_eq!(ev["floor_db"], -95.0, "{ev}");
            assert_eq!(ev["q"][500], 10, "{ev}");
            sweeps.push(ev["sweeps"].as_u64().unwrap());
        }
    }
    assert_eq!(sweeps, [1, 2, 3, 4, 4]);
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["average"], 4, "{ctx}");
    assert_eq!(ctx["dwell_ms"], 200, "{ctx}");
    assert!(ctx.get("average_asked").is_none(), "{ctx}");
    assert!(ctx.get("dwell_ms_asked").is_none(), "{ctx}");
    node.job_stop(&id).await.unwrap();
}

/// A self-sweep's record carries the same `timing` as a stepped one's, with no tune
/// time; a step its source sent no row for is counted skipped.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_sweeps_record_fills_the_same_timing() {
    let (_dir, node, _port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let (id, _rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    until("a sweep's timing", Duration::from_secs(10), || {
        timing(&node, &id).is_some_and(|t| t["sweep_ms"].as_f64().unwrap_or(0.0) > 0.0)
    })
    .await;
    let t = timing(&node, &id).unwrap();
    for k in [
        "sweep_ms",
        "step_ms",
        "tune_ms",
        "slow_tunes",
        "skipped",
        "fft_wait_ms",
        "hz_per_s",
        "at_ns",
    ] {
        assert!(!t[k].is_null(), "no {k}: {t}");
    }
    assert_eq!(t["tune_ms"]["median"].as_f64(), Some(0.0), "{t}");
    assert_eq!(t["tune_ms"]["max"].as_f64(), Some(0.0), "{t}");
    assert_eq!(t["skipped"], 0, "{t}");
    assert!(t["hz_per_s"].as_f64().unwrap() > 0.0, "{t}");
    node.job_stop(&id).await.unwrap();

    let script = SweepScript {
        skip_step: Some(2),
        ..Default::default()
    };
    let (_dir, node, _port) = open_with(ScanPort::sweeping(script)).await;
    let (id, _rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    until("a skipped step", Duration::from_secs(10), || {
        timing(&node, &id).is_some_and(|t| t["skipped"].as_u64().unwrap_or(0) >= 1)
    })
    .await;
    node.job_stop(&id).await.unwrap();
}

/// A sweep the source lost and retries keeps the scan up: the record says `waiting`
/// with the reason, then `null` once rows come again; nothing is tuned and the
/// method stays `source`.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_sweep_says_waiting_and_resumes() {
    let script = SweepScript {
        wait_after: Some(6),
        ..Default::default()
    };
    let (_dir, node, port) = open_with(ScanPort::sweeping(script)).await;
    let (id, _rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    until("the record says waiting", Duration::from_secs(10), || {
        scan_ctx(&node, &id)["waiting"] == "socket lost"
    })
    .await;
    assert_eq!(job_state(&node, &id), Some(JobState::Running));
    until("the record stops waiting", Duration::from_secs(10), || {
        scan_ctx(&node, &id)["waiting"].is_null()
    })
    .await;
    assert_eq!(job_state(&node, &id), Some(JobState::Running));
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "source", "{ctx}");
    assert_eq!(ctx["steps"], 4, "{ctx}");
    assert!(ctx["timing"].is_object(), "{ctx}");
    assert!(port.tunes().is_empty(), "{:?}", port.tunes());
    node.job_stop(&id).await.unwrap();
}

/// A sweep the source ends for good fails the scan with a reason that is the
/// sweep's, not the source's: no fault is opened and the job on the source's IQ
/// side runs on.
#[tokio::test(flavor = "multi_thread")]
async fn a_sweep_the_source_gives_up_on_fails_the_scan_without_faulting_the_source() {
    let script = SweepScript {
        end_after: Some(6),
        ..Default::default()
    };
    let (_dir, node, _port) = open_with(ScanPort::sweeping(script)).await;
    let rigid = start_job(&node, start(JobKind::Spectrum, 100 * MHZ, MHZ)).await;
    let (id, _rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    until("the scan ends", Duration::from_secs(10), || {
        job_state(&node, &id) != Some(JobState::Running)
    })
    .await;
    match job_state(&node, &id) {
        Some(JobState::Failed { reason }) => {
            assert!(
                reason.starts_with("sweep on source radio0: gone"),
                "{reason}"
            )
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(radio0_info(&node).await.fault.is_none());
    // The supervisor records the end before it releases the claim.
    until("the claim is released", Duration::from_secs(5), || {
        node.lease_sweepers(&radio0()).is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(job_state(&node, &rigid), Some(JobState::Running));
    node.job_stop(&rigid).await.unwrap();
}

/// A source faster than the node loses rows, never queues them without bound: the
/// rows that arrive come in the source's order and the scan runs on.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_reader_costs_rows_not_memory() {
    let script = SweepScript {
        burst: true,
        ..Default::default()
    };
    let (_dir, node, port) = open_with(ScanPort::sweeping(script)).await;
    let (id, mut rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    until("a row is dropped", Duration::from_secs(10), || {
        port.dropped() > 0
    })
    .await;
    let got = rows(&mut rx, 20, Duration::from_secs(10)).await;
    for w in got.windows(2) {
        assert!(
            w[0].provenance.span.end_ns.0 < w[1].provenance.span.end_ns.0,
            "rows out of order: {:?} then {:?}",
            w[0].provenance.span,
            w[1].provenance.span
        );
    }
    assert_eq!(job_state(&node, &id), Some(JobState::Running));
    node.job_stop(&id).await.unwrap();
}

/// A span the source's sweep does not hold is stepped, without asking the source.
#[tokio::test(flavor = "multi_thread")]
async fn a_span_outside_the_sources_sweep_is_stepped() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    // 0..40 MHz: under the sweep's 1 MHz floor.
    let (id, _rx) = start_scan(&node).await;
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    assert!(port.requests().is_empty());
    assert_eq!(scan_ctx(&node, &id)["method"], "stepped");
    assert!(node.lease_sweepers(&radio0()).is_empty());
    node.job_stop(&id).await.unwrap();
}

/// A scan asking for `bits` asks for IQ: it is stepped, without asking the source.
#[tokio::test(flavor = "multi_thread")]
async fn a_scan_with_bits_is_stepped() {
    let script = SweepScript {
        depths: true,
        ..Default::default()
    };
    let (_dir, node, port) = open_with(ScanPort::sweeping(script)).await;
    let (id, _rx) = start_bits(&node, SWEEP_SPAN, Some(8)).await;
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    assert!(port.requests().is_empty());
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "stepped", "{ctx}");
    assert_eq!(ctx["bits"], 8, "{ctx}");
    node.job_stop(&id).await.unwrap();
}

/// A source whose sweep never starts is given up on, so the gate is not held for
/// it: the call is read as a refusal, its claim is dropped and the scan steps.
#[cfg(feature = "test-fixtures")]
#[tokio::test(flavor = "multi_thread")]
async fn a_sweep_start_that_never_returns_is_refused_and_the_scan_steps() {
    let script = SweepScript {
        hang: true,
        ..Default::default()
    };
    let (_dir, node, port) = open_with(ScanPort::sweeping(script)).await;
    let wait = Duration::from_millis(300);
    node.debug_set_sweep_start_timeout(wait);
    let asked = Instant::now();
    let (id, _rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    let took = asked.elapsed();
    assert!(took >= wait, "started after {took:?}");
    // Well under the node's own 5 s: the shortened wait was the one in force.
    assert!(took < Duration::from_secs(4), "started after {took:?}");
    assert_eq!(port.requests().len(), 1);
    assert!(node.lease_sweepers(&radio0()).is_empty());
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    assert_eq!(scan_ctx(&node, &id)["method"], "stepped");
    node.job_stop(&id).await.unwrap();
}

// The scan's method and the fallback (plan 36.2): `method` forces `stepped` or
// `source`; with none a refused sweep steps at a first start and says why, and a
// scan that ran as `source` waits for its sweep instead at a restart.

/// The refusal the method tests' sources give.
const NO_WATERFALL: &str = "no waterfall channel free";

/// A port that refuses every sweep with [`NO_WATERFALL`].
fn refusing() -> Arc<ScanPort> {
    ScanPort::sweeping(SweepScript {
        refuse: Some(NO_WATERFALL.into()),
        ..Default::default()
    })
}

/// A scan's `job_start` over [`SWEEP_SPAN`] with `method` and `bin_hz`.
fn sweep_start(method: Option<ScanMethod>, bin_hz: Option<u64>) -> Op {
    with_method(
        start(JobKind::Scan, SWEEP_SPAN.0, SWEEP_SPAN.1),
        method,
        bin_hz,
    )
}

/// `graph_put` job `id`'s graph with its `fft` node's params changed by `f`.
async fn put_fft(
    node: &Node,
    id: &JobId,
    f: impl FnOnce(&mut serde_json::Map<String, Value>),
) -> Result<OpResult, OpError> {
    let mut g = graph_of(node, id).await;
    let fft = g.nodes.iter_mut().find(|n| n.kind == "fft").unwrap();
    f(fft.params.as_object_mut().unwrap());
    node.apply(Op::GraphPut {
        job: id.clone(),
        graph: g,
    })
    .await
}

/// No scan job is listed.
fn no_scan_listed(node: &Node) -> bool {
    node.job_list().iter().all(|r| r.kind != "scan")
}

/// A sweep the source refuses at a first start steps the span instead, and the
/// record says the method and why the source refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sweep_falls_back_to_stepped_and_says_why() {
    let (_dir, node, port) = open_with(refusing()).await;
    let (id, _rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    assert_eq!(port.requests().len(), 1);
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "stepped", "{ctx}");
    assert_eq!(ctx["fallback"], NO_WATERFALL, "{ctx}");
    assert!(node.lease_sweepers(&radio0()).is_empty());
    assert_eq!(radio0_info(&node).await.holders, vec![id.clone()]);
    node.job_stop(&id).await.unwrap();
}

/// With the source's capture held, the fallback is the usual conflict naming the
/// holders, the source's reason among its options; nothing is stopped and no scan
/// is left.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sweep_under_a_held_channel_is_the_conflict_with_the_reason() {
    let (_dir, node, port) = open_with(refusing()).await;
    let rigid = start_job(&node, start(JobKind::Spectrum, 100 * MHZ, MHZ)).await;
    match node
        .apply(start(JobKind::Scan, SWEEP_SPAN.0, SWEEP_SPAN.1))
        .await
    {
        Err(OpError::Conflict {
            holders, options, ..
        }) => {
            assert_eq!(holders, vec![rigid.clone()]);
            let want = format!("the source refused to sweep for itself: {NO_WATERFALL}");
            assert!(options.contains(&want), "{options:?}");
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(port.requests().len(), 1);
    assert!(no_scan_listed(&node), "{:?}", node.job_list());
    assert!(node.lease_sweepers(&radio0()).is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(job_state(&node, &rigid), Some(JobState::Running));
    assert_eq!(radio0_info(&node).await.holders, vec![rigid.clone()]);
    node.job_stop(&rigid).await.unwrap();
}

/// `method: "stepped"` steps a span the source could sweep, never asking it.
#[tokio::test(flavor = "multi_thread")]
async fn method_stepped_never_asks_the_source() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let id = start_job(&node, sweep_start(Some(ScanMethod::Stepped), None)).await;
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    assert!(port.requests().is_empty());
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "stepped", "{ctx}");
    assert!(ctx.get("fallback").is_none(), "{ctx}");
    assert_eq!(
        fft_node(&graph_of(&node, &id).await).params["method"],
        "stepped"
    );
    node.job_stop(&id).await.unwrap();
}

/// `method: "source"` and a refusal is the start's error, holding the reason: no
/// tune, no claim, no scan.
#[tokio::test(flavor = "multi_thread")]
async fn method_source_fails_instead_of_falling_back() {
    let (_dir, node, port) = open_with(refusing()).await;
    match node
        .apply(sweep_start(Some(ScanMethod::Source), None))
        .await
    {
        Err(OpError::Other { message }) => {
            assert_eq!(message, format!("sweep on source radio0: {NO_WATERFALL}"))
        }
        other => panic!("expected Other, got {other:?}"),
    }
    assert_eq!(port.requests().len(), 1);
    assert!(port.tunes().is_empty(), "{:?}", port.tunes());
    assert!(node.lease_sweepers(&radio0()).is_empty());
    assert!(no_scan_listed(&node));
}

/// `method: "source"` on a span the source's sweep does not hold is refused naming
/// the span, before the source is asked.
#[tokio::test(flavor = "multi_thread")]
async fn method_source_outside_the_sweep_is_refused_naming_the_span() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    // 0..40 MHz: under the sweep's 1 MHz floor.
    let op = with_method(
        start(JobKind::Scan, 20 * MHZ, 40 * MHZ),
        Some(ScanMethod::Source),
        None,
    );
    match node.apply(op).await {
        Err(OpError::Other { message }) => assert_eq!(
            message,
            "source radio0 does not sweep 0-40000000 Hz for itself"
        ),
        other => panic!("expected Other, got {other:?}"),
    }
    assert!(port.requests().is_empty());
    assert!(port.tunes().is_empty());
    assert!(no_scan_listed(&node));
}

/// `bin_hz` is the bin asked of the source; without it the target bin (6 kHz over
/// the source's 1 kHz floor).
#[tokio::test(flavor = "multi_thread")]
async fn bin_hz_goes_to_the_source() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let id = start_job(&node, sweep_start(None, Some(30_000))).await;
    assert_eq!(port.requests()[0].bin_hz, Hz(30_000));
    assert_eq!(
        fft_node(&graph_of(&node, &id).await).params["bin_hz"],
        30_000
    );
    assert_eq!(scan_ctx(&node, &id)["method"], "source");
    node.job_stop(&id).await.unwrap();
    let id = start_job(&node, sweep_start(None, None)).await;
    assert_eq!(port.requests()[1].bin_hz, Hz(6000));
    node.job_stop(&id).await.unwrap();
}

/// `graph_put` of the `fft` node's `method` changes a running self-sweep to stepped:
/// its sweep ends and the radio is stepped.
#[tokio::test(flavor = "multi_thread")]
async fn graph_put_changes_the_method_on_a_running_scan() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let (id, mut rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    let put = put_fft(&node, &id, |p| {
        p.insert("method".into(), "stepped".into());
    });
    assert!(matches!(put.await, Ok(OpResult::Ok)));
    until("the source's sweep ends", Duration::from_secs(5), || {
        port.live() == 0
    })
    .await;
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    assert_eq!(job_state(&node, &id), Some(JobState::Running));
    assert_eq!(scan_ctx(&node, &id)["method"], "stepped");
    assert!(node.lease_sweepers(&radio0()).is_empty());
    assert_eq!(port.requests().len(), 1);
    node.job_stop(&id).await.unwrap();
}

/// A scan that ran as `source`, restarted under its id (here a `graph_put` of its
/// dwell) while the source refuses, neither steps nor fails: it runs, its record
/// says `waiting` with the reason and nothing is tuned; once the source grants the
/// sweep again the rows come and `waiting` is `null`.
#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_self_sweep_refused_waits_instead_of_stepping() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let (id, mut rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    port.set_refuse(Some("a sweep is already running"));
    let put = put_fft(&node, &id, |p| {
        p.insert("dwell_ms".into(), 40.into());
    });
    assert!(matches!(put.await, Ok(OpResult::Ok)));
    until("the record says waiting", Duration::from_secs(5), || {
        scan_ctx(&node, &id)["waiting"] == "a sweep is already running"
    })
    .await;
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "source", "{ctx}");
    assert!(ctx.get("steps").is_none(), "{ctx}");
    assert!(ctx.get("fallback").is_none(), "{ctx}");
    assert_eq!(job_state(&node, &id), Some(JobState::Running));
    assert_eq!(node.lease_sweepers(&radio0()), vec![id.clone()]);
    assert!(radio0_info(&node).await.holders.is_empty());
    // Asked again a second later: still refused, still waiting.
    until("the source is asked again", Duration::from_secs(10), || {
        port.requests().len() >= 3
    })
    .await;
    assert!(port.tunes().is_empty(), "{:?}", port.tunes());
    assert_eq!(job_state(&node, &id), Some(JobState::Running));
    let mut rx = event_rx(&node, &id);
    port.set_refuse(None);
    until("the record stops waiting", Duration::from_secs(20), || {
        scan_ctx(&node, &id)["waiting"].is_null()
    })
    .await;
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "source", "{ctx}");
    assert_eq!(ctx["steps"], 4, "{ctx}");
    assert_eq!(ctx["dwell_ms"], 40, "{ctx}");
    assert!(port.tunes().is_empty(), "{:?}", port.tunes());
    node.job_stop(&id).await.unwrap();
    until("the source's sweep ends", Duration::from_secs(5), || {
        port.live() == 0
    })
    .await;
    assert!(node.lease_sweepers(&radio0()).is_empty());
}

/// A `graph_put` the source cannot run is refused with `job_start`'s text before
/// anything is stopped: `method: "source"` on a span outside the sweep, `bin_hz`
/// with `method: "stepped"`. Each scan runs on with its method.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_put_the_source_cannot_sweep_is_refused_and_the_scan_runs_on() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    // 0..40 MHz: stepped, outside the sweep.
    let (stepped, _rx) = start_scan(&node).await;
    tunes_at_least(&port, 1, Duration::from_secs(5)).await;
    let put = put_fft(&node, &stepped, |p| {
        p.insert("method".into(), "source".into());
    });
    match put.await {
        Err(OpError::Other { message }) => assert_eq!(
            message,
            "source radio0 does not sweep 0-40000000 Hz for itself"
        ),
        other => panic!("expected Other, got {other:?}"),
    }
    assert_eq!(job_state(&node, &stepped), Some(JobState::Running));
    assert_eq!(scan_ctx(&node, &stepped)["method"], "stepped");
    node.job_stop(&stepped).await.unwrap();

    let (source, mut rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    let put = put_fft(&node, &source, |p| {
        p.insert("method".into(), "stepped".into());
        p.insert("bin_hz".into(), 30_000.into());
    });
    match put.await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "bin_hz applies to a source that sweeps for itself")
        }
        other => panic!("expected Other, got {other:?}"),
    }
    assert_eq!(job_state(&node, &source), Some(JobState::Running));
    assert_eq!(scan_ctx(&node, &source)["method"], "source");
    // Not stopped: the same sweep runs on.
    assert_eq!(port.requests().len(), 1);
    assert_eq!(port.live(), 1);
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    node.job_stop(&source).await.unwrap();
}

/// A source slow to start its sweep, under a restart of the scan (`graph_put`): the
/// restart waits for it, one sweep runs under the scan's one claim, and a job
/// started on the source meanwhile waits for the gate and is granted.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_sweep_start_under_a_restart_runs_one_sweep() {
    let script = SweepScript {
        start_ms: Some(300),
        ..Default::default()
    };
    let (_dir, node, port) = open_with(ScanPort::sweeping(script)).await;
    let (id, mut rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    let asked = Instant::now();
    let (put, rigid) = tokio::join!(
        put_fft(&node, &id, |p| {
            p.insert("dwell_ms".into(), 40.into());
        }),
        async {
            // Once the restart has stopped the old run and is starting the new sweep.
            until("the restart asks again", Duration::from_secs(5), || {
                port.requests().len() == 2
            })
            .await;
            node.apply(start(JobKind::Spectrum, 100 * MHZ, MHZ)).await
        }
    );
    assert!(matches!(put, Ok(OpResult::Ok)), "{put:?}");
    assert!(asked.elapsed() >= Duration::from_millis(300));
    let rigid = match rigid {
        Ok(OpResult::Job(j)) => j,
        other => panic!("expected a job, got {other:?}"),
    };
    let mut rx = event_rx(&node, &id);
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    until("one sweep runs", Duration::from_secs(5), || {
        port.live() == 1
    })
    .await;
    assert_eq!(port.requests().len(), 2);
    assert_eq!(node.lease_sweepers(&radio0()), vec![id.clone()]);
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "source", "{ctx}");
    assert_eq!(ctx["dwell_ms"], 40, "{ctx}");
    assert_eq!(job_state(&node, &rigid), Some(JobState::Running));
    node.job_stop(&id).await.unwrap();
    node.job_stop(&rigid).await.unwrap();
}

/// `op` (a `job_start`) with `replace` naming `ids`.
fn replacing(mut op: Op, ids: &[JobId]) -> Op {
    if let Op::JobStart { replace, .. } = &mut op {
        *replace = ids.to_vec();
    }
    op
}

/// `replace` on a scan the source sweeps for itself stops nothing: the sweep takes
/// no lease, so the named holder (and the elastic job beside it) runs on.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_sweep_with_replace_leaves_the_named_running() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let named = start_job(&node, start(JobKind::Spectrum, 100 * MHZ, MHZ)).await;
    let follower = start_job(&node, elastic(start(JobKind::Spectrum, 100 * MHZ, MHZ))).await;
    let op = replacing(
        start(JobKind::Scan, SWEEP_SPAN.0, SWEEP_SPAN.1),
        std::slice::from_ref(&named),
    );
    let id = start_job(&node, op).await;
    assert_eq!(scan_ctx(&node, &id)["method"], "source");
    tokio::time::sleep(Duration::from_millis(200)).await;
    for job in [&named, &follower, &id] {
        assert_eq!(job_state(&node, job), Some(JobState::Running), "{}", job.0);
    }
    assert_eq!(radio0_info(&node).await.holders, vec![named.clone()]);
    let band = Tuning {
        center_hz: Hz(100 * MHZ),
        bandwidth_hz: Hz(MHZ),
    };
    assert!(
        port.tunes().iter().all(|t| *t == band),
        "{:?}",
        port.tunes()
    );
    node.job_stop(&id).await.unwrap();
    assert_eq!(job_state(&node, &named), Some(JobState::Running));
    node.job_stop(&follower).await.unwrap();
    node.job_stop(&named).await.unwrap();
}

/// `replace` on a scan whose source refuses to sweep is honoured for the stepped
/// scan that follows: the named holder is stopped under the same hold and the scan
/// steps, saying why.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sweep_with_replace_stops_the_named_and_steps() {
    let (_dir, node, port) = open_with(refusing()).await;
    let named = start_job(&node, start(JobKind::Spectrum, 100 * MHZ, MHZ)).await;
    let op = replacing(
        start(JobKind::Scan, SWEEP_SPAN.0, SWEEP_SPAN.1),
        std::slice::from_ref(&named),
    );
    let id = start_job(&node, op).await;
    assert_eq!(port.requests().len(), 1);
    assert_eq!(job_state(&node, &named), Some(JobState::Stopped));
    let ctx = scan_ctx(&node, &id);
    assert_eq!(ctx["method"], "stepped", "{ctx}");
    assert_eq!(ctx["fallback"], NO_WATERFALL, "{ctx}");
    assert_eq!(radio0_info(&node).await.holders, vec![id.clone()]);
    tunes_at_least(&port, 2, Duration::from_secs(5)).await;
    node.job_stop(&id).await.unwrap();
}

/// `replace` with `method: "source"` is refused before anything: a sweep makes no
/// room, so the named holder runs on and the source is not asked.
#[tokio::test(flavor = "multi_thread")]
async fn method_source_with_replace_is_refused() {
    let (_dir, node, port) = open_with(ScanPort::sweeping(SweepScript::default())).await;
    let named = start_job(&node, start(JobKind::Spectrum, 100 * MHZ, MHZ)).await;
    let op = replacing(
        sweep_start(Some(ScanMethod::Source), None),
        std::slice::from_ref(&named),
    );
    match node.apply(op).await {
        Err(OpError::Other { message }) => {
            assert_eq!(message, "replace applies to a stepped scan")
        }
        other => panic!("expected Other, got {other:?}"),
    }
    assert!(port.requests().is_empty());
    assert!(no_scan_listed(&node));
    assert_eq!(job_state(&node, &named), Some(JobState::Running));
    node.job_stop(&named).await.unwrap();
}

/// A `graph_put` storing `bits` beside `method: "source"` or a `bin_hz` is refused
/// before the stop: a depth is for a stepped scan. The self-sweep runs on.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_put_of_bits_beside_source_or_bin_hz_is_refused() {
    let script = SweepScript {
        depths: true,
        ..Default::default()
    };
    let (_dir, node, port) = open_with(ScanPort::sweeping(script)).await;
    let (id, mut rx) = start_span(&node, SWEEP_SPAN.0, SWEEP_SPAN.1).await;
    rows(&mut rx, 4, Duration::from_secs(10)).await;
    for (k, v) in [("method", Value::from("source")), ("bin_hz", 30_000.into())] {
        let put = put_fft(&node, &id, |p| {
            p.insert("bits".into(), 8.into());
            p.insert(k.into(), v);
        });
        match put.await {
            Err(OpError::Other { message }) => {
                assert_eq!(message, "bits applies to a stepped scan", "{k}")
            }
            other => panic!("{k}: expected Other, got {other:?}"),
        }
    }
    assert_eq!(job_state(&node, &id), Some(JobState::Running));
    assert_eq!(scan_ctx(&node, &id)["method"], "source");
    assert_eq!(port.requests().len(), 1);
    assert_eq!(port.live(), 1);
    node.job_stop(&id).await.unwrap();
}
