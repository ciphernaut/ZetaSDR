//! The `scan` job: time-shares one radio across a span wider than its capture.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
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
use zeta_core::ops::{JobKind, Op, OpError, OpResult, scan_dwell_samples, spectrum_row_boundary};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageRegistry, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{
    Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, Span, StreamId, StreamKind, Timestamp,
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
/// `fail_tune`, that tune (1-based) and every later one fails. Its record can be
/// replaced once ([`ScanPort::replace_caps`]), as a worker port's is by a
/// respawn's hello.
struct ScanPort {
    id: SourceId,
    caps: Capabilities,
    later: OnceLock<Capabilities>,
    cur: Mutex<Option<Tuning>>,
    tunes: Mutex<Vec<(Tuning, Instant)>>,
    fail_tune: Option<usize>,
    /// The `t0` of every block a `Gapping` port started late.
    gaps: Mutex<Vec<u64>>,
    /// While set, no block is sent ([`ScanPort::hold`]).
    held: AtomicBool,
    tx: broadcast::Sender<IqBlock>,
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

    fn build(
        fail_tune: Option<usize>,
        remote: bool,
        push: bool,
        delivery: Delivery,
        max_bandwidth_hz: u64,
    ) -> Arc<Self> {
        let p = Arc::new(Self {
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
            },
            later: OnceLock::new(),
            cur: Mutex::new(None),
            tunes: Mutex::new(Vec::new()),
            fail_tune,
            gaps: Mutex::new(Vec::new()),
            held: AtomicBool::new(false),
            tx: broadcast::channel(16).0,
        });
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
        if self.fail_tune.is_some_and(|f| n >= f) {
            return Err(PortError::Io(format!("tune {n} refused")));
        }
        self.tunes.lock().unwrap().push((t, Instant::now()));
        *self.cur.lock().unwrap() = Some(t);
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

/// Stands in for the dsp `fft` stage (core cannot depend on it). It cuts each
/// input block into rows as the real stage does: `row_frames` frames of `size`
/// each when the params give it, else closing row `k` at the first frame ending
/// at or after [`spectrum_row_boundary`]; a partial row at the block's end is dropped. One event per row with
/// the params' `size`, `rows_per_s`, `row_frames`, `notch_hz` and `edge_mask`,
/// the block's `center_hz`, the row's sample count and the tone it carries (mean
/// phase step); the frame's span is the row's. Its params check refuses an
/// `edge_mask` outside `0..0.5` naming the field, as the real stage does. The
/// slow one sleeps `delay` before each run.
struct FakeFft {
    spec: StageSpec,
    delay: Duration,
}

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
        elastic: false,
        replace: vec![],
    }
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
    // A reader takes a missing `method` as `stepped`.
    assert!(node.job_context(&id).unwrap()["scan"]["method"].is_null());
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
#[tokio::test(flavor = "multi_thread")]
async fn a_skip_in_an_unwritten_sweep_shows_at_the_next_write() {
    let (_dir, node, port) = open_with(ScanPort::fast()).await;
    let (id, _rx) = start_span(&node, 100 * MHZ, FAST_RATE_HZ).await;
    until("a timing", Duration::from_secs(10), || {
        timing(&node, &id).is_some()
    })
    .await;
    let first = timing(&node, &id).unwrap()["at_ns"].as_u64().unwrap();
    // Stall the port right after the first write: once the ring's backlog is
    // used up one step waits out its 520 ms timeout, a skip in a sweep that ends
    // inside the second the throttle holds the record back. A tune after a quiet
    // spell of 450 ms is that step's end; the samples come back then.
    port.hold(true);
    let mut last = (port.tunes().len(), Instant::now());
    until("a step given up on", Duration::from_secs(10), || {
        let n = port.tunes().len();
        if n == last.0 {
            return false;
        }
        let quiet = last.1.elapsed() >= Duration::from_millis(450);
        last = (n, Instant::now());
        quiet
    })
    .await;
    port.hold(false);
    until("a skip reported", Duration::from_secs(10), || {
        timing(&node, &id).is_some_and(|t| t["skipped"].as_u64().unwrap_or(0) >= 1)
    })
    .await;
    let t = timing(&node, &id).unwrap();
    assert!(t["at_ns"].as_u64().unwrap() > first, "{t}");
    node.job_stop(&id).await.unwrap();
}
