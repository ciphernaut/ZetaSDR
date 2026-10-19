//! The `scan` job: time-shares one radio across a span wider than its capture.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
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

/// A 16 MHz-wide port that records every `tune` and, while tuned, sends an
/// 8192-sample block every 5 ms at its current band carrying a tone at
/// [`tone_hz`] of that band's centre.
struct ScanPort {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tunes: Mutex<Vec<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
}

impl ScanPort {
    fn new() -> Arc<Self> {
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
                remote: false,
                push: false,
            },
            cur: Mutex::new(None),
            tunes: Mutex::new(Vec::new()),
            tx: broadcast::channel(16).0,
        });
        let weak = Arc::downgrade(&p);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let Some(p) = weak.upgrade() else { return };
                let Some(t) = p.current() else { continue };
                let rate = t.bandwidth_hz.0 as f64;
                let w = 2.0 * std::f64::consts::PI * tone_hz(t.center_hz.0) / rate;
                let samples: Vec<Complex<f32>> = (0..8192)
                    .map(|n| {
                        let ph = w * n as f64;
                        Complex::new(ph.cos() as f32, ph.sin() as f32)
                    })
                    .collect();
                let t0 = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64;
                let _ = p.tx.send(IqBlock {
                    t0: Timestamp(t0),
                    sample_rate_hz: t.bandwidth_hz,
                    center_hz: t.center_hz,
                    samples: samples.into(),
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
/// per input block with the params' `size`, the block's `center_hz`, its
/// sample count and the tone it carries (mean phase step).
struct FakeFft(StageSpec);

#[async_trait]
impl Stage for FakeFft {
    fn spec(&self) -> &StageSpec {
        &self.0
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
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

fn cfg(dir: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        backends: vec![Backend::Cpu],
        resume: true,
        scan_dwell_ms: 20,
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
    }
}

async fn open() -> (tempfile::TempDir, Node, Arc<ScanPort>) {
    let dir = tempfile::tempdir().unwrap();
    let port = ScanPort::new();
    let node = Node::open(cfg(dir.path()), registry(), vec![port.clone()])
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
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.kind == StreamKind::Event && s.provenance.job.as_ref() == Some(&id))
        .expect("the scan has an event stream");
    (id.clone(), node.bus().subscribe(&stream.id).unwrap())
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
    let centres = [8 * MHZ, 24 * MHZ, 40 * MHZ];
    let mut seen = [0usize; 3];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Ok(f)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        let Payload::Event(ev) = &f.payload else {
            panic!("scan stream carries events")
        };
        assert_eq!(f.provenance.job.as_ref(), Some(&id));
        let c = ev["center_hz"].as_u64().unwrap();
        let i = centres.iter().position(|&x| x == c).expect("a step centre");
        assert_eq!(
            f.provenance.channel.as_ref().map(|c| c.0.as_str()),
            Some(format!("scan/{i}").as_str())
        );
        // spectrum_size(16 MHz) = 4096.
        assert_eq!(ev["size"], 4096);
        assert!(ev["samples"].as_u64().unwrap() >= 4096, "{ev}");
        let tone = ev["tone_hz"].as_f64().unwrap();
        assert!((tone - tone_hz(c)).abs() < 1_000.0, "step {i}: {ev}");
        seen[i] += 1;
    }
    assert!(seen.iter().all(|&n| n >= 2), "rows per step {seen:?}");

    let tunes = port.tunes();
    assert!(tunes.len() >= 6, "{} tunes", tunes.len());
    for (k, t) in tunes.iter().enumerate() {
        assert_eq!(t.center_hz.0, centres[k % 3], "tune {k}");
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
