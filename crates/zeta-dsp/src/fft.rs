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
pub use zeta_core::spectrum::{STEP_DB, quantise_row, quantise_row_masked};
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
    /// Rows per second of signal time; graphs persisted before it existed get 20.
    #[serde(default = "rows_per_s_default")]
    #[schemars(range(min = 1, max = 60), default = "rows_per_s_default")]
    pub rows_per_s: u32,
    /// Width (Hz) of the notch masked about the DC bin; 0 masks nothing. Signed so a
    /// negative value reaches [`FftParams::check`] and is refused naming the field.
    #[serde(default)]
    pub notch_hz: i64,
    /// Fraction of the row masked at each edge, in `0..0.5`; 0 masks nothing.
    #[serde(default)]
    pub edge_mask: f64,
    /// When set (at least 1), a row is the mean of exactly this many frames, whatever
    /// `rows_per_s` says (which the row still reports): a scan step's one row of its
    /// dwell. Absent or `null`, rows follow `rows_per_s`.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub row_frames: Option<u32>,
    /// A `scan`'s dwell at each step, ms (1..=5000); absent or `null` is the node's
    /// (`--scan-dwell-ms`). Read by the scan job from its stored `fft` node; the stage
    /// itself ignores it.
    #[serde(default)]
    #[schemars(range(min = 1, max = 5000))]
    pub dwell_ms: Option<u64>,
    /// A `scan`'s rows are the mean power of each step's last this many sweeps
    /// (1..=64); absent or `null` is 1. Read by the scan job as `dwell_ms` is.
    #[serde(default)]
    #[schemars(range(min = 1, max = 64))]
    pub average: Option<u32>,
    /// A `scan`'s sample depth on its source's link, one of the source's
    /// `depths`; absent or `null` is the source's own. Read by the scan job as
    /// `dwell_ms` is, which checks it against the source.
    #[serde(default)]
    pub bits: Option<u8>,
}

impl FftParams {
    /// What the schema cannot say: `notch_hz` not negative, `edge_mask` in `0..0.5`,
    /// `row_frames` at least 1, `dwell_ms` in `1..=SCAN_DWELL_MAX_MS` and `average` in
    /// `1..=SCAN_AVERAGE_MAX`. The error names the field.
    pub fn check(&self) -> Result<(), String> {
        use zeta_core::ops::{SCAN_AVERAGE_MAX, SCAN_DWELL_MAX_MS};
        if self.notch_hz < 0 {
            return Err("notch_hz must be at least 0".into());
        }
        if self.row_frames == Some(0) {
            return Err("row_frames must be at least 1".into());
        }
        if let Some(d) = self.dwell_ms
            && !(1..=SCAN_DWELL_MAX_MS).contains(&d)
        {
            return Err(format!("dwell_ms must be in 1..={SCAN_DWELL_MAX_MS}"));
        }
        if let Some(a) = self.average
            && !(1..=SCAN_AVERAGE_MAX).contains(&a)
        {
            return Err(format!("average must be in 1..={SCAN_AVERAGE_MAX}"));
        }
        if !(0.0..0.5).contains(&self.edge_mask) {
            return Err("edge_mask must be in 0..0.5".into());
        }
        Ok(())
    }
}

fn rows_per_s_default() -> u32 {
    zeta_core::ops::SPECTRUM_ROWS_PER_S
}

/// The bins `[lo, hi)` a row of `size` bins of `bin_hz` masks: the notch, bins `k`
/// with `|(k - size/2) * bin_hz| <= notch_hz / 2` (bin `size/2` is DC, so with
/// `m = floor((notch_hz / 2) / bin_hz)` it is `[size/2 - m, size/2 + m + 1)`; none
/// for `notch_hz == 0`), and `floor(size * edge_mask)` bins at each edge. Clamped
/// into `0..size`, sorted and merged where they overlap or touch.
pub fn masked_ranges(size: usize, bin_hz: f64, notch_hz: u64, edge_mask: f64) -> Vec<[usize; 2]> {
    let mut ranges: Vec<[usize; 2]> = Vec::new();
    if notch_hz > 0 && bin_hz > 0.0 {
        let m = ((notch_hz as f64 / 2.0) / bin_hz).floor() as usize;
        let mid = size / 2;
        ranges.push([
            mid.saturating_sub(m),
            mid.saturating_add(m).saturating_add(1),
        ]);
    }
    let e = (size as f64 * edge_mask).floor() as usize;
    if e > 0 {
        ranges.push([0, e]);
        ranges.push([size.saturating_sub(e), size]);
    }
    let mut ranges: Vec<[usize; 2]> = ranges
        .into_iter()
        .map(|[lo, hi]| [lo.min(size), hi.min(size)])
        .filter(|[lo, hi]| lo < hi)
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<[usize; 2]> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r[0] <= last[1] => last[1] = last[1].max(r[1]),
            _ => merged.push(r),
        }
    }
    merged
}

/// Accepted `rows_per_s`.
const ROWS_PER_S: std::ops::RangeInclusive<u32> = 1..=60;

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
/// With `row_frames`, row `k` closes at sample `(k + 1) * row_frames * size` instead.
/// A change of rate or centre, or lost frames, restarts the stream: the partial row
/// is dropped and the next row starts at the next block's `t0`.
struct RowCutter {
    size: usize,
    window: Window,
    rows_per_s: u64,
    /// Chunks per row, when the params fix it (`FftParams::row_frames`).
    row_frames: Option<u64>,
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
    fn new(size: usize, window: Window, rows_per_s: u32, row_frames: Option<u32>) -> Self {
        Self {
            size,
            window,
            rows_per_s: rows_per_s as u64,
            row_frames: row_frames.map(u64::from),
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
        match self.row_frames {
            Some(n) => (self.row_index + 1) * n * self.size as u64,
            None => {
                zeta_core::ops::spectrum_row_boundary(self.row_index, self.rate_hz, self.rows_per_s)
            }
        }
    }

    /// Frames were lost: drop the carry and the partial row, and re-anchor the
    /// cadence so the next row starts at the next block's `t0` (lost samples are
    /// never counted, so continuing the old counters would drift).
    fn gap(&mut self) {
        self.restart(self.rate_hz, self.center_hz);
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
            // A retune or a new rate: the partial row (and carry) mixes two bands or
            // rates, so it is dropped and the cadence restarts at this block's `t0`.
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

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        serde_json::from_value::<FftParams>(params.clone())
            .map_err(|e| e.to_string())?
            .check()
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
        p.check()
            .map_err(|e| StageError::Failed(format!("fft params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let size = p.size as usize;
        let rows_per_s = p.rows_per_s;
        let backend = self.backend.clone();
        let mut cutter = RowCutter::new(size, p.window, rows_per_s, p.row_frames);
        let (notch_hz, edge_mask) = (p.notch_hz as u64, p.edge_mask);
        // The masked bins of the current rate, recomputed when a row's rate differs
        // from the last row's (the cutter restarts then anyway).
        let mut masked: Option<(u64, Vec<[usize; 2]>)> = None;
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
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
                    let bin_hz = row.rate_hz as f64 / size as f64;
                    if masked.as_ref().is_none_or(|(rate, _)| *rate != row.rate_hz) {
                        masked = Some((
                            row.rate_hz,
                            masked_ranges(size, bin_hz, notch_hz, edge_mask),
                        ));
                    }
                    let ranges = masked.as_ref().map_or(&[][..], |(_, r)| &r[..]);
                    let (floor_db, q) = quantise_row_masked(&row.power.db(), ranges);
                    let mut prov = frame.provenance.clone();
                    prov.span = Span {
                        start_ns: Timestamp(row.start_ns),
                        end_ns: Timestamp(row.end_ns),
                    };
                    let mut ev = serde_json::json!({
                        "size": size,
                        "bin_hz": bin_hz,
                        "rows_per_s": rows_per_s,
                        "center_hz": row.center_hz,
                        "floor_db": floor_db,
                        "step_db": STEP_DB,
                        "q": q,
                    });
                    if !ranges.is_empty() {
                        ev["masked"] = serde_json::json!(ranges);
                    }
                    Frame::new(out.clone(), prov, Payload::Event(ev))
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

    /// The first row of the stage run with `params` over one 4800-sample block of a
    /// 6 kHz tone at 48 kHz (one 50 ms row of 3 chunks closes in it), checking the
    /// row's span and that the rest of the block stays a partial row.
    async fn one_row(params: serde_json::Value) -> serde_json::Value {
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
                    params,
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
        ev.clone()
    }

    /// Every row the stage run with `params` emits over one block of `n` samples of a
    /// 6 kHz tone at 48 kHz, once the input closes.
    async fn rows_of(params: serde_json::Value, n: usize) -> Vec<Frame> {
        let prov = Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let bus = Arc::new(StreamBus::new(64));
        let out = bus.register(StreamKind::Event, prov.clone());
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(8);
        let o2 = out.clone();
        let h = tokio::spawn(async move {
            FftStage::new(Arc::new(CpuFft::new()))
                .run(
                    params,
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
                t0: Timestamp(0),
                sample_rate_hz: Hz(RATE),
                center_hz: Hz(100_000_000),
                samples: crate::util::nco(n, 6_000.0, RATE as f64).into(),
            }),
        ))
        .unwrap();
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let mut rows = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            rows.push(f);
        }
        rows
    }

    #[tokio::test]
    async fn row_frames_closes_a_row_on_that_many_frames() {
        // At 60 rows a second of 48 kHz a row is 800 samples, so three frames make three rows.
        let p = |row_frames: serde_json::Value| {
            let mut p = serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 60});
            p["row_frames"] = row_frames;
            p
        };
        assert_eq!(rows_of(p(serde_json::Value::Null), 3 * 1024).await.len(), 3);
        // With row_frames 3, the same three frames are one row, whatever rows_per_s says.
        let rows = rows_of(p(3.into()), 3 * 1024).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provenance.span.start_ns, Timestamp(0));
        assert_eq!(
            rows[0].provenance.span.end_ns,
            Timestamp(3072 * 1_000_000_000 / RATE)
        );
        let Payload::Event(ev) = &rows[0].payload else {
            panic!("expected an event")
        };
        assert_eq!(ev["rows_per_s"], 60);
        // Two frames are not a row of three.
        assert!(rows_of(p(3.into()), 2 * 1024).await.is_empty());
        // At least one frame.
        let stage = FftStage::new(Arc::new(CpuFft::new()));
        let e = stage.check_params(&p(0.into())).unwrap_err();
        assert!(e.contains("row_frames"), "{e}");
    }

    #[tokio::test]
    async fn fft_stage_emits_v2_rows() {
        let ev =
            one_row(serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20})).await;
        // 4800 samples at 48 kHz, 20 rows/s: one 50 ms row (3 chunks) closes in this block.
        assert_eq!(ev["q"].as_array().unwrap().len(), 1024);
        assert_eq!(ev["bin_hz"].as_f64(), Some(46.875));
        assert_eq!(ev["size"], 1024);
        assert_eq!(ev["rows_per_s"], 20);
        assert_eq!(ev["step_db"].as_f64(), Some(0.5));
        assert!(ev["floor_db"].as_f64().is_some());
        assert_eq!(ev["center_hz"], 100_000_000u64);
    }

    #[test]
    fn masked_ranges_of_a_notch_are_symmetric_about_the_dc_bin() {
        // 48 kHz over 1024 bins: 46.875 Hz each; 1875 Hz is 40 bins, so m = 20: 41 bins.
        assert_eq!(masked_ranges(1024, 46.875, 1875, 0.0), vec![[492, 533]]);
        assert_eq!(
            masked_ranges(1024, 46.875, 0, 0.0),
            Vec::<[usize; 2]>::new()
        );
    }

    #[test]
    fn masked_ranges_of_an_edge_mask() {
        assert_eq!(
            masked_ranges(1024, 46.875, 0, 0.1),
            vec![[0, 102], [922, 1024]]
        );
        assert_eq!(
            masked_ranges(1024, 46.875, 1875, 0.1),
            vec![[0, 102], [492, 533], [922, 1024]]
        );
    }

    #[test]
    fn masked_ranges_merge_and_clamp() {
        // 64 bins of 750 Hz; a 60 kHz notch is m = 40: [-8, 73) clamped to the row and merged with the edges.
        assert_eq!(masked_ranges(64, 750.0, 60_000, 0.1), vec![[0, 64]]);
        // A notch touching an edge range merges with it: m = 28 → [4, 61) plus [0, 6) and [58, 64).
        assert_eq!(masked_ranges(64, 750.0, 42_000, 0.1), vec![[0, 64]]);
    }

    #[test]
    fn quantise_row_masked_takes_the_floor_from_unmasked_bins() {
        let db: Vec<f32> = (0..100).map(|i| -100.0 + 2.0 * i as f32).collect();
        // The lower half masked (a hump that would otherwise set the floor): the floor is the
        // 2nd percentile of 0, 2, ..., 98 (index 0 → 0 dB) minus 5.
        let (floor, q) = quantise_row_masked(&db, &[[0, 50]]);
        assert_eq!(floor, -5.0);
        assert_eq!(q.len(), 100);
        assert_eq!(q[0], 0); // -100 dB, masked, still quantised (clamped at 0)
        assert_eq!(q[50], 10);
        assert_eq!(q[99], 206);
        assert_eq!(quantise_row_masked(&db, &[]), quantise_row(&db));
    }

    #[test]
    fn quantise_row_masked_with_everything_masked_falls_back() {
        let db: Vec<f32> = (0..100).map(|i| -100.0 + 2.0 * i as f32).collect();
        assert_eq!(quantise_row_masked(&db, &[[0, 100]]), quantise_row(&db)); // floor -103
    }

    #[test]
    fn fft_params_check_names_the_field() {
        let p = |notch_hz: i64, edge_mask: f64| FftParams {
            size: 1024,
            window: Window::Hann,
            rows_per_s: 20,
            notch_hz,
            edge_mask,
            row_frames: None,
            dwell_ms: None,
            average: None,
            bits: None,
        };
        assert_eq!(p(0, 0.0).check(), Ok(()));
        assert_eq!(p(300_000, 0.49).check(), Ok(()));
        assert_eq!(
            p(-1, 0.0).check(),
            Err("notch_hz must be at least 0".into())
        );
        assert_eq!(p(0, 0.5).check(), Err("edge_mask must be in 0..0.5".into()));
        assert_eq!(
            p(0, -0.1).check(),
            Err("edge_mask must be in 0..0.5".into())
        );
        let stage = FftStage::new(Arc::new(CpuFft::new()));
        let e = stage
            .check_params(&serde_json::json!({"size": 1024, "window": "hann", "edge_mask": 0.6}))
            .unwrap_err();
        assert!(e.contains("edge_mask"), "{e}");
        assert_eq!(
            stage.check_params(&serde_json::json!({"size": 1024, "window": "hann"})),
            Ok(())
        );
    }

    #[test]
    fn fft_params_check_bounds_the_scan_look() {
        let stage = FftStage::new(Arc::new(CpuFft::new()));
        let check = |field: &str, v: u64| {
            let mut p = serde_json::json!({"size": 1024, "window": "hann"});
            p[field] = v.into();
            stage.check_params(&p)
        };
        assert_eq!(check("dwell_ms", 1), Ok(()));
        assert_eq!(check("dwell_ms", 5000), Ok(()));
        assert_eq!(check("average", 64), Ok(()));
        assert_eq!(check("bits", 8), Ok(()));
        for (field, v) in [
            ("dwell_ms", 0),
            ("dwell_ms", 5001),
            ("average", 0),
            ("average", 65),
        ] {
            let e = check(field, v).unwrap_err();
            assert!(e.contains(field), "{field} {v}: {e}");
        }
    }

    #[test]
    fn fft_params_schema_lists_the_mask_params_with_defaults() {
        let s = serde_json::to_value(schemars::schema_for!(FftParams)).unwrap();
        assert_eq!(s["properties"]["notch_hz"]["default"], 0);
        assert_eq!(s["properties"]["edge_mask"]["default"], 0.0);
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !required.contains(&"notch_hz") && !required.contains(&"edge_mask"),
            "{required:?}"
        );
        // A graph with a bad edge_mask is refused by validate naming the field (as detect's is).
        let mut r = StageRegistry::new();
        r.register(Arc::new(FftStage::new(Arc::new(CpuFft::new()))));
        let g = Graph {
            id: GraphId::new(),
            nodes: vec![Node {
                id: NodeId("a".into()),
                kind: "fft".into(),
                params: serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20, "edge_mask": 0.6}),
            }],
            edges: vec![],
        };
        match validate(&g, &r) {
            Err(GraphError::BadParams { reason, .. }) => {
                assert!(reason.contains("edge_mask"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn fft_stage_rows_carry_masked_only_when_a_bin_is_masked() {
        let masked = one_row(serde_json::json!({
            "size": 1024, "window": "hann", "rows_per_s": 20,
            "notch_hz": 1875, "edge_mask": 0.1,
        }))
        .await;
        assert_eq!(
            masked["masked"],
            serde_json::json!([[0, 102], [492, 533], [922, 1024]])
        );
        assert_eq!(masked["q"].as_array().unwrap().len(), 1024);
        assert!(masked["floor_db"].as_f64().is_some_and(f64::is_finite));
        let plain =
            one_row(serde_json::json!({"size": 1024, "window": "hann", "rows_per_s": 20})).await;
        assert!(plain.get("masked").is_none(), "{plain}");
        for k in ["size", "bin_hz", "rows_per_s", "step_db", "center_hz"] {
            assert_eq!(plain[k], masked[k], "{k}");
        }
    }
}
