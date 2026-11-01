//! Channel banks: several channel stages drawing from one shared PFB.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use num_complex::Complex;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeta_core::bank::{BankFactory, ChannelBank};
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::bank::PfbBankFactory;
use zeta_dsp::channel::ChannelStage;
use zeta_dsp::fft::{Window, power_spectrum_db};

const RATE: u64 = 8_000_000;
const CENTER: u64 = 100_000_000;
const BLOCK: usize = 16_384;
const BLOCKS: usize = 100;
const T0: u64 = 1_000_000_000;
const TONES: [f64; 4] = [-3_000_000.0, -1_000_000.0, 500_000.0, 2_500_000.0];
const OUT_RATE: u64 = 400_000;
/// Output samples skipped while the PFB and channel filters fill.
const SETTLE: usize = 400;

/// Block `i` of the four-tone 8 Msps stream (unit tones, absolute phase).
fn block(i: usize) -> IqBlock {
    let start = (i * BLOCK) as u64;
    let samples: Vec<Complex<f32>> = (0..BLOCK as u64)
        .map(|n| {
            TONES.iter().fold(Complex::new(0.0, 0.0), |acc, f| {
                let ph =
                    2.0 * std::f64::consts::PI * (((start + n) as f64) * f / RATE as f64).fract();
                let (s, c) = ph.sin_cos();
                acc + Complex::new(c as f32, s as f32)
            })
        })
        .collect();
    IqBlock {
        // 125 ns per sample at 8 Msps: exact.
        t0: Timestamp(T0 + start * 125),
        sample_rate_hz: Hz(RATE),
        center_hz: Hz(CENTER),
        samples: samples.into(),
    }
}

fn prov() -> Provenance {
    Provenance {
        source: SourceId("wide".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(0),
        },
    }
}

fn bank() -> Arc<dyn ChannelBank> {
    PfbBankFactory
        .make(Hz(RATE), Hz(CENTER))
        .expect("8 Msps gets a bank")
}

/// One running channel stage and what it has published.
struct Running {
    blocks: Arc<Mutex<Vec<IqBlock>>>,
    cancel: CancellationToken,
    stage: JoinHandle<Result<(), StageError>>,
    collect: JoinHandle<()>,
}

impl Running {
    fn count(&self) -> usize {
        self.blocks.lock().unwrap().len()
    }
    fn samples(&self) -> Vec<Complex<f32>> {
        let b = self.blocks.lock().unwrap();
        b.iter().flat_map(|b| b.samples.iter().copied()).collect()
    }
}

/// Spawn a channel stage reading `root` (the job's root IQ stream) with `bank`.
fn spawn(
    bus: &Arc<StreamBus>,
    root: &broadcast::Sender<Frame>,
    bank: Option<Arc<dyn ChannelBank>>,
    center_hz: u64,
    bandwidth_hz: u64,
    out_rate_hz: u64,
) -> Running {
    let out = bus.register(StreamKind::Iq, prov());
    let mut rx = bus.subscribe(&out).unwrap();
    let blocks = Arc::new(Mutex::new(Vec::new()));
    let b2 = blocks.clone();
    let collect = tokio::spawn(async move {
        while let Ok(f) = rx.recv().await {
            let Payload::Iq(b) = f.payload else {
                panic!("iq expected")
            };
            b2.lock().unwrap().push(b);
        }
    });
    let cancel = CancellationToken::new();
    let params = serde_json::json!({
        "center_hz": center_hz,
        "bandwidth_hz": bandwidth_hz,
        "out_rate_hz": out_rate_hz,
    });
    let ctx = StageContext {
        source: SourceId("wide".into()),
        bank,
        note: None,
        mode: None,
        dc_hz: None,
        mix_inputs: None,
    };
    let (bus2, rx_in, c2) = (bus.clone(), root.subscribe(), cancel.clone());
    let stage = tokio::spawn(async move {
        ChannelStage::new()
            .run(params, vec![rx_in], vec![out], bus2, c2, ctx)
            .await
    });
    Running {
        blocks,
        cancel,
        stage,
        collect,
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Push blocks `from..to` into the bank (and onto the root stream), keeping
/// every stage in `live` within 16 blocks so none lags.
async fn feed(
    bank: &Arc<dyn ChannelBank>,
    root: &broadcast::Sender<Frame>,
    live: &[&Running],
    base: &[usize],
    from: usize,
    to: usize,
) {
    for i in from..to {
        let b = block(i);
        bank.push(&b);
        let _ = root.send(Frame::new(StreamId::new(), prov(), Payload::Iq(b)));
        let ahead = i + 1 - from;
        until("stages to keep up", || {
            live.iter()
                .zip(base)
                .all(|(r, &b)| r.count() + 16 >= b + ahead)
        })
        .await;
    }
}

/// (offset of the spectrum peak from 0 Hz in FFT bins, strongest other
/// component relative to the peak in dB, outside the peak's main lobe).
fn peak(x: &[Complex<f32>]) -> (i64, f32) {
    let size = 4096;
    let spec = power_spectrum_db(x, size, Window::Hann);
    let (k, &p) = spec
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    let rel = k as i64 - (size / 2) as i64;
    let other = spec
        .iter()
        .enumerate()
        .filter(|(j, _)| (*j as i64 - k as i64).abs() > 3)
        .map(|(_, &v)| v - p)
        .fold(f32::MIN, f32::max);
    (rel, other)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_channels_from_one_bank_each_get_their_tone() {
    let bank = bank();
    assert_eq!(bank.bin_hz(), 1_000_000);
    let bus = Arc::new(StreamBus::new(256));
    let root = broadcast::channel(4).0;
    let runs: Vec<Running> = TONES
        .iter()
        .map(|f| {
            let c = (CENTER as f64 + f) as u64;
            spawn(&bus, &root, Some(bank.clone()), c, 200_000, OUT_RATE)
        })
        .collect();
    until("four bank consumers", || bank.consumers() == 4).await;
    let live: Vec<&Running> = runs.iter().collect();
    feed(&bank, &root, &live, &[0; 4], 0, BLOCKS).await;
    until("every block out", || {
        runs.iter().all(|r| r.count() == BLOCKS)
    })
    .await;

    for (r, f) in runs.iter().zip(TONES) {
        let b = r.blocks.lock().unwrap();
        assert_eq!(b[0].sample_rate_hz, Hz(OUT_RATE));
        assert_eq!(b[0].center_hz, Hz((CENTER as f64 + f) as u64));
        drop(b);
        let x = r.samples();
        assert_eq!(x.len(), BLOCKS * BLOCK / (RATE / OUT_RATE) as usize);
        let (rel, other) = peak(&x[SETTLE..]);
        let tail = &x[SETTLE..];
        let power = tail.iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / tail.len() as f64;
        let power_db = 10.0 * power.log10();
        eprintln!(
            "tone {f} Hz: {power_db:.2} dB, peak at {rel} bins, strongest other {other:.1} dB"
        );
        // A unit tone, also when centred on the boundary between two bins
        // (+0.5 and +2.5 MHz): the prototype is flat to 0.75 bin widths.
        assert!((-1.0..=0.5).contains(&power_db), "tone {f}: {power_db} dB");
        assert!(rel.abs() <= 1, "tone {f}: peak {rel} bins from 0 Hz");
        assert!(other <= -50.0, "tone {f}: other component at {other} dB");
    }
    for r in runs {
        r.cancel.cancel();
        assert!(matches!(r.stage.await.unwrap(), Err(StageError::Cancelled)));
        r.collect.abort();
    }
    assert_eq!(bank.consumers(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channel_wider_than_a_bin_uses_the_direct_path_and_matches() {
    let bank = bank();
    let bus = Arc::new(StreamBus::new(256));
    let root = broadcast::channel(256).0;
    let center = CENTER + 1_000_000;
    let with = spawn(
        &bus,
        &root,
        Some(bank.clone()),
        center,
        2_000_000,
        2_000_000,
    );
    let without = spawn(&bus, &root, None, center, 2_000_000, 2_000_000);
    until("both stages subscribed", || root.receiver_count() == 2).await;
    assert_eq!(
        bank.consumers(),
        0,
        "a 2 MHz channel must not use 1 MHz bins"
    );
    let n = 20;
    feed(&bank, &root, &[&with, &without], &[0, 0], 0, n).await;
    until("every block out", || {
        with.count() == n && without.count() == n
    })
    .await;
    let (a, b) = (with.samples(), without.samples());
    assert_eq!(a.len(), n * BLOCK / 4);
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert!((x - y).norm() <= 1e-3, "sample {i}: {x} vs {y}");
    }
    let ta: Vec<_> = with.blocks.lock().unwrap().iter().map(|b| b.t0).collect();
    let tb: Vec<_> = without
        .blocks
        .lock()
        .unwrap()
        .iter()
        .map(|b| b.t0)
        .collect();
    assert_eq!(ta, tb);
    for r in [with, without] {
        r.cancel.cancel();
        let _ = r.stage.await;
        r.collect.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_one_bank_consumer_leaves_the_others_gapless() {
    let bank = bank();
    let bus = Arc::new(StreamBus::new(256));
    let root = broadcast::channel(4).0;
    let runs: Vec<Running> = TONES
        .iter()
        .map(|f| {
            let c = (CENTER as f64 + f) as u64;
            spawn(&bus, &root, Some(bank.clone()), c, 200_000, OUT_RATE)
        })
        .collect();
    until("four bank consumers", || bank.consumers() == 4).await;
    let all: Vec<&Running> = runs.iter().collect();
    let half = BLOCKS / 2;
    feed(&bank, &root, &all, &[0; 4], 0, half).await;
    until("first half out", || runs.iter().all(|r| r.count() == half)).await;

    let mut runs = runs.into_iter();
    let stopped = runs.next().unwrap();
    let rest: Vec<Running> = runs.collect();
    stopped.cancel.cancel();
    assert!(matches!(
        stopped.stage.await.unwrap(),
        Err(StageError::Cancelled)
    ));
    stopped.collect.abort();
    assert_eq!(bank.consumers(), 3);

    let live: Vec<&Running> = rest.iter().collect();
    feed(&bank, &root, &live, &[half; 3], half, BLOCKS).await;
    until("second half out", || {
        rest.iter().all(|r| r.count() == BLOCKS)
    })
    .await;
    for r in &rest {
        let b = r.blocks.lock().unwrap();
        for w in b.windows(2) {
            let dur = w[0].samples.len() as u64 * 1_000_000_000 / OUT_RATE;
            assert_eq!(w[1].t0.0, w[0].t0.0 + dur, "gap at {}", w[0].center_hz.0);
        }
        // The first output's time is the input start less the PFB's group
        // delay (63.5 input samples, about 4 us at 8 Msps).
        assert!(
            b[0].t0.0 <= T0 + 2_500 && b[0].t0.0 + 10_000 >= T0,
            "t0 {}",
            b[0].t0.0
        );
    }
    for r in rest {
        r.cancel.cancel();
        let _ = r.stage.await;
        r.collect.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_out_rate_the_bin_rate_does_not_divide_uses_the_direct_path() {
    // 2.4 Msps: 8 bins of 300 kHz, bin rate 600 kHz, which 48 kHz does not
    // divide (a graph expanded for the source rate). The direct path serves it.
    const R: u64 = 2_400_000;
    let bank = PfbBankFactory
        .make(Hz(R), Hz(CENTER))
        .expect("2.4 Msps gets a bank");
    assert_eq!(bank.bin_hz(), 300_000);
    let bus = Arc::new(StreamBus::new(256));
    let root = broadcast::channel(256).0;
    let center = CENTER + 100_000;
    let with = spawn(&bus, &root, Some(bank.clone()), center, 12_500, 48_000);
    let without = spawn(&bus, &root, None, center, 12_500, 48_000);
    until("both stages subscribed", || root.receiver_count() == 2).await;
    assert_eq!(bank.consumers(), 0, "no bin subscription for 48 kHz");
    let n = 10;
    for i in 0..n {
        let samples: Vec<Complex<f32>> = (0..BLOCK as u64)
            .map(|k| {
                let t = (i as u64 * BLOCK as u64 + k) as f64;
                let ph = 2.0 * std::f64::consts::PI * (t * 101_000.0 / R as f64).fract();
                let (s, c) = ph.sin_cos();
                Complex::new(c as f32, s as f32)
            })
            .collect();
        let b = IqBlock {
            t0: Timestamp(T0 + i as u64 * BLOCK as u64 * 1_000_000_000 / R),
            sample_rate_hz: Hz(R),
            center_hz: Hz(CENTER),
            samples: samples.into(),
        };
        bank.push(&b);
        root.send(Frame::new(StreamId::new(), prov(), Payload::Iq(b)))
            .unwrap();
    }
    until("every block out", || {
        with.count() == n && without.count() == n
    })
    .await;
    let (a, b) = (with.samples(), without.samples());
    assert_eq!(with.blocks.lock().unwrap()[0].sample_rate_hz, Hz(48_000));
    assert_eq!(a.len(), (n * BLOCK).div_ceil(50));
    assert_eq!(a, b);
    for r in [with, without] {
        r.cancel.cancel();
        assert!(matches!(r.stage.await.unwrap(), Err(StageError::Cancelled)));
        r.collect.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bank_that_cannot_keep_up_drops_and_counts() {
    let bank = bank();
    let (mut rx, _, _) = bank.subscribe(CENTER, 200_000).unwrap();
    // 400 blocks queued at once: far more than the 64-block input queue.
    let b = block(0);
    for _ in 0..400 {
        bank.push(&b);
    }
    let dropped = bank.dropped();
    assert!(dropped > 0, "nothing dropped");
    // What was queued still comes out (the bin channel keeps the newest 64).
    let mut got = 0u64;
    let mut lagged = 0u64;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Ok(_)) => got += 1,
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => lagged += n,
            _ => break,
        }
        if got + lagged + dropped == 400 {
            break;
        }
    }
    assert_eq!(
        got + lagged + dropped,
        400,
        "got {got} lagged {lagged} dropped {dropped}"
    );
}

/// A port whose band the test changes behind the lease (as a remote node or
/// a device could), sending blocks by hand.
struct Retunable {
    id: SourceId,
    caps: zeta_core::types::Capabilities,
    band: Mutex<zeta_core::port::Tuning>,
    tx: broadcast::Sender<IqBlock>,
}

#[async_trait::async_trait]
impl zeta_core::port::Port for Retunable {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &zeta_core::types::Capabilities {
        &self.caps
    }
    async fn tune(
        &self,
        _: zeta_core::port::Tuning,
    ) -> Result<zeta_core::port::Tuning, zeta_core::port::PortError> {
        Ok(*self.band.lock().unwrap())
    }
    fn current(&self) -> Option<zeta_core::port::Tuning> {
        Some(*self.band.lock().unwrap())
    }
    fn subscribe(&self) -> zeta_core::port::IqReceiver {
        self.tx.subscribe()
    }
}

struct CountingPfb(std::sync::atomic::AtomicUsize);

impl BankFactory for CountingPfb {
    fn make(&self, rate_hz: Hz, center_hz: Hz) -> Option<Arc<dyn ChannelBank>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        PfbBankFactory.make(rate_hz, center_hz)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_band_change_fails_a_bank_served_job_with_the_reason() {
    use zeta_core::graph::{Graph, GraphId, Node as GraphNode, NodeId};
    use zeta_core::job::JobState;
    use zeta_core::node::{Node, NodeConfig};
    use zeta_core::port::Tuning;
    use zeta_core::stage::{Backend, StageRegistry};
    use zeta_core::types::{Capabilities, Claim, SampleFormat};

    let dir = tempfile::tempdir().unwrap();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    };
    let node = Node::open(cfg, reg, vec![]).await.unwrap();
    let factory = Arc::new(CountingPfb(Default::default()));
    node.set_bank_factory(factory.clone());
    let band = Tuning {
        center_hz: Hz(CENTER),
        bandwidth_hz: Hz(RATE),
    };
    let port = Arc::new(Retunable {
        id: SourceId("wide".into()),
        caps: Capabilities {
            tune_min_hz: Claim::Asserted(Hz(1_000_000)),
            tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
            max_bandwidth_hz: Claim::Asserted(Hz(RATE)),
            formats: vec![SampleFormat::Cf32],
            channels: 1,
            rx: true,
            tx: false,
            remote: false,
            push: false,
        },
        band: Mutex::new(band),
        tx: broadcast::channel(64).0,
    });
    node.add_port(port.clone()).unwrap();
    let graph = Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId("ch".into()),
            kind: "channel".into(),
            params: serde_json::json!({
                "center_hz": CENTER + 500_000,
                "bandwidth_hz": 200_000,
                "out_rate_hz": OUT_RATE,
            }),
        }],
        edges: vec![],
    };
    let job = node
        .job_start(
            "fm_demod",
            port.id.clone(),
            Tuning {
                center_hz: Hz(CENTER + 500_000),
                bandwidth_hz: Hz(200_000),
            },
            graph,
            None,
            false,
        )
        .await
        .unwrap();
    let bank = node.bank(&port.id).unwrap();
    until("the stage on the bank", || bank.consumers() == 1).await;
    for i in 0..4 {
        let _ = port.tx.send(block(i));
    }

    // The band moves under the job: blocks now come at another centre.
    let moved = Tuning {
        center_hz: Hz(CENTER + 2_000_000),
        bandwidth_hz: Hz(RATE),
    };
    *port.band.lock().unwrap() = moved;
    let failed = |node: &Node| {
        node.job_list()
            .into_iter()
            .find(|r| r.id == job)
            .and_then(|r| match r.state {
                JobState::Failed { reason } => Some(reason),
                _ => None,
            })
    };
    for i in 4..2000 {
        let mut b = block(i);
        b.center_hz = moved.center_hz;
        let _ = port.tx.send(b);
        if failed(&node).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let reason = failed(&node).expect("the job failed");
    assert!(
        reason.contains("capture band changed") && reason.contains("restart the job"),
        "{reason}"
    );
    assert!(
        reason.contains(&(CENTER + 2_000_000).to_string()),
        "{reason}"
    );
    until("the new bank", || {
        factory.0.load(std::sync::atomic::Ordering::SeqCst) == 2
    })
    .await;
    assert_eq!(node.bank(&port.id).unwrap().band(), moved);
}

/// A measure job on a bank whose input skips one block of time every 400 ms
/// (no unbroken second ever arrives) still emits, as partial windows, and
/// notes them in its context.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measure_emits_partials_when_the_input_gaps_every_400_ms() {
    use zeta_core::node::{Node, NodeConfig};
    use zeta_core::ops::{JobKind, Op, OpResult};
    use zeta_core::port::Tuning;
    use zeta_core::stage::{Backend, StageRegistry};
    use zeta_core::types::{Capabilities, Claim, SampleFormat};

    const RATE_HZ: u64 = 2_048_000;
    const CENTER_HZ: u64 = 27_000_000;
    let dir = tempfile::tempdir().unwrap();
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cfg = NodeConfig {
        state_dir: dir.path().join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: Some(Arc::new(PfbBankFactory)),
        modes_dir: None,
    };
    let node = Node::open(cfg, reg, vec![]).await.unwrap();
    let port = Arc::new(Retunable {
        id: SourceId("cb".into()),
        caps: Capabilities {
            tune_min_hz: Claim::Asserted(Hz(1_000_000)),
            tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
            max_bandwidth_hz: Claim::Asserted(Hz(RATE_HZ)),
            formats: vec![SampleFormat::Cf32],
            channels: 1,
            rx: true,
            tx: false,
            remote: false,
            push: false,
        },
        band: Mutex::new(Tuning {
            center_hz: Hz(CENTER_HZ),
            bandwidth_hz: Hz(RATE_HZ),
        }),
        tx: broadcast::channel(64).0,
    });
    node.add_port(port.clone()).unwrap();
    let tone_hz = CENTER_HZ + 100_000;
    let tx = port.tx.clone();
    tokio::spawn(async move {
        let block_ns = BLOCK as u64 * 1_000_000_000 / RATE_HZ;
        let per_400ms = (400_000_000 / block_ns) as usize;
        let mut tick = tokio::time::interval(Duration::from_nanos(block_ns));
        let mut skipped = 0u64;
        for n in 0usize.. {
            tick.tick().await;
            if n > 0 && n % per_400ms == 0 {
                skipped += 1;
            }
            let samples: Vec<Complex<f32>> = (0..BLOCK as u64)
                .map(|k| {
                    let i = (n * BLOCK) as u64 + k;
                    let ph = 2.0
                        * std::f64::consts::PI
                        * ((i as f64) * 100_000.0 / RATE_HZ as f64).fract();
                    Complex::from_polar(0.5, ph as f32)
                })
                .collect();
            let _ = tx.send(IqBlock {
                t0: Timestamp(T0 + (n as u64 + skipped) * block_ns),
                sample_rate_hz: Hz(RATE_HZ),
                center_hz: Hz(CENTER_HZ),
                samples: samples.into(),
            });
        }
    });
    let id = match node
        .apply(Op::JobStart {
            kind: JobKind::Measure,
            source: port.id.clone(),
            center_hz: Hz(tone_hz),
            bandwidth_hz: Hz(12_500),
            deviation_hz: None,
            sideband: None,
            elastic: false,
            replace: vec![],
        })
        .await
    {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start: {r:?}"),
    };
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.kind == StreamKind::Event && s.provenance.job.as_ref() == Some(&id))
        .expect("the job's event stream");
    let mut rx = node.bus().subscribe(&stream.id).unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match rx.recv().await {
                Ok(f) => return f,
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("event stream: {e}"),
            }
        }
    })
    .await
    .expect("a measure event within 4 s despite the gaps");
    let Payload::Event(ev) = &frame.payload else {
        panic!("expected an event")
    };
    assert_eq!(ev["partial"], true);
    let span = frame.provenance.span;
    assert!(span.end_ns.0 - span.start_ns.0 < 1_000_000_000);
    let ctx = node.job_context(&id).expect("job listed");
    assert!(
        ctx["measure"]["partial"].as_u64().unwrap_or(0) >= 1,
        "context {ctx}"
    );
}
