//! The `scan` job: time-shares one radio across a span wider than its capture.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::bank::{BankFactory, ChannelBank};
use zeta_core::graph::{Graph, GraphError, Node as GraphNode};
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageRegistry, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{
    Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, StreamId, StreamKind, Timestamp,
};

const MHZ: u64 = 1_000_000;

/// The tone offset the port serves at `center_hz`, so each step has its own.
fn tone_hz(center_hz: u64) -> f64 {
    (center_hz / 100) as f64
}

/// Cadence of the port's blocks.
const BLOCK_MS: u64 = 5;

/// Samples in one block at `rate_hz`: `BLOCK_MS` of them.
fn block_len(rate_hz: u64) -> usize {
    (rate_hz * BLOCK_MS / 1_000) as usize
}

/// A 16 MHz-wide port that records every `tune` and, while tuned, sends a
/// block of `BLOCK_MS` of samples every `BLOCK_MS` at its current band,
/// carrying a tone at [`tone_hz`] of that band's centre. Block timestamps
/// advance by exactly `BLOCK_MS` from the first, so the ring sees one
/// contiguous run per step and a dwell reads several blocks. With
/// `fail_tune`, that tune (1-based) and every later one fails.
struct ScanPort {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tunes: Mutex<Vec<Tuning>>,
    fail_tune: Option<usize>,
    tx: broadcast::Sender<IqBlock>,
}

/// One block's samples at `t`: a whole number of tone cycles for every step
/// centre used here (8, 24 and 40 MHz at 16 MHz), so blocks join without a
/// phase jump.
fn block_samples(t: Tuning) -> Arc<[Complex<f32>]> {
    let rate = t.bandwidth_hz.0;
    let w = 2.0 * std::f64::consts::PI * tone_hz(t.center_hz.0) / rate as f64;
    (0..block_len(rate))
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
        Self::build(None, remote, push)
    }

    fn with_fail_tune(fail_tune: Option<usize>) -> Arc<Self> {
        Self::build(fail_tune, false, false)
    }

    fn build(fail_tune: Option<usize>, remote: bool, push: bool) -> Arc<Self> {
        let p = Arc::new(Self {
            id: SourceId("radio0".into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(MHZ)),
                tune_max_hz: Claim::Asserted(Hz(6_000 * MHZ)),
                max_bandwidth_hz: Claim::Asserted(Hz(16 * MHZ)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote,
                push,
            },
            cur: Mutex::new(None),
            tunes: Mutex::new(Vec::new()),
            fail_tune,
            tx: broadcast::channel(16).0,
        });
        let weak = Arc::downgrade(&p);
        tokio::spawn(async move {
            let base = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            let mut tick = tokio::time::interval(Duration::from_millis(BLOCK_MS));
            let mut cache: Option<(Tuning, Arc<[Complex<f32>]>)> = None;
            for k in 0u64.. {
                tick.tick().await;
                let Some(p) = weak.upgrade() else { return };
                let Some(t) = p.current() else { continue };
                if cache.as_ref().is_none_or(|(c, _)| *c != t) {
                    cache = Some((t, block_samples(t)));
                }
                let samples = cache.as_ref().expect("filled").1.clone();
                let _ = p.tx.send(IqBlock {
                    t0: Timestamp(base + k * BLOCK_MS * 1_000_000),
                    sample_rate_hz: t.bandwidth_hz,
                    center_hz: t.center_hz,
                    samples,
                });
            }
        });
        p
    }

    fn tunes(&self) -> Vec<Tuning> {
        self.tunes.lock().unwrap().clone()
    }
}

#[async_trait]
impl Port for ScanPort {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let n = self.tunes.lock().unwrap().len() + 1;
        if self.fail_tune.is_some_and(|f| n >= f) {
            return Err(PortError::Io(format!("tune {n} refused")));
        }
        self.tunes.lock().unwrap().push(t);
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

/// Stands in for the dsp `fft` stage (core cannot depend on it): one event
/// per input block with the params' `size`, `notch_hz` and `edge_mask`, the
/// block's `center_hz`, its sample count and the tone it carries (mean phase
/// step). Its params check refuses an `edge_mask` outside `0..0.5` naming the
/// field, as the real stage does.
struct FakeFft(StageSpec);

#[async_trait]
impl Stage for FakeFft {
    fn spec(&self) -> &StageSpec {
        &self.0
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
            let s = &b.samples;
            let acc: Complex<f64> = s
                .windows(2)
                .map(|w| {
                    let p = w[1] * w[0].conj();
                    Complex::new(p.re as f64, p.im as f64)
                })
                .sum();
            let tone = acc.arg() * b.sample_rate_hz.0 as f64 / (2.0 * std::f64::consts::PI);
            let ev = serde_json::json!({
                "size": params["size"],
                "notch_hz": params["notch_hz"].clone(),
                "edge_mask": params["edge_mask"].clone(),
                "center_hz": b.center_hz.0,
                "samples": s.len(),
                "tone_hz": tone,
            });
            bus.publish(Frame::new(
                outputs[0].clone(),
                frame.provenance.clone(),
                Payload::Event(ev),
            ));
        }
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(FakeFft(StageSpec {
        kind: "fft",
        inputs: vec![StreamKind::Iq],
        outputs: vec![StreamKind::Event],
        params_schema: schemars::json_schema!({}),
    })));
    r
}

fn cfg_dwell(dir: &Path, scan_dwell_ms: u64) -> NodeConfig {
    NodeConfig {
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
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(
        cfg_dwell(dir.path(), dwell_ms),
        registry(),
        vec![port.clone()],
    )
    .await
    .unwrap();
    (dir, node, port)
}

/// Start a scan over 0..40 MHz; its id and a receiver on its event stream.
async fn start_scan(node: &Node) -> (JobId, broadcast::Receiver<Frame>) {
    let id = match node.apply(start(JobKind::Scan, 20 * MHZ, 40 * MHZ)).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    };
    let rx = event_rx(node, &id);
    (id, rx)
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
    let one = block_len(16 * MHZ) as u64;
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
    // A 100 ms dwell leaves a wide margin between a tune and the step's fft.
    let (_dir, node, port) = open_dwell(ScanPort::new(), 100).await;
    let (_id, _rx) = start_scan(&node).await;
    // Both counts are taken just after a tune, inside the step's dwell, when
    // no step's fft streams are live: after the first tune, and after the
    // sixth (the last step of one even and one odd sweep, three steps each).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let count_after_tune = async |n: usize| {
        while port.tunes().len() < n {
            assert!(tokio::time::Instant::now() < deadline, "scan stalled");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        node.bus().list().len()
    };
    let before = count_after_tune(1).await;
    let after = count_after_tune(6).await;
    eprintln!("streams before {before}, after two sweeps {after}");
    assert_eq!(before, after, "per-step fft streams must be unregistered");
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
    assert_eq!(ctx["scan"]["step_hz"], 16 * MHZ, "{ctx}");
    // The harness opens the node with a 20 ms dwell.
    assert_eq!(ctx["scan"]["dwell_ms"], 20, "{ctx}");
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
    // radio0 is local hardware (remote false, push false): the defaults are in force on a 16 MHz step.
    let (_dir, node, _port) = open().await;
    let (id, mut rx) = start_scan(&node).await;
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(
        ctx["scan"]["mask"],
        serde_json::json!({"notch_hz": 50_000, "edge_mask": 0.02})
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
    assert_eq!(ev["edge_mask"], 0.02);
    let g = graph_of(&node, &id).await;
    assert_eq!(g.nodes.len(), 1);
    assert_eq!(fft_node(&g).params["notch_hz"], 50_000);
    assert_eq!(fft_node(&g).params["edge_mask"], 0.02);
    assert_eq!(fft_node(&g).params["size"], 4096);
    node.job_stop(&id).await.unwrap();
    // A remote source gets none.
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
    // covers a step too narrow for it), but with no odd sweep to fill the edges in, no edge mask.
    let (_dir, node, port) = open().await;
    let id = match node.apply(start(JobKind::Scan, 100 * MHZ, MHZ)).await {
        Ok(OpResult::Job(id)) => id,
        o => panic!("{o:?}"),
    };
    tunes_at_least(&port, 3, Duration::from_secs(3)).await;
    assert!(port.tunes().iter().all(|t| *t
        == Tuning {
            center_hz: Hz(100 * MHZ),
            bandwidth_hz: Hz(MHZ)
        }));
    let ctx = node.job_context(&id).unwrap();
    assert_eq!(ctx["scan"]["steps"], 1);
    assert!(ctx["scan"]["alternate"].is_null(), "{ctx}");
    assert_eq!(
        ctx["scan"]["mask"],
        serde_json::json!({"notch_hz": 50_000, "edge_mask": 0.0})
    );
    let g = graph_of(&node, &id).await;
    let stored = fft_node(&g);
    assert_eq!(stored.params["notch_hz"], 50_000);
    assert_eq!(stored.params["edge_mask"], 0.02);
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
