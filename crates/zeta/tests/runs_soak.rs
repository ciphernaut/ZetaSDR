//! Starting and stopping jobs on a bank source, with a pin moved between bands each
//! round, returns the node to where it began: no stage task, stream, elastic lease or
//! bank consumer outlives its job (a field node once sat at 430% CPU after two hours of
//! this; a restart cured it).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use zeta_core::bank::BankFactory;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult, SourceInfo};
use zeta_core::port::{Port, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, JobId, SourceId};
use zeta_dsp::bank::PfbBankFactory;
use zeta_source_file::{FileOpts, FileSource};

const CENTER_HZ: u64 = 27_000_000;
/// Wide enough for a bank: 8 bins of 256 000 Hz.
const RATE_HZ: u32 = 2_048_000;
const STEP_HZ: u64 = 25_000;
const CHANNELS: usize = 16;

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

fn cfg(dir: &Path, bank_factory: Option<Arc<dyn BankFactory>>) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
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

fn band(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

fn start(kind: JobKind, t: Tuning, elastic: bool) -> Op {
    Op::JobStart {
        kind,
        source: SourceId("cb".into()),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        elastic,
        replace: vec![],
    }
}

async fn started(node: &Node, op: Op) -> JobId {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("expected a job, got {r:?}"),
    }
}

async fn sources(node: &Node) -> Vec<SourceInfo> {
    match node.apply(Op::SourceList {}).await {
        Ok(OpResult::Sources(v)) => v,
        r => panic!("source_list: {r:?}"),
    }
}

/// Poll `f` every 20 ms for up to `ms`; whether it held.
async fn wait_until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while !f() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// 20 rounds of an elastic spectrum and a rigid measure on a 2.048 MHz bank source,
/// a pin moved between two bands each round, then both stopped: the stage tasks and
/// streams come back to where they began, no elastic lease and no bank consumer left.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_stop_with_pins_returns_to_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let (file, port) = port(&fixture(dir.path()));
    let node = Node::open(cfg(dir.path(), pfb()), registry(), vec![port])
        .await
        .unwrap();
    file.start();
    let src = SourceId("cb".into());
    let base = (node.debug_stage_count(), node.bus().list().len());
    for i in 0..20 {
        let s = started(
            &node,
            start(JobKind::Spectrum, band(CENTER_HZ, 2_048_000), true),
        )
        .await;
        let m = started(
            &node,
            start(JobKind::Measure, band(channel_hz(i % 16), 12_500), false),
        )
        .await;
        let c = if i % 2 == 0 {
            CENTER_HZ
        } else {
            CENTER_HZ + 512_000
        };
        node.apply(Op::SourcePin {
            source: src.clone(),
            center_hz: Hz(c),
            bandwidth_hz: Hz(1_024_000),
            replace: vec![],
        })
        .await
        .unwrap();
        node.apply(Op::JobStop { job: s }).await.unwrap();
        node.apply(Op::JobStop { job: m }).await.unwrap();
    }
    assert!(
        wait_until(2_000, || (
            node.debug_stage_count(),
            node.bus().list().len()
        ) == base)
        .await,
        "stage tasks and streams {:?}, began at {base:?}",
        (node.debug_stage_count(), node.bus().list().len())
    );
    assert!(node.lease_elastic(&src).is_empty());
    assert_eq!(
        sources(&node).await[0]
            .bank
            .as_ref()
            .map_or(0, |b| b.consumers),
        0
    );
}
