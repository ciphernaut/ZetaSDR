//! Small DSP helpers shared by stages and fixtures.

use std::f64::consts::PI;

use num_complex::Complex;

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
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
