//! Wideband FM demodulation: discriminator, de-emphasis, 15 kHz low-pass, 48 kHz audio.
#![cfg(feature = "test-fixtures")]

use std::f64::consts::PI;

use num_complex::Complex;
use zeta_dsp::fft::{Window, power_spectrum_db};
use zeta_dsp::fixtures::{tone, wfm_modulate};
use zeta_dsp::wfm::{WFM_AUDIO_TAPS, WfmDemod, WfmParams, deemphasis};

fn params(deviation_hz: u64) -> WfmParams {
    WfmParams {
        deviation_hz,
        audio_rate_hz: 48_000,
        deemphasis_us: 50,
    }
}

/// Demodulate `iq` at `rate_hz` in `block`-sample blocks.
fn demod(iq: &[Complex<f32>], rate_hz: u64, p: &WfmParams, block: usize) -> Vec<f32> {
    let mut d = WfmDemod::new(p).unwrap();
    let mut out = Vec::new();
    for b in iq.chunks(block) {
        out.extend(d.process(b, rate_hz).unwrap());
    }
    out
}

/// Amplitude of the `freq_hz` component of `x` (single-bin DFT; `x` should hold
/// a whole number of cycles).
fn amplitude(x: &[f32], rate_hz: f64, freq_hz: f64) -> f64 {
    let (re, im) = x.iter().enumerate().fold((0.0, 0.0), |(re, im), (i, &v)| {
        let ph = 2.0 * PI * (i as f64 * freq_hz / rate_hz).fract();
        (re + v as f64 * ph.cos(), im - v as f64 * ph.sin())
    });
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

#[test]
fn wfm_demod_recovers_a_1khz_tone_at_48k() {
    let audio = tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, true);
    assert_eq!(iq.len(), 240_000);
    let out = demod(&iq, 240_000, &params(75_000), 2_400);
    assert!(out.len().abs_diff(48_000) <= 2, "len {}", out.len());
    let tail = &out[4_800..4_800 + 43_200];

    // 4800-point spectrum: 10 Hz bins, 1 kHz on bin 100 above centre.
    let cx: Vec<Complex<f32>> = tail.iter().map(|&s| Complex::new(s, 0.0)).collect();
    let db = power_spectrum_db(&cx, 4_800, Window::Hann);
    let (peak_bin, _) = db
        .iter()
        .enumerate()
        .skip(2_400)
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap();
    assert_eq!(peak_bin, 2_400 + 100, "peak at bin {peak_bin}");

    // Discriminator gives 0.5 (37.5 kHz of 75 kHz); 50 us de-emphasis at 1 kHz
    // scales by 1/sqrt(1 + (2 pi 1000 tau)^2).
    let tau = 50e-6;
    let want = 0.5 / (1.0 + (2.0 * PI * 1_000.0 * tau).powi(2)).sqrt() / 2f64.sqrt();
    let rms = (tail.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
    eprintln!("wfm tone rms {rms:.4}, expected {want:.4}");
    assert!((rms - want).abs() <= 0.2 * want, "rms {rms} vs {want}");

    let a_tone = amplitude(tail, 48_000.0, 1_000.0);
    let a_pilot = amplitude(tail, 48_000.0, 19_000.0);
    let rejection = 20.0 * (a_tone / a_pilot).log10();
    eprintln!("wfm pilot {rejection:.1} dB below the tone");
    assert!(rejection >= 20.0, "pilot only {rejection} dB down");
}

#[test]
fn wfm_demod_clamps_over_deviation() {
    let audio = tone(48_000, 24_000, 1_000.0, 1.0);
    let iq = wfm_modulate(&audio, 48_000, 240_000, 150_000.0, 0.0, false);
    let out = demod(&iq, 240_000, &params(75_000), 2_400);
    assert!(!out.is_empty());
    assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
}

#[test]
fn wfm_demod_rejects_bad_params() {
    let bad = |p: WfmParams| WfmDemod::new(&p).err().unwrap();
    assert!(bad(params(0)).contains("deviation_hz"));
    assert!(
        bad(WfmParams {
            deemphasis_us: 60,
            ..params(75_000)
        })
        .contains("deemphasis_us")
    );
    assert!(
        bad(WfmParams {
            audio_rate_hz: 0,
            ..params(75_000)
        })
        .contains("audio_rate_hz")
    );
    for us in [50, 75] {
        assert!(
            WfmDemod::new(&WfmParams {
                deemphasis_us: us,
                ..params(75_000)
            })
            .is_ok()
        );
    }
}

#[test]
fn deemphasis_is_a_one_pole_low_pass_carrying_state() {
    // Step response: y[n] = 1 - (1 - a)^(n+1), a = 1 - exp(-1/(rate tau)).
    let a = 1.0 - (-1.0f64 / (48_000.0 * 75e-6)).exp();
    let mut x = vec![1.0f32; 64];
    let mut state = 0.0;
    deemphasis(&mut x[..10], 48_000, 75, &mut state);
    deemphasis(&mut x[10..], 48_000, 75, &mut state);
    for (n, &y) in x.iter().enumerate() {
        let want = 1.0 - (1.0 - a).powi(n as i32 + 1);
        assert!((y as f64 - want).abs() < 1e-5, "{n}: {y} vs {want}");
    }
    assert_eq!(state, x[63]);
}

#[test]
fn streaming_fir_in_blocks_matches_one_shot() {
    use zeta_dsp::util::{StreamingFir, fir_decimate_real, lowpass_taps};
    let x = tone(240_000, 5_001, 3_000.0, 1.0);
    let taps = lowpass_taps(15_000.0, 240_000.0, 127);
    let whole = fir_decimate_real(&x, &taps, 1);
    let mut f = StreamingFir::new(taps);
    let mut parts = Vec::new();
    for chunk in [&x[..50], &x[50..51], &x[51..2_000], &x[2_000..]] {
        parts.extend(f.process(chunk));
    }
    assert_eq!(parts.len(), whole.len());
    for (a, b) in parts.iter().zip(&whole) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }
}

#[test]
fn wfm_demod_rejects_the_pilot_at_960k() {
    // At 960 kHz a fixed 127-tap low-pass has a ~15 kHz transition band and
    // passes the 19 kHz pilot; the taps scale with the rate. The tone is as
    // loud as the pilot (0.1), so de-emphasis alone (~15 dB) is not enough.
    let audio = tone(48_000, 48_000, 1_000.0, 0.1);
    let iq = wfm_modulate(&audio, 48_000, 960_000, 75_000.0, 0.0, true);
    let out = demod(&iq, 960_000, &params(75_000), 9_600);
    assert!(out.len().abs_diff(48_000) <= 2, "len {}", out.len());
    let tail = &out[4_800..4_800 + 43_200];
    let a_tone = amplitude(tail, 48_000.0, 1_000.0);
    let a_pilot = amplitude(tail, 48_000.0, 19_000.0);
    let rejection = 20.0 * (a_tone / a_pilot).log10();
    eprintln!("wfm at 960 kHz: pilot {rejection:.1} dB below the tone");
    assert!(rejection >= 40.0, "pilot only {rejection} dB down");
}

#[test]
fn wfm_demod_at_2_msps_bounds_its_filter() {
    // At 2 Msps the old rule would have built a 1001-tap low-pass at the
    // input rate; resampling to 240 kHz first keeps it at the 240 kHz count.
    let audio = tone(48_000, 48_000, 1_000.0, 0.1);
    let iq = wfm_modulate(&audio, 48_000, 2_000_000, 75_000.0, 0.0, true);
    let mut d = WfmDemod::new(&params(75_000)).unwrap();
    let mut out = Vec::new();
    for b in iq.chunks(20_000) {
        out.extend(d.process(b, 2_000_000).unwrap());
    }
    let taps = d.audio_lpf_taps();
    eprintln!("wfm at 2 Msps: audio low-pass {taps} taps");
    assert!(taps > 0 && taps <= 1_025, "taps {taps}");
    // The old rule's 1001 taps would also pass 1025; the 240 kHz count would not.
    assert_eq!(taps, WFM_AUDIO_TAPS, "taps {taps}");
    assert!(out.len().abs_diff(48_000) <= 2, "len {}", out.len());
    let tail = &out[4_800..4_800 + 43_200];
    let a_tone = amplitude(tail, 48_000.0, 1_000.0);
    let a_pilot = amplitude(tail, 48_000.0, 19_000.0);
    let rejection = 20.0 * (a_tone / a_pilot).log10();
    eprintln!("wfm at 2 Msps: pilot {rejection:.1} dB below the tone");
    assert!(rejection >= 40.0, "pilot only {rejection} dB down");
}
