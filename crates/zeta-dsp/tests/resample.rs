//! Streaming rational resampler.

use num_complex::Complex;
use zeta_dsp::fft::{Window, power_spectrum_db};
use zeta_dsp::resample::Resampler;
use zeta_dsp::util::{fir_decimate_real, lowpass_taps};

fn tone(rate_hz: f64, n: usize, freq_hz: f64, amp: f32) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let ph = (i as f64 * freq_hz / rate_hz).fract();
            amp * (2.0 * std::f64::consts::PI * ph).sin() as f32
        })
        .collect()
}

#[test]
fn resampler_keeps_a_tone_clean_across_blocks_at_200k_to_48k() {
    let x = tone(200_000.0, 400_000, 1_000.0, 1.0);
    let mut r = Resampler::new(200_000, 48_000).unwrap();
    assert_eq!(r.ratio(), (6, 25));
    let mut y = Vec::new();
    for block in x.chunks(1_000) {
        y.extend(r.process(block));
    }
    assert!(y.len().abs_diff(96_000) <= 2, "len {}", y.len());
    assert!(y.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    // 4800-point spectrum at 48 kHz: 10 Hz bins, 1 kHz exactly on bin 100.
    let size = 4_800;
    let tail: Vec<Complex<f32>> = y[2_000..].iter().map(|&s| Complex::new(s, 0.0)).collect();
    let db = power_spectrum_db(&tail, size, Window::Hann);
    let half = size / 2;
    let (peak_bin, &peak) = db
        .iter()
        .enumerate()
        .skip(half)
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap();
    assert_eq!(peak_bin, half + 100, "peak at bin {peak_bin}");
    // A real tone also sits at -1 kHz; the Hann main lobe spans ±1 bin.
    let near = |k: usize| k.abs_diff(half + 100) <= 1 || k.abs_diff(half - 100) <= 1;
    let worst = db
        .iter()
        .enumerate()
        .filter(|&(k, _)| !near(k))
        .map(|(_, &v)| v)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(peak - worst >= 50.0, "spur only {} dB down", peak - worst);
}

#[test]
fn resampler_handles_integer_and_identity_ratios() {
    let x = tone(96_000.0, 4_801, 1_000.0, 1.0);
    let mut r = Resampler::new(96_000, 48_000).unwrap();
    assert_eq!(r.ratio(), (1, 2));
    let mut y = Vec::new();
    for chunk in [&x[..1001], &x[1001..1002], &x[1002..]] {
        y.extend(r.process(chunk));
    }
    // L = 1: polyphase reduces to a plain FIR-and-decimate with 8 × 2 + 1 taps.
    let want = fir_decimate_real(&x, &lowpass_taps(21_600.0, 96_000.0, 17), 2);
    assert_eq!(y.len(), want.len());
    assert_eq!(y.len(), 2_401);
    for (a, b) in y.iter().zip(&want) {
        assert!((a - b).abs() < 1e-5, "{a} vs {b}");
    }

    let mut id = Resampler::new(48_000, 48_000).unwrap();
    assert_eq!(id.ratio(), (1, 1));
    assert_eq!(id.process(&x[..1000]), x[..1000].to_vec());
    assert_eq!(id.process(&x[1000..1001]), x[1000..1001].to_vec());
}

#[test]
fn resampler_upsamples_with_unity_gain() {
    // 12 kHz to 48 kHz (L = 4): a 1 kHz tone keeps its amplitude.
    let x = tone(12_000.0, 12_000, 1_000.0, 0.5);
    let mut r = Resampler::new(12_000, 48_000).unwrap();
    assert_eq!(r.ratio(), (4, 1));
    let y: Vec<f32> = x.chunks(333).flat_map(|c| r.process(c)).collect();
    assert_eq!(y.len(), 48_000);
    let peak = y[4_800..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
}

#[test]
fn resampler_rejects_zero_rates() {
    assert!(Resampler::new(0, 48_000).is_err());
    assert!(Resampler::new(48_000, 0).is_err());
    assert!(Resampler::new(0, 0).is_err());
}

#[test]
fn resampler_rejects_ratios_beyond_the_limit() {
    // 12 007 Hz is prime: 48 000 / 12 007 is within the limit, 999 983 Hz (prime) is not.
    assert_eq!(
        Resampler::new(12_007, 48_000).unwrap().ratio(),
        (48_000, 12_007)
    );
    let e = Resampler::new(999_983, 48_000).unwrap_err();
    assert_eq!(
        e,
        "resampler 999983 -> 48000 Hz reduces to 48000/999983, beyond the 100000 limit"
    );
}
