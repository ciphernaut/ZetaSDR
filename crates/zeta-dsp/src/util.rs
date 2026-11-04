//! Small DSP helpers shared by stages and fixtures.

use std::f64::consts::PI;

use num_complex::Complex;
use zeta_core::stage::StageContext;

/// A decoder stage's event payload: `fields` in the job's mode-row envelope (`protocol`,
/// `summary`, `t_ns`) when the context has the row, else `fields` with `protocol` alone.
pub fn event(
    ctx: &StageContext,
    start_ns: u64,
    protocol: &str,
    mut fields: serde_json::Value,
) -> serde_json::Value {
    match &ctx.mode {
        Some(row) => zeta_core::modes::summary::envelope(row, start_ns, fields),
        None => {
            fields["protocol"] = protocol.into();
            fields
        }
    }
}

/// `len` samples of the unit-magnitude complex exponential `exp(j 2 pi f n / rate)`.
/// `freq_hz` may be negative.
pub fn nco(len: usize, freq_hz: f64, rate_hz: f64) -> Vec<Complex<f32>> {
    let step = freq_hz / rate_hz;
    (0..len)
        .map(|n| {
            // Reduce to one cycle before scaling to radians to keep precision.
            let (s, c) = (2.0 * PI * (n as f64 * step).fract()).sin_cos();
            Complex::new(c as f32, s as f32)
        })
        .collect()
}

/// Symmetric Hann window of `n` points (zero at both ends; `[1.0]` for `n == 1`).
pub fn hann(n: usize) -> Vec<f32> {
    if n == 1 {
        return vec![1.0];
    }
    let m = (n.saturating_sub(1)) as f64;
    (0..n)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / m).cos()) as f32)
        .collect()
}

/// Hann-windowed sinc low-pass FIR with cutoff `cutoff_hz` at `rate_hz`,
/// normalised to unity DC gain. Panics unless `taps` is odd.
pub fn lowpass_taps(cutoff_hz: f64, rate_hz: f64, taps: usize) -> Vec<f32> {
    assert!(taps % 2 == 1, "lowpass_taps: taps must be odd, got {taps}");
    let fc = cutoff_hz / rate_hz;
    let mid = (taps / 2) as f64;
    let w = hann(taps);
    let h: Vec<f64> = (0..taps)
        .map(|i| {
            let t = i as f64 - mid;
            let sinc = if t == 0.0 {
                2.0 * fc
            } else {
                (2.0 * PI * fc * t).sin() / (PI * t)
            };
            sinc * w[i] as f64
        })
        .collect();
    let sum: f64 = h.iter().sum();
    h.iter().map(|v| (v / sum) as f32).collect()
}

/// Causal FIR filter then keep every `decim`-th output, starting with the first.
/// The filter starts from zero history, so output `k` is the filter output at
/// input index `k * decim`; the result has `x.len().div_ceil(decim)` samples.
pub fn fir_decimate(x: &[Complex<f32>], taps: &[f32], decim: usize) -> Vec<Complex<f32>> {
    assert!(decim > 0, "fir_decimate: decim must be > 0");
    (0..x.len())
        .step_by(decim)
        .map(|n| {
            taps.iter()
                .enumerate()
                .take(n + 1)
                .fold(Complex::new(0.0, 0.0), |acc, (j, &t)| acc + x[n - j] * t)
        })
        .collect()
}

/// Real-valued [`fir_decimate`]: causal FIR from zero history, keeping every
/// `decim`-th output starting with the first (`x.len().div_ceil(decim)` samples).
pub fn fir_decimate_real(x: &[f32], taps: &[f32], decim: usize) -> Vec<f32> {
    assert!(decim > 0, "fir_decimate_real: decim must be > 0");
    (0..x.len())
        .step_by(decim)
        .map(|n| {
            taps.iter()
                .enumerate()
                .take(n + 1)
                .fold(0.0, |acc, (j, &t)| acc + x[n - j] * t)
        })
        .collect()
}

/// Causal real FIR at decimation 1 that carries its last `taps.len() - 1`
/// inputs across calls, so filtering a signal in blocks equals filtering it
/// whole (history starts at zero).
#[derive(Clone, Debug)]
pub struct StreamingFir {
    taps: Vec<f32>,
    history: Vec<f32>,
}

impl StreamingFir {
    /// A filter with `taps` (at least one) and zero history.
    pub fn new(taps: Vec<f32>) -> Self {
        assert!(!taps.is_empty(), "StreamingFir: no taps");
        let history = vec![0.0; taps.len() - 1];
        Self { taps, history }
    }

    /// Forget the history (back to zero), as at construction.
    pub fn reset(&mut self) {
        self.history.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Filter `x`, one output per input.
    pub fn process(&mut self, x: &[f32]) -> Vec<f32> {
        let h = self.history.len();
        let mut buf = std::mem::take(&mut self.history);
        buf.extend_from_slice(x);
        let out = (0..x.len())
            .map(|n| {
                // buf[h + n] is x[n]; tap j multiplies x[n - j].
                self.taps
                    .iter()
                    .enumerate()
                    .fold(0.0, |acc, (j, &t)| acc + buf[h + n - j] * t)
            })
            .collect();
        self.history = buf.split_off(buf.len() - h);
        out
    }
}

/// Complex-input [`StreamingFir`]: real taps, complex samples, history
/// carried across calls so block-wise filtering equals filtering whole.
#[derive(Clone, Debug)]
pub struct StreamingComplexFir {
    taps: Vec<f32>,
    history: Vec<Complex<f32>>,
}

impl StreamingComplexFir {
    /// A filter with `taps` (at least one) and zero history.
    pub fn new(taps: Vec<f32>) -> Self {
        assert!(!taps.is_empty(), "StreamingComplexFir: no taps");
        let history = vec![Complex::new(0.0, 0.0); taps.len() - 1];
        Self { taps, history }
    }

    /// Filter `x`, one output per input.
    pub fn process(&mut self, x: &[Complex<f32>]) -> Vec<Complex<f32>> {
        let h = self.history.len();
        let mut buf = std::mem::take(&mut self.history);
        buf.extend_from_slice(x);
        let out = (0..x.len())
            .map(|n| {
                // buf[h + n] is x[n]; tap j multiplies x[n - j].
                self.taps
                    .iter()
                    .enumerate()
                    .fold(Complex::new(0.0, 0.0), |acc, (j, &t)| {
                        acc + buf[h + n - j] * t
                    })
            })
            .collect();
        self.history = buf.split_off(buf.len() - h);
        out
    }
}

/// Mean power of `x` in dB (`10 log10(mean |x|^2)`); negative infinity when
/// `x` is empty or all zero.
pub fn power_db(x: &[Complex<f32>]) -> f32 {
    if x.is_empty() {
        return f32::NEG_INFINITY;
    }
    let p: f64 = x.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / x.len() as f64;
    (10.0 * p.log10()) as f32
}

/// CRC-16/X-25 (the AX.25 FCS): init 0xFFFF, reflected poly 0x8408, final XOR 0xFFFF.
pub fn crc16_x25(data: &[u8]) -> u16 {
    crate::crc::crc(&crate::crc::CRC16_X25, data) as u16
}
