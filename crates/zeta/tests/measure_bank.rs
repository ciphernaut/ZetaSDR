//! `measure` on the bank path: a job at one channel of a wide capture emits events
//! (full seconds when the input is unbroken, partial windows when it gaps), and a
//! cut window is noted in the job's context.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use tokio::sync::broadcast;
use zeta_core::bank::BankFactory;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Hz, JobId, SourceId, StreamKind};
use zeta_dsp::bank::PfbBankFactory;
use zeta_source_file::{FileOpts, FileSource};

const CENTER_HZ: u64 = 27_000_000;
/// Wide enough for a bank: 8 bins of 256 000 Hz.
const RATE_HZ: u32 = 2_048_000;
const STEP_HZ: u64 = 25_000;
const BANDWIDTH_HZ: u64 = 12_500;
const BLOCK: usize = 16_384;

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

fn cfg(dir: &Path, bank_factory: Option<Arc<dyn BankFactory>>) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory,
    }
}

fn pfb() -> Option<Arc<dyn BankFactory>> {
    Some(Arc::new(PfbBankFactory))
}

/// Channel `i`'s centre: 25 kHz steps either side of the capture's centre.
fn channel_hz(i: usize) -> u64 {
    CENTER_HZ - 8 * STEP_HZ + i as u64 * STEP_HZ
}

/// One second at [`RATE_HZ`] of a tone on channels 0..16.
fn tones() -> Vec<Complex<f32>> {
    (0..RATE_HZ as usize)
        .map(|k| {
            (0..16).fold(Complex::new(0.0, 0.0), |acc, i| {
                let f = channel_hz(i) as f64 - CENTER_HZ as f64;
                let ph = 2.0 * std::f64::consts::PI * (k as f64 * f / RATE_HZ as f64).fract();
                acc + Complex::from_polar(0.05, ph as f32)
            })
        })
        .collect()
}

fn fixture(dir: &Path) -> PathBuf {
    zeta_dsp::fixtures::write_fixture(dir, "cb", &tones(), RATE_HZ, CENTER_HZ)
}

/// A looping, real-time file source `cb` on `meta`.
fn file_port(meta: &Path) -> (Arc<FileSource>, Arc<dyn Port>) {
    let opts = FileOpts {
        realtime: true,
        block_samples: BLOCK,
        repeat: true,
        tune_range: None,
    };
    let src = Arc::new(FileSource::open(SourceId("cb".into()), meta, opts).unwrap());
    let port: Arc<dyn Port> = src.clone();
    (src, port)
}

async fn start_measure(node: &Node, center_hz: u64) -> JobId {
    let op = Op::JobStart {
        kind: JobKind::Measure,
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

/// The first event of `id`'s event stream within `within`.
async fn first_event(
    node: &Node,
    id: &JobId,
    within: Duration,
) -> Option<(serde_json::Value, u64)> {
    let stream = node
        .bus()
        .list()
        .into_iter()
        .find(|s| s.kind == StreamKind::Event && s.provenance.job.as_ref() == Some(id))
        .expect("the job's event stream");
    let mut rx = node.bus().subscribe(&stream.id).unwrap();
    let frame = tokio::time::timeout(within, async {
        loop {
            match rx.recv().await {
                Ok(f) => return Some(f),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()??;
    let span = frame.provenance.span;
    let Payload::Event(ev) = frame.payload else {
        panic!("expected an event")
    };
    Some((ev, span.end_ns.0 - span.start_ns.0))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measure_on_the_bank_path_emits_within_4_s() {
    let dir = tempfile::tempdir().unwrap();
    let (src, port) = file_port(&fixture(dir.path()));
    let node = Node::open(cfg(dir.path(), pfb()), registry(), vec![port])
        .await
        .unwrap();
    src.start();
    let id = start_measure(&node, channel_hz(3)).await;
    let note = node.job_context(&id).expect("job listed")["channel"].clone();
    assert_eq!(note["path"], "bank");
    let (ev, span_ns) = first_event(&node, &id, Duration::from_secs(4))
        .await
        .expect("a measure event");
    assert_eq!(ev["partial"], false);
    assert!(
        (span_ns as i64 - 1_000_000_000).abs() < 25_000_000,
        "span {span_ns} ns"
    );
}
