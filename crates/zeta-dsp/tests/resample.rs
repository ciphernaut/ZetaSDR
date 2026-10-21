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
    // L = 1: polyphase reduces to a plain FIR-and-decimate; 8 × 2 + 1 = 17
    // taps is raised to the 63-tap floor.
    let want = fir_decimate_real(&x, &lowpass_taps(21_600.0, 96_000.0, 63), 2);
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
fn resampler_attenuates_30_khz_by_40_db_at_96k_to_48k() {
    // 30 kHz is above the 24 kHz output Nyquist and would alias to 18 kHz.
    let rms_out = |freq_hz: f64| {
        let x = tone(96_000.0, 96_000, freq_hz, 1.0);
        let mut r = Resampler::new(96_000, 48_000).unwrap();
        let y: Vec<f32> = x.chunks(1_000).flat_map(|c| r.process(c)).collect();
        let tail = &y[1_000..];
        (tail.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt()
    };
    let (pass, stop) = (rms_out(1_000.0), rms_out(30_000.0));
    let db = 20.0 * (pass / stop).log10();
    eprintln!("96k->48k: 30 kHz down {db:.1} dB relative to 1 kHz");
    assert!(db >= 40.0, "30 kHz only {db:.1} dB down");
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
fn exact_ratios_below_the_factor_are_unchanged() {
    for (i, o, ratio) in [
        (96_000, 48_000, (1, 2)),
        (200_000, 48_000, (6, 25)),
        (2_048_000, 2_400_000, (75, 64)),
        (204_800, 48_000, (15, 64)),
    ] {
        let r = Resampler::new(i, o).unwrap();
        assert_eq!(r.ratio(), ratio, "{i} -> {o}");
        assert_eq!(r.input_rate_hz(), i, "{i} -> {o}");
    }
}

#[test]
fn the_live_rates_snap_within_the_bound() {
    for (i, o, snapped, ratio) in [
        (398_961u64, 48_000u64, 398_976u64, (125u64, 1039u64)),
        (260_339, 48_000, 260_352, (125, 678)),
        (398_961, 240_000, 398_976, (625, 1039)),
    ] {
        let r = Resampler::new(i, o).unwrap();
        assert_eq!(r.input_rate_hz(), snapped, "{i} -> {o}");
        assert_eq!(r.ratio(), ratio, "{i} -> {o}");
        let ppm = i.abs_diff(snapped) as f64 * 1e6 / i as f64;
        assert!(ppm < 300.0, "{i} -> {o}: {ppm} ppm");
    }
}

#[test]
fn a_snapped_resampler_streams_like_the_exact_one_at_the_snapped_rate() {
    let x = tone(398_976.0, 398_976, 1_000.0, 0.5);
    let a = Resampler::new(398_961, 48_000).unwrap().process(&x);
    let b = Resampler::new(398_976, 48_000).unwrap().process(&x);
    assert!(!a.is_empty());
    assert_eq!(a, b);
}

#[test]
fn zero_rates_still_error() {
    for (i, o) in [(0, 48_000), (48_000, 0)] {
        let e = Resampler::new(i, o).unwrap_err();
        assert!(e.contains("resampler rates must be >= 1"), "{e}");
    }
}

#[test]
fn a_rate_with_no_snap_in_reach_still_errors() {
    // 6857 divides 47 999; no multiple of it lies within 999 Hz of 999 983.
    let e = Resampler::new(999_983, 47_999).unwrap_err();
    assert_eq!(
        e,
        "resampler 999983 -> 47999 Hz reduces to 47999/999983, beyond the 2048 limit and no rate within 1000 ppm snaps"
    );
}
