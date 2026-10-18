//! FFT stage: averaged power spectrum of an IQ stream, as `Event` frames.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{StreamId, StreamKind};

use crate::util::hann;
use crate::{Input, frame_span, run_map};

/// Floor added to linear power before the dB conversion.
const DB_FLOOR: f32 = 1e-20;

/// FFT sizes the stage accepts.
const SIZES: [u32; 11] = [
    64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
];

/// Analysis window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    Hann,
    Rect,
}

impl Window {
    fn coeffs(self, n: usize) -> Vec<f32> {
        match self {
            Window::Hann => hann(n),
            Window::Rect => vec![1.0; n],
        }
    }
}

/// Computes spectra. Backends differ only in the per-chunk FFT.
pub trait FftBackend: Send + Sync {
    fn backend(&self) -> Backend;
    /// Power spectrum in dB relative to full scale, `size` bins fftshifted so
    /// bin 0 is the lowest frequency, averaged over all complete `size`-sample
    /// chunks of `x`.
    fn power_spectrum_db(&self, x: &[Complex<f32>], size: usize, window: Window) -> Vec<f32>;
}

/// The shared windowing, averaging and dB path. `mag2` receives one windowed
/// `size`-sample chunk and returns `|FFT|^2` per bin in natural (unshifted) order.
/// Linear power is averaged over chunks, normalised by the window's power sum
/// (so a full-scale tone peaks near 0 dB), shifted and converted to dB.
pub(crate) fn chunk_average(
    x: &[Complex<f32>],
    size: usize,
    window: Window,
    mag2: impl Fn(&[Complex<f32>]) -> Vec<f32>,
) -> Vec<f32> {
    let w = window.coeffs(size);
    let norm = w.iter().sum::<f32>().powi(2);
    let mut acc = vec![0.0f64; size];
    let mut chunks = 0usize;
    for chunk in x.chunks_exact(size) {
        let windowed: Vec<Complex<f32>> = chunk.iter().zip(&w).map(|(&s, &c)| s * c).collect();
        for (a, p) in acc.iter_mut().zip(mag2(&windowed)) {
            *a += p as f64;
        }
        chunks += 1;
    }
    let scale = if chunks == 0 {
        0.0
    } else {
        1.0 / (chunks as f64 * norm as f64)
    };
    // fftshift: natural bins [0, size/2) are the non-negative frequencies.
    let half = size / 2;
    (0..size)
        .map(|k| {
            let p = (acc[(k + half) % size] * scale) as f32;
            10.0 * (p + DB_FLOOR).log10()
        })
        .collect()
}

/// CPU backend: rustfft plans cached per size.
#[derive(Default)]
pub struct CpuFft {
    plans: Mutex<HashMap<usize, Arc<dyn Fft<f32>>>>,
}

impl CpuFft {
    pub fn new() -> Self {
        Self::default()
    }

    fn plan(&self, size: usize) -> Arc<dyn Fft<f32>> {
        let mut plans = self.plans.lock().unwrap_or_else(|e| e.into_inner());
        plans
            .entry(size)
            .or_insert_with(|| FftPlanner::new().plan_fft_forward(size))
            .clone()
    }
}

impl FftBackend for CpuFft {
    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn power_spectrum_db(&self, x: &[Complex<f32>], size: usize, window: Window) -> Vec<f32> {
        let plan = self.plan(size);
        chunk_average(x, size, window, |chunk| {
            let mut buf = chunk.to_vec();
            plan.process(&mut buf);
            buf.iter().map(|c| c.norm_sqr()).collect()
        })
    }
}

/// [`CpuFft`] spectrum of `x`; see [`FftBackend::power_spectrum_db`].
pub fn power_spectrum_db(x: &[Complex<f32>], size: usize, window: Window) -> Vec<f32> {
    CpuFft::new().power_spectrum_db(x, size, window)
}

fn size_schema(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type": "integer", "enum": SIZES})
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct FftParams {
    #[schemars(schema_with = "size_schema")]
    pub size: u32,
    pub window: Window,
}

/// Turns each IQ block into one averaged power-spectrum event, averaging the
/// block's complete `size`-sample chunks. A block shorter than `size` emits no
/// event, and a trailing partial chunk of a longer block is dropped.
pub struct FftStage {
    spec: StageSpec,
    backend: Arc<dyn FftBackend>,
}

impl FftStage {
    pub fn new(backend: Arc<dyn FftBackend>) -> Self {
        Self {
            spec: StageSpec {
                kind: "fft",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(FftParams),
            },
            backend,
        }
    }
}

#[async_trait]
impl Stage for FftStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        self.backend.backend()
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
    ) -> Result<(), StageError> {
        let p: FftParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("fft params: {e}")))?;
        if !SIZES.contains(&p.size) {
            return Err(StageError::Failed(format!(
                "fft params: size {} must be one of {SIZES:?}",
                p.size
            )));
        }
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let size = p.size as usize;
        let backend = self.backend.clone();
        run_map(inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed("fft stage expects iq frames".into()));
            };
            if block.samples.len() < size {
                return Ok(Vec::new());
            }
            let rate = block.sample_rate_hz.0;
            let db = backend.power_spectrum_db(&block.samples, size, p.window);
            let mut prov = frame.provenance.clone();
            prov.span = frame_span(block.t0, block.samples.len(), rate);
            Ok(vec![Frame::new(
                out.clone(),
                prov,
                Payload::Event(serde_json::json!({
                    "t0_ns": block.t0.0,
                    "center_hz": block.center_hz.0,
                    "sample_rate_hz": rate,
                    "size": size,
                    "bin_hz": rate as f64 / size as f64,
                    "db": db,
                })),
            )])
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use zeta_core::graph::{Graph, GraphError, GraphId, Node, NodeId, validate};
    use zeta_core::port::IqBlock;
    use zeta_core::stage::StageRegistry;
    use zeta_core::types::{Hz, Provenance, SourceId, Span, Timestamp};

    use super::*;

    const RATE: u64 = 48_000;

    #[test]
    fn power_spectrum_peak_at_tone_bin() {
        let x = crate::util::nco(4096, 6_000.0, RATE as f64);
        let db = power_spectrum_db(&x, 1024, Window::Hann);
        assert_eq!(db.len(), 1024);
        let (argmax, peak) = db
            .iter()
            .enumerate()
            .fold((0, f32::MIN), |b, (i, &v)| if v > b.1 { (i, v) } else { b });
        assert_eq!(argmax, 512 + 128);
        let mut sorted = db.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[512];
        assert!(peak - median >= 40.0, "peak {peak} median {median}");
        assert!(
            peak.abs() < 1.0,
            "full-scale tone should be near 0 dB: {peak}"
        );
    }

    #[test]
    fn fft_size_schema_rejects_1000() {
        let mut r = StageRegistry::new();
        r.register(Arc::new(FftStage::new(Arc::new(CpuFft::new()))));
        let g = |size: u32| Graph {
            id: GraphId::new(),
            nodes: vec![Node {
                id: NodeId("a".into()),
                kind: "fft".into(),
                params: serde_json::json!({"size": size, "window": "hann"}),
            }],
            edges: vec![],
        };
        assert!(matches!(
            validate(&g(1000), &r),
            Err(GraphError::BadParams { .. })
        ));
        assert_eq!(validate(&g(1024), &r), Ok(()));
    }

    #[tokio::test]
    async fn fft_stage_emits_one_event_per_block() {
        let prov = Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Event, prov.clone());
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(8);
        let o2 = out.clone();
        let h = tokio::spawn(async move {
            FftStage::new(Arc::new(CpuFft::new()))
                .run(
                    serde_json::json!({"size": 1024, "window": "hann"}),
                    vec![rx_in],
                    vec![o2],
                    bus,
                    CancellationToken::new(),
                )
                .await
        });
        tx.send(Frame::new(
            StreamId::new(),
            prov,
            Payload::Iq(IqBlock {
                t0: Timestamp(5),
                sample_rate_hz: Hz(RATE),
                center_hz: Hz(100_000_000),
                samples: crate::util::nco(4800, 6_000.0, RATE as f64).into(),
            }),
        ))
        .unwrap();
        let f = rx_out.recv().await.unwrap();
        let Payload::Event(ev) = &f.payload else {
            panic!("expected an event")
        };
        assert_eq!(ev["db"].as_array().unwrap().len(), 1024);
        assert_eq!(ev["bin_hz"].as_f64(), Some(46.875));
        assert_eq!(ev["size"], 1024);
        assert_eq!(ev["t0_ns"], 5);
        assert_eq!(ev["center_hz"], 100_000_000u64);
        assert_eq!(ev["sample_rate_hz"], RATE);
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }
}
