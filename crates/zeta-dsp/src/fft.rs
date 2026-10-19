//! FFT stage: power-averaged spectrum rows of an IQ stream at a fixed rate of
//! signal time, quantised, as `Event` frames (spectrum event v2).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Span, StreamId, StreamKind, Timestamp};

use crate::util::hann;
use crate::{Input, run_map};

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
    /// Linear power per bin, `size` bins fftshifted so bin 0 is the lowest
    /// frequency, normalised by the window's power sum (a full-scale tone peaks
    /// near 1) and summed over all complete `size`-sample chunks of `x`; with the
    /// number of chunks summed.
    fn power_sum(&self, x: &[Complex<f32>], size: usize, window: Window) -> PowerSum;
    /// Power spectrum in dB relative to full scale: [`FftBackend::power_sum`]
    /// averaged over its chunks.
    fn power_spectrum_db(&self, x: &[Complex<f32>], size: usize, window: Window) -> Vec<f32> {
        self.power_sum(x, size, window).db()
    }
}

/// Linear power summed over `chunks` FFTs (see [`FftBackend::power_sum`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PowerSum {
    pub sum: Vec<f64>,
    pub chunks: usize,
}

impl PowerSum {
    pub fn new(size: usize) -> Self {
        Self {
            sum: vec![0.0; size],
            chunks: 0,
        }
    }

    /// Adds `other`'s chunks (same size).
    pub fn add(&mut self, other: &PowerSum) {
        for (a, b) in self.sum.iter_mut().zip(&other.sum) {
            *a += b;
        }
        self.chunks += other.chunks;
    }

    /// The average power per bin in dB (very low when no chunk was summed).
    pub fn db(&self) -> Vec<f32> {
        let scale = if self.chunks == 0 {
            0.0
        } else {
            1.0 / self.chunks as f64
        };
        self.sum
            .iter()
            .map(|&p| 10.0 * ((p * scale) as f32 + DB_FLOOR).log10())
            .collect()
    }
}

/// The shared windowing and summing path. `mag2` receives one windowed
/// `size`-sample chunk and returns `|FFT|^2` per bin in natural (unshifted) order.
/// Linear power is summed over chunks, normalised by the window's power sum and
/// fftshifted.
pub(crate) fn chunk_power(
    x: &[Complex<f32>],
    size: usize,
    window: Window,
    mag2: impl Fn(&[Complex<f32>]) -> Vec<f32>,
) -> PowerSum {
    let w = window.coeffs(size);
    let norm = 1.0 / (w.iter().sum::<f32>() as f64).powi(2);
    let mut acc = vec![0.0f64; size];
    let mut chunks = 0usize;
    for chunk in x.chunks_exact(size) {
        let windowed: Vec<Complex<f32>> = chunk.iter().zip(&w).map(|(&s, &c)| s * c).collect();
        for (a, p) in acc.iter_mut().zip(mag2(&windowed)) {
            *a += p as f64;
        }
        chunks += 1;
    }
    // fftshift: natural bins [0, size/2) are the non-negative frequencies.
    let half = size / 2;
    PowerSum {
        sum: (0..size).map(|k| acc[(k + half) % size] * norm).collect(),
        chunks,
    }
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

    fn power_sum(&self, x: &[Complex<f32>], size: usize, window: Window) -> PowerSum {
        let plan = self.plan(size);
        chunk_power(x, size, window, |chunk| {
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
    /// Rows per second of signal time.
    #[schemars(range(min = 1, max = 60))]
    pub rows_per_s: u32,
}

/// Quantisation step of a row's `q` values.
pub const STEP_DB: f32 = 0.5;
/// A row's `floor_db` is this percentile of its dB values ...
const FLOOR_PERCENTILE: f32 = 0.02;
/// ... minus this margin.
const FLOOR_MARGIN_DB: f32 = 5.0;
/// Accepted `rows_per_s`.
const ROWS_PER_S: std::ops::RangeInclusive<u32> = 1..=60;

/// `(floor_db, q)` for a row of dB values: `floor_db` is the row's 2nd-percentile
/// value minus 5 dB and `q = clamp(round((db - floor_db) / STEP_DB), 0, 255)`, so
/// `floor_db + q * STEP_DB` is within half a step of `db` over 127.5 dB above the floor.
pub fn quantise_row(db: &[f32]) -> (f32, Vec<u8>) {
    if db.is_empty() {
        return (0.0, Vec::new());
    }
    let mut sorted = db.to_vec();
    sorted.sort_by(f32::total_cmp);
    let idx = (FLOOR_PERCENTILE * (sorted.len() - 1) as f32) as usize;
    let floor = sorted[idx] - FLOOR_MARGIN_DB;
    let q = db
        .iter()
        .map(|&v| ((v - floor) / STEP_DB).round().clamp(0.0, 255.0) as u8)
        .collect();
    (floor, q)
}

/// Nanoseconds of `n` samples at `rate_hz`.
fn ns_of(n: usize, rate_hz: u64) -> u64 {
    if rate_hz == 0 {
        0
    } else {
        (n as u128 * 1_000_000_000 / rate_hz as u128) as u64
    }
}

/// One completed row: its averaged power and the signal time it covers.
struct Row {
    power: PowerSum,
    start_ns: u64,
    end_ns: u64,
    rate_hz: u64,
    center_hz: u64,
}

/// Cuts a sample stream into consecutive `size`-sample chunks (across block
/// boundaries) and groups their power into rows of `1 / rows_per_s` seconds of
/// signal time, counted in samples. Row `k` closes at the first chunk ending at or
/// after sample `ceil((k + 1) * rate / rows_per_s)` since the stream (re)started;
/// a row shorter than one chunk therefore holds one chunk and the next rows catch up.
/// A change of rate or centre restarts the stream (the partial row is dropped).
struct RowCutter {
    size: usize,
    window: Window,
    rows_per_s: u64,
    rate_hz: u64,
    center_hz: u64,
    /// Samples of a chunk not yet complete, and the time of its first sample.
    carry: Vec<Complex<f32>>,
    carry_t0_ns: u64,
    /// Samples in completed chunks since the stream (re)started.
    consumed: u64,
    /// Index of the row being filled.
    row_index: u64,
    row: PowerSum,
    row_start_ns: Option<u64>,
    row_end_ns: u64,
}

impl RowCutter {
    fn new(size: usize, window: Window, rows_per_s: u32) -> Self {
        Self {
            size,
            window,
            rows_per_s: rows_per_s as u64,
            rate_hz: 0,
            center_hz: 0,
            carry: Vec::with_capacity(size),
            carry_t0_ns: 0,
            consumed: 0,
            row_index: 0,
            row: PowerSum::new(size),
            row_start_ns: None,
            row_end_ns: 0,
        }
    }

    fn restart(&mut self, rate_hz: u64, center_hz: u64) {
        self.rate_hz = rate_hz;
        self.center_hz = center_hz;
        self.carry.clear();
        self.consumed = 0;
        self.row_index = 0;
        self.row = PowerSum::new(self.size);
        self.row_start_ns = None;
    }

    /// Samples since the restart at which the current row closes.
    fn boundary(&self) -> u64 {
        ((self.row_index + 1) * self.rate_hz).div_ceil(self.rows_per_s)
    }

    /// Frames were lost: the next chunk starts afresh (the row keeps its chunks).
    fn gap(&mut self) {
        self.carry.clear();
    }

    /// Adds `p` (covering `[start_ns, end_ns)`); the row when this closes one.
    fn add(&mut self, p: &PowerSum, start_ns: u64, end_ns: u64) -> Option<Row> {
        self.row.add(p);
        self.consumed += (p.chunks * self.size) as u64;
        self.row_start_ns.get_or_insert(start_ns);
        self.row_end_ns = end_ns;
        if self.consumed < self.boundary() {
            return None;
        }
        while self.boundary() <= self.consumed {
            self.row_index += 1;
        }
        let power = std::mem::replace(&mut self.row, PowerSum::new(self.size));
        Some(Row {
            power,
            start_ns: self.row_start_ns.take().unwrap_or(start_ns),
            end_ns,
            rate_hz: self.rate_hz,
            center_hz: self.center_hz,
        })
    }

    fn push(&mut self, block: &IqBlock, backend: &dyn FftBackend) -> Vec<Row> {
        let (rate, center) = (block.sample_rate_hz.0, block.center_hz.0);
        if rate != self.rate_hz || center != self.center_hz {
            self.restart(rate, center);
        }
        let (size, window) = (self.size, self.window);
        let x = &block.samples[..];
        let t = |i: usize| block.t0.0.saturating_add(ns_of(i, rate));
        let mut rows = Vec::new();
        let mut i = 0;
        if !self.carry.is_empty() {
            let need = size - self.carry.len();
            if x.len() < need {
                self.carry.extend_from_slice(x);
                return rows;
            }
            self.carry.extend_from_slice(&x[..need]);
            i = need;
            let p = backend.power_sum(&self.carry, size, window);
            let start = self.carry_t0_ns;
            self.carry.clear();
            rows.extend(self.add(&p, start, t(i)));
        }
        while x.len() - i >= size {
            let avail = (x.len() - i) / size;
            let to_boundary = (self.boundary().saturating_sub(self.consumed) as usize)
                .div_ceil(size)
                .max(1);
            let n = avail.min(to_boundary);
            let p = backend.power_sum(&x[i..i + n * size], size, window);
            rows.extend(self.add(&p, t(i), t(i + n * size)));
            i += n * size;
        }
        if i < x.len() {
            self.carry.extend_from_slice(&x[i..]);
            self.carry_t0_ns = t(i);
        }
        rows
    }
}

/// Turns an IQ stream into spectrum rows at `rows_per_s` per second of signal
/// time (spectrum event v2). Every FFT of consecutive `size`-sample chunks between
/// two rows is power-averaged (linear power); each row is emitted as
/// `{size, bin_hz, rows_per_s, center_hz, floor_db, step_db, q}` with
/// `db = floor_db + q * step_db` (see [`quantise_row`]); the frame's provenance
/// span covers the averaged chunks.
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
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: FftParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("fft params: {e}")))?;
        if !SIZES.contains(&p.size) {
            return Err(StageError::Failed(format!(
                "fft params: size {} must be one of {SIZES:?}",
                p.size
            )));
        }
        if !ROWS_PER_S.contains(&p.rows_per_s) {
            return Err(StageError::Failed(format!(
                "fft params: rows_per_s {} must be in {ROWS_PER_S:?}",
                p.rows_per_s
            )));
        }
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let size = p.size as usize;
        let rows_per_s = p.rows_per_s;
        let backend = self.backend.clone();
        let mut cutter = RowCutter::new(size, p.window, rows_per_s);
        run_map(inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    cutter.gap();
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed("fft stage expects iq frames".into()));
            };
            Ok(cutter
                .push(block, backend.as_ref())
                .into_iter()
                .map(|row| {
                    let (floor_db, q) = quantise_row(&row.power.db());
                    let mut prov = frame.provenance.clone();
                    prov.span = Span {
                        start_ns: Timestamp(row.start_ns),
                        end_ns: Timestamp(row.end_ns),
                    };
                    Frame::new(
                        out.clone(),
                        prov,
                        Payload::Event(serde_json::json!({
                            "size": size,
                            "bin_hz": row.rate_hz as f64 / size as f64,
                            "rows_per_s": rows_per_s,
                            "center_hz": row.center_hz,
                            "floor_db": floor_db,
                            "step_db": STEP_DB,
                            "q": q,
                        })),
                    )
                })
                .collect())
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
                params: serde_json::json!({"size": size, "window": "hann", "rows_per_s": 20}),
            }],
            edges: vec![],
        };
        assert!(matches!(
            validate(&g(1000), &r),
            Err(GraphError::BadParams { .. })
        ));
        assert_eq!(validate(&g(1024), &r), Ok(()));
        assert_eq!(validate(&g(8192), &r), Ok(()));
    }

    #[test]
    fn quantise_row_floor_is_2nd_percentile_minus_5() {
        let db: Vec<f32> = (0..100).map(|i| -100.0 + 2.0 * i as f32).collect();
        let (floor, q) = quantise_row(&db);
        assert_eq!(floor, -98.0 - 5.0);
        assert_eq!(q[0], 6);
        assert_eq!(q[50], 206);
        assert_eq!(q[99], 255); // 201 dB above the floor clamps at 127.5
    }

    #[tokio::test]
    async fn fft_stage_emits_v2_rows() {
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
                    serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20}),
                    vec![rx_in],
                    vec![o2],
                    bus,
                    CancellationToken::new(),
                    zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
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
        // 4800 samples at 48 kHz, 20 rows/s: one 50 ms row (3 chunks) closes in this block.
        assert_eq!(ev["q"].as_array().unwrap().len(), 1024);
        assert_eq!(ev["bin_hz"].as_f64(), Some(46.875));
        assert_eq!(ev["size"], 1024);
        assert_eq!(ev["rows_per_s"], 20);
        assert_eq!(ev["step_db"].as_f64(), Some(0.5));
        assert!(ev["floor_db"].as_f64().is_some());
        assert_eq!(ev["center_hz"], 100_000_000u64);
        assert_eq!(f.provenance.span.start_ns, Timestamp(5));
        assert_eq!(
            f.provenance.span.end_ns,
            Timestamp(5 + 3072 * 1_000_000_000 / RATE)
        );
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        assert!(
            rx_out.try_recv().is_err(),
            "the rest of the block is a partial row"
        );
    }
}
