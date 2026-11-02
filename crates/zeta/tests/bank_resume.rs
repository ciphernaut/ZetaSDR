//! The channel bank on the real chain: channel stages take it whether their jobs start
//! fresh or resume (the factory reaches `Node::open`), and the path each took, the
//! source's bank and the lag by stage kind are all observable.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use zeta_core::bank::BankFactory;
use zeta_core::job::JobState;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{BankInfo, JobKind, Op, OpResult, SourceInfo};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId};
use zeta_dsp::bank::PfbBankFactory;
use zeta_source_file::{FileOpts, FileSource};

const CENTER_HZ: u64 = 27_000_000;
/// Wide enough for a bank: 8 bins of 256 000 Hz.
const RATE_HZ: u32 = 2_048_000;
const STEP_HZ: u64 = 25_000;
const CHANNELS: usize = 16;
const BANDWIDTH_HZ: u64 = 12_500;

/// The lag counters are process-wide: the tests run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

fn cfg(dir: &Path, bank_factory: Option<Arc<dyn BankFactory>>) -> NodeConfig {
    NodeConfig {
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory,
        modes_dir: None,
    }
}

fn pfb() -> Option<Arc<dyn BankFactory>> {
    Some(Arc::new(PfbBankFactory))
}

/// Channel `i`'s centre: 25 kHz steps either side of the capture's centre.
fn channel_hz(i: usize) -> u64 {
    CENTER_HZ - 8 * STEP_HZ + i as u64 * STEP_HZ
}

/// One second at [`RATE_HZ`] of a tone on every channel.
fn fixture(dir: &Path) -> PathBuf {
    let n = RATE_HZ as usize;
    let iq: Vec<Complex<f32>> = (0..n)
        .map(|k| {
            (0..CHANNELS).fold(Complex::new(0.0, 0.0), |acc, i| {
                let f = channel_hz(i) as f64 - CENTER_HZ as f64;
                let ph = 2.0 * std::f64::consts::PI * (k as f64 * f / RATE_HZ as f64).fract();
                acc + Complex::from_polar(0.05, ph as f32)
            })
        })
        .collect();
    zeta_dsp::fixtures::write_fixture(dir, "cb", &iq, RATE_HZ, CENTER_HZ)
}

/// A looping, real-time file source `cb` on `meta`.
fn port(meta: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    let opts = FileOpts {
        realtime: true,
        block_samples: 16_384,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("cb".into()), meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

async fn start(node: &Node, center_hz: u64) -> JobId {
    let op = Op::JobStart {
        kind: JobKind::FmDemod,
        source: SourceId("cb".into()),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(BANDWIDTH_HZ),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start at {center_hz} Hz: {r:?}"),
    }
}

async fn sources(node: &Node) -> Vec<SourceInfo> {
    match node.apply(Op::SourceList {}).await {
        Ok(OpResult::Sources(v)) => v,
        r => panic!("source_list: {r:?}"),
    }
}

fn channel_note(node: &Node, id: &JobId) -> serde_json::Value {
    node.job_context(id).expect("job listed")["channel"].clone()
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn bank_note() -> serde_json::Value {
    serde_json::json!({"path": "bank", "bin_hz": 256_000})
}

/// The kinds whose lag count grew since `before`.
fn lagged_since(before: &[(&'static str, u64)]) -> Vec<(&'static str, u64)> {
    zeta_dsp::lagged_frames_by_kind()
        .into_iter()
        .filter(|(k, n)| {
            let was = before.iter().find(|(b, _)| b == k).map_or(0, |(_, n)| *n);
            *n > was
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_channels_take_the_bank() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(&fixture(dir.path()));
    let node = Node::open(cfg(dir.path(), pfb()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let lagged = zeta_dsp::lagged_frames();
    let mut ids = Vec::new();
    for i in 0..CHANNELS {
        ids.push(start(&node, channel_hz(i)).await);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    for id in &ids {
        assert_eq!(channel_note(&node, id), bank_note(), "job {}", id.0);
    }
    assert_eq!(
        sources(&node).await[0].bank,
        Some(BankInfo {
            bin_hz: 256_000,
            consumers: CHANNELS,
            dropped: 0,
        })
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(zeta_dsp::lagged_frames(), lagged);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resumed_channels_take_the_bank_like_fresh_ones() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let meta = fixture(dir.path());
    let ids = {
        let (src, port) = port(&meta);
        let node = Node::open(cfg(dir.path(), pfb()), registry(), vec![port])
            .await
            .unwrap();
        src.start();
        let mut ids = Vec::new();
        for i in 0..CHANNELS {
            ids.push(start(&node, channel_hz(i)).await);
        }
        ids
    };
    let (src, port) = port(&meta);
    let node = Node::open(cfg(dir.path(), pfb()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let before = zeta_dsp::lagged_frames_by_kind();
    until(
        "every resumed job running on the bank",
        Duration::from_secs(2),
        || {
            let jobs = node.job_list();
            ids.iter().all(|id| {
                jobs.iter()
                    .any(|r| &r.id == id && r.state == JobState::Running)
                    && channel_note(&node, id) == bank_note()
            })
        },
    )
    .await;
    assert_eq!(
        node.bank(&SourceId("cb".into())).map(|b| b.consumers()),
        Some(CHANNELS)
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(lagged_since(&before), []);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_factory_the_path_is_direct_and_says_so() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = port(&fixture(dir.path()));
    let node = Node::open(cfg(dir.path(), None), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start(&node, channel_hz(0)).await;
    let direct = serde_json::json!({"path": "direct", "reason": "no bank"});
    until("the direct note", Duration::from_secs(1), || {
        channel_note(&node, &id) == direct
    })
    .await;
    assert_eq!(sources(&node).await[0].bank, None);
}
