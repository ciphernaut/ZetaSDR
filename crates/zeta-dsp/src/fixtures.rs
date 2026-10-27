//! Test-only fixture helpers, behind feature `test-fixtures`: SigMF writing, speech-like
//! audio and real noise. Every generator of [`crate::synth`] is re-exported here, so
//! `zeta_dsp::fixtures::aprs_packet_iq` and friends keep working.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use num_complex::Complex;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::types::{Hz, Timestamp};

pub use crate::synth::*;

/// Speech-like test audio: 300 ms bursts of a 3-tone complex (220, 440 and
/// 1 320 Hz, equal weight) with a 4 Hz amplitude tremolo, separated by 300 ms of
/// silence, starting with a burst; peak amplitude 0.5.
pub fn speech_like(rate_hz: u32, seconds: f32) -> Vec<f32> {
    let len = (rate_hz as f64 * seconds as f64) as usize;
    let period = rate_hz as usize * 600 / 1000;
    let burst = rate_hz as usize * 300 / 1000;
    (0..len)
        .map(|n| {
            if n % period >= burst {
                return 0.0;
            }
            let t = n as f64 / rate_hz as f64;
            let tones: f64 = [220.0, 440.0, 1320.0]
                .iter()
                .map(|f| (2.0 * PI * f * t).sin())
                .sum::<f64>()
                / 3.0;
            let tremolo = 0.75 + 0.25 * (2.0 * PI * 4.0 * t).sin();
            (0.5 * tremolo * tones) as f32
        })
        .collect()
}

/// Write `iq` as a cf32_le SigMF pair `<stem>.sigmf-{data,meta}` in `dir`
/// (start time 0, no annotations); returns the meta path. Panics on I/O error.
pub fn write_fixture(
    dir: &Path,
    stem: &str,
    iq: &[Complex<f32>],
    rate_hz: u32,
    center_hz: u64,
) -> PathBuf {
    let meta = CaptureMeta {
        sample_rate_hz: Hz(rate_hz as u64),
        center_hz: Hz(center_hz),
        start: Timestamp(0),
        annotations: Vec::new(),
    };
    write_cf32(dir, stem, &meta, iq).expect("write fixture")
}

/// Add real white Gaussian noise at `snr_db` below the mean power of real `x`
/// (taken as 1.0 if zero). Same deterministic LCG plus Box-Muller as [`awgn`].
pub fn awgn_real(x: &mut [f32], snr_db: f32, seed: u64) {
    if x.is_empty() {
        return;
    }
    let p: f64 = x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len() as f64;
    let p = if p > 0.0 { p } else { 1.0 };
    let sigma = (p / 10f64.powf(snr_db as f64 / 10.0)).sqrt();
    let mut state = seed;
    let mut uniform = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 11) + 1) as f64 / (1u64 << 53) as f64
    };
    for s in x.iter_mut() {
        let (u1, u2) = (uniform(), uniform());
        *s += (sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()) as f32;
    }
}
