//! `measure` on a local hardware receiver ignores the receiver's DC spike (the bins
//! within `MEASURE_DC_MASK_HZ` of the capture's centre); on a recording it does not.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use num_complex::Complex;
use tokio::sync::broadcast;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::port::{IqReceiver, Port, PortError, Tuning};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream::Payload;
use zeta_core::types::{Capabilities, Hz, JobId, SourceId, StreamKind};
use zeta_source_file::{FileOpts, FileSource};

const CENTER_HZ: u64 = 27_000_000;
const RATE_HZ: u32 = 192_000;
const BLOCK: usize = 4_800;

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

fn cfg(dir: &Path) -> NodeConfig {
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
        bank_factory: None,
        modes_dir: None,
    }
}

/// `n` samples of a tone 40 kHz above centre at 0.1 under a DC spike (a constant
/// 0.5) and 20 dB noise.
fn spike_and_tone(n: usize) -> Vec<Complex<f32>> {
    let mut iq: Vec<Complex<f32>> = (0..n)
        .map(|k| {
            let ph = 2.0 * std::f64::consts::PI * (k as f64 * 40_000.0 / RATE_HZ as f64).fract();
            Complex::from_polar(0.1, ph as f32) + Complex::new(0.5, 0.0)
        })
        .collect();
    zeta_dsp::fixtures::awgn(&mut iq, 20.0, 7);
    iq
}

fn fixture(dir: &Path) -> PathBuf {
    let iq = spike_and_tone(2 * RATE_HZ as usize);
    zeta_dsp::fixtures::write_fixture(dir, "dc", &iq, RATE_HZ, CENTER_HZ)
}

/// A looping, real-time file source `dc` on `meta`.
fn file_source(meta: &Path) -> Arc<FileSource> {
    let opts = FileOpts {
        realtime: true,
        block_samples: BLOCK,
        repeat: true,
        tune_range: None,
    };
    Arc::new(FileSource::open(SourceId("dc".into()), meta, opts).unwrap())
}

/// The recording presented as a local device: its capability record with `push: false`
/// (and `remote: false`), everything else delegated.
struct LocalCaps {
    inner: Arc<FileSource>,
    caps: Capabilities,
}

impl LocalCaps {
    fn new(inner: Arc<FileSource>) -> Self {
        let mut caps = inner.capabilities().clone();
        caps.push = false;
        caps.remote = false;
        Self { inner, caps }
    }
}

// `Port` is an `async_trait`; its `tune` written out as that macro expands it.
impl Port for LocalCaps {
    fn id(&self) -> &SourceId {
        self.inner.id()
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn tune<'life0, 'async_trait>(
        &'life0 self,
        t: Tuning,
    ) -> Pin<Box<dyn Future<Output = Result<Tuning, PortError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        self.inner.tune(t)
    }

    fn current(&self) -> Option<Tuning> {
        self.inner.current()
    }

    fn subscribe(&self) -> IqReceiver {
        self.inner.subscribe()
    }
}

/// A measure on `bandwidth_hz` around `center_hz` of the recording's band (which a
/// file source without a tune range accepts, keeping its capture at the recording's
/// centre).
async fn start_measure(node: &Node, center_hz: u64, bandwidth_hz: u64) -> JobId {
    let op = Op::JobStart {
        kind: JobKind::Measure,
        source: SourceId("dc".into()),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        elastic: false,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        r => panic!("job_start: {r:?}"),
    }
}

/// The first event of `id`'s event stream within `within`.
async fn first_event(node: &Node, id: &JobId, within: Duration) -> Option<serde_json::Value> {
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
    let Payload::Event(ev) = frame.payload else {
        panic!("expected an event")
    };
    Some(ev)
}

/// Start a measure on `band` (centre, width) of `port` (fed by `src`) and return its
/// first event's `center_hz` and its context's `measure.dc_mask_hz`.
async fn measure_on(
    dir: &Path,
    src: Arc<FileSource>,
    port: Arc<dyn Port>,
    (center_hz, bandwidth_hz): (u64, u64),
) -> (u64, u64) {
    let node = Node::open(cfg(dir), registry(), vec![port]).await.unwrap();
    src.start();
    let id = start_measure(&node, center_hz, bandwidth_hz).await;
    let ev = first_event(&node, &id, Duration::from_secs(4))
        .await
        .expect("a measure event");
    let ctx = node.job_context(&id).expect("job listed");
    let center = ev["center_hz"].as_u64().expect("center_hz");
    let mask = ctx["measure"]["dc_mask_hz"].as_u64().expect("dc_mask_hz");
    (center, mask)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_measure_on_a_local_device_ignores_the_dc_spike() {
    let dir = tempfile::tempdir().unwrap();
    let src = file_source(&fixture(dir.path()));
    let port: Arc<dyn Port> = Arc::new(LocalCaps::new(src.clone()));
    // A 100 kHz box at 27.02 MHz: the spike (27.00) 20 kHz inside it and inside the
    // mask, the tone (27.04) 20 kHz on the other side. The capture stays at the
    // recording's 27 MHz, so the DC is off the box's centre and is masked.
    let (center, mask) = measure_on(dir.path(), src, port, (CENTER_HZ + 20_000, 100_000)).await;
    // The label is not asserted: the DC offset wobbles the envelope and phase, so
    // layer 1 may say fm or carrier.
    assert!(
        center.abs_diff(CENTER_HZ + 40_000) <= 400,
        "center_hz {center}"
    );
    assert_eq!(mask, 25_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_measure_on_a_recording_measures_the_spike() {
    let dir = tempfile::tempdir().unwrap();
    let src = file_source(&fixture(dir.path()));
    let port: Arc<dyn Port> = src.clone();
    let (center, mask) = measure_on(dir.path(), src, port, (CENTER_HZ, RATE_HZ as u64)).await;
    assert!(center.abs_diff(CENTER_HZ) <= 400, "center_hz {center}");
    assert_eq!(mask, 0);
}
