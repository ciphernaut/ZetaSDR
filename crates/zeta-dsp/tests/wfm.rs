//! Wideband FM demodulation: discriminator, de-emphasis, 15 kHz low-pass, 48 kHz audio;
//! with `stereo`, the pilot-locked L-R decode and its mono fallback.
#![cfg(feature = "test-fixtures")]

use std::f64::consts::PI;

use num_complex::Complex;
use zeta_dsp::fft::{Window, power_spectrum_db};
use zeta_dsp::fixtures::{
    STEREO_PILOT_LEVEL, awgn, stereo_mpx, tone, wfm_modulate, wfm_modulate_composite,
    wfm_stereo_modulate,
};
use zeta_dsp::wfm::{
    PILOT_SNR_OFF_DB, STEREO_OFF_DB, STEREO_ON_DB, WFM_AUDIO_TAPS, WfmDemod, WfmParams, deemphasis,
};

fn params(deviation_hz: u64) -> WfmParams {
    WfmParams {
        deviation_hz,
        audio_rate_hz: 48_000,
        deemphasis_us: 50,
        stereo: false,
    }
}

fn stereo() -> WfmParams {
    WfmParams {
        stereo: true,
        ..params(75_000)
    }
}

/// Demodulate `iq` at `rate_hz` in `block`-sample blocks with `stereo: true`: the
/// interleaved output, the demodulator (for its reading), and the reading's `stereo`
/// after each block.
fn demod_stereo(
    iq: &[Complex<f32>],
    rate_hz: u64,
    block: usize,
) -> (Vec<f32>, WfmDemod, Vec<bool>) {
    let mut d = WfmDemod::new(&stereo()).unwrap();
    let mut out = Vec::new();
    let mut states = Vec::new();
    for b in iq.chunks(block) {
        let y = d.process(b, rate_hz).unwrap();
        assert_eq!(y.len() % 2, 0, "a block's output is whole L, R pairs");
        out.extend(y);
        states.push(d.pilot().unwrap().stereo);
    }
    (out, d, states)
}

/// The left and right channels of interleaved `x`.
fn split(x: &[f32]) -> (Vec<f32>, Vec<f32>) {
    (
        x.iter().step_by(2).copied().collect(),
        x.iter().skip(1).step_by(2).copied().collect(),
    )
}

/// `20 log10(a / b)`.
fn db(a: f64, b: f64) -> f64 {
    20.0 * (a / b).log10()
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

/// FNV-1a over the bits of `x`: a checksum of exact output samples.
fn checksum(x: &[f32]) -> u64 {
    x.iter().fold(0xcbf2_9ce4_8422_2325, |h, s| {
        s.to_bits()
            .to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    })
}

#[test]
fn wfm_demod_mono_output_is_pinned() {
    // Checksums taken before stereo was added: `stereo: false` (the default) must give
    // these exact samples, on a mono station at the stage rate and on a stereo
    // multiplex at twice it (through the stage resampler).
    let audio = tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, true);
    let a = demod(&iq, 240_000, &params(75_000), 2_400);
    let l = tone(480_000, 240_000, 1_000.0, 0.5);
    let r = tone(480_000, 240_000, 3_000.0, 0.3);
    let iq = wfm_stereo_modulate(&l, &r, 480_000, 75_000.0, STEREO_PILOT_LEVEL);
    let b = demod(&iq, 480_000, &params(75_000), 9_600);
    eprintln!(
        "mono checksums {:#018x} ({}) {:#018x} ({})",
        checksum(&a),
        a.len(),
        checksum(&b),
        b.len()
    );
    assert_eq!((checksum(&a), a.len()), (0x1f1a_c7d8_8f9c_09c0, 48_000));
    assert_eq!((checksum(&b), b.len()), (0xf2ed_2c6a_6591_9644, 24_000));
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

/// The settled part of a 48 kHz channel: the 0.7 s ending 50 ms before its end (whole
/// cycles of 1, 2.5, 3 and 10 kHz). In a 2 s stereo run that is past the lock and the
/// pilot SNR's settling, which take up to a second.
fn settled(x: &[f32]) -> &[f32] {
    let end = x.len() - 2_400;
    &x[end - 33_600..end]
}

#[test]
fn stereo_keeps_a_left_only_tone_out_of_the_right_and_the_mirror() {
    // At the stage rate and at the 200 kHz the built-in expansion feeds the stage.
    for rate in [240_000u32, 200_000] {
        let one = tone(rate, 2 * rate as usize, 1_000.0, 0.5);
        let silent = vec![0.0; 2 * rate as usize];
        for (left, right, name) in [(&one, &silent, "left"), (&silent, &one, "right")] {
            let iq = wfm_stereo_modulate(left, right, rate, 75_000.0, STEREO_PILOT_LEVEL);
            let (out, d, _) = demod_stereo(&iq, rate as u64, rate as usize / 100);
            assert!(d.pilot().unwrap().stereo, "{rate} {name}: {:?}", d.pilot());
            let (l, r) = split(&out);
            assert!(l.len().abs_diff(96_000) <= 2, "len {}", l.len());
            let (l, r) = (settled(&l), settled(&r));
            let (on, off) = if name == "left" { (l, r) } else { (r, l) };
            let a_on = amplitude(on, 48_000.0, 1_000.0);
            let a_off = amplitude(off, 48_000.0, 1_000.0);
            let separation = db(a_on, a_off);
            eprintln!(
                "stereo at {rate} Hz, {name}-only 1 kHz: {separation:.1} dB separation, \
                 tone {a_on:.4}"
            );
            assert!(
                separation >= 30.0,
                "{rate} {name}: separation only {separation:.1} dB"
            );
            // The multiplex carries 0.9 of each channel (L+R and L-R at 0.45 each), so
            // the channel is 0.9 of the 0.5 tone, after 50 us de-emphasis at 1 kHz.
            let want = 0.45 / (1.0 + (2.0 * PI * 1_000.0 * 50e-6).powi(2)).sqrt();
            assert!(
                (a_on - want).abs() <= 0.1 * want,
                "{rate} {name}: tone {a_on} vs {want}"
            );
            let pilot = db(a_on, amplitude(on, 48_000.0, 19_000.0));
            assert!(
                pilot >= 20.0,
                "{rate} {name}: pilot only {pilot:.1} dB below the tone"
            );
        }
    }
}

#[test]
fn stereo_de_emphasises_each_channel() {
    // A 10 kHz tone in L and silence in R: 50 us takes 10 kHz down by 10.4 dB (0.303),
    // where an un-de-emphasised channel would keep the whole 0.9 of the 0.5 tone.
    let ten = tone(240_000, 480_000, 10_000.0, 0.5);
    let silent = vec![0.0; 480_000];
    let iq = wfm_stereo_modulate(&ten, &silent, 240_000, 75_000.0, STEREO_PILOT_LEVEL);
    let (out, _, _) = demod_stereo(&iq, 240_000, 2_400);
    let (l, r) = split(&out);
    let (l, r) = (settled(&l), settled(&r));
    let want = 0.45 / (1.0 + (2.0 * PI * 10_000.0 * 50e-6).powi(2)).sqrt();
    let a_l = amplitude(l, 48_000.0, 10_000.0);
    eprintln!("stereo 10 kHz in L: {a_l:.4}, want {want:.4}");
    assert!((a_l - want).abs() <= 0.1 * want, "L {a_l} vs {want}");
    let separation = db(a_l, amplitude(r, 48_000.0, 10_000.0));
    assert!(separation >= 30.0, "10 kHz separation {separation:.1} dB");
}

/// `iq` with noise added for a station SNR of `snr_db(t)` dB at `t` seconds (the noise
/// over the whole `rate_hz` capture, as `awgn` gives it).
fn with_noise(iq: &mut [Complex<f32>], rate_hz: u32, seed: u64, snr_db: impl Fn(f64) -> f64) {
    let mut noise = vec![Complex::new(0.0f32, 0.0); iq.len()];
    // On zeros `awgn` takes unit power, the FM signal's: this is 0 dB SNR.
    awgn(&mut noise, 0.0, seed);
    for (n, (s, w)) in iq.iter_mut().zip(&noise).enumerate() {
        let gain = 10f64.powf(-snr_db(n as f64 / rate_hz as f64) / 20.0) as f32;
        *s += w * gain;
    }
}

#[test]
fn a_weak_station_plays_mono_with_its_pilot_present_and_its_snr_low() {
    // 10 dB SNR over the capture: the pilot is there (pilot_db about -12) but the
    // stereo would be some 15 dB noisier than the mono.
    let left = tone(240_000, 240_000, 1_000.0, 0.5);
    let right = tone(240_000, 240_000, 3_000.0, 0.5);
    let mut iq = wfm_stereo_modulate(&left, &right, 240_000, 75_000.0, STEREO_PILOT_LEVEL);
    with_noise(&mut iq, 240_000, 9, |_| 10.0);
    let (out, d, states) = demod_stereo(&iq, 240_000, 2_400);
    let reading = d.pilot().unwrap();
    eprintln!("weak station: {reading:?}");
    assert!(states.iter().all(|s| !s), "{reading:?}");
    assert!(reading.pilot_db.unwrap() > STEREO_ON_DB, "{reading:?}");
    assert!(
        reading.pilot_snr_db.unwrap() < PILOT_SNR_OFF_DB,
        "{reading:?}"
    );
    let (l, r) = split(&out);
    assert_eq!(l, r);
}

#[test]
fn a_station_fading_and_returning_switches_once_each_way() {
    // 10 s: 30 dB SNR for 1 s, down to 5 dB over 4 s, back to 30 dB over 4 s, held
    // for 1 s. After the first lock, stereo turns off once on the way down and on once
    // on the way up.
    let fs = 240_000;
    let len = 10 * fs as usize;
    let left = tone(fs, len, 1_000.0, 0.4);
    let right = tone(fs, len, 2_500.0, 0.3);
    let mut iq = wfm_stereo_modulate(&left, &right, fs, 75_000.0, STEREO_PILOT_LEVEL);
    with_noise(&mut iq, fs, 13, |t| {
        let down = ((t - 1.0) / 4.0).min((9.0 - t) / 4.0).clamp(0.0, 1.0);
        30.0 - 25.0 * down
    });
    let (_, _, states) = demod_stereo(&iq, fs as u64, 2_400);
    let changes: Vec<(usize, bool)> = states
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] != w[1])
        .map(|(i, w)| (i + 1, w[1]))
        .collect();
    eprintln!("fade: stereo changes at 10 ms blocks {changes:?}");
    let to: Vec<bool> = changes.iter().map(|c| c.1).collect();
    assert_eq!(to, [true, false, true], "{changes:?}");
    assert!(
        changes[0].0 < 100,
        "stereo within the first second: {changes:?}"
    );
}

#[test]
fn a_loud_clean_station_still_decodes_stereo() {
    // The loudest a sine programme can be (L+R at full deviation, peak 0.98 with the
    // pilot) under a pilot at 8 %, the low end of the standard: the pilot reads about
    // -21 dB against the MPX, its SNR stays high.
    let full = tone(240_000, 240_000, 1_000.0, 1.0);
    let iq = wfm_stereo_modulate(&full, &full, 240_000, 75_000.0, 0.08);
    let (_, d, _) = demod_stereo(&iq, 240_000, 2_400);
    let reading = d.pilot().unwrap();
    eprintln!("loud station: {reading:?}");
    assert!(reading.stereo, "{reading:?}");
    assert!(reading.pilot_db.unwrap() < -20.0, "{reading:?}");
}

#[test]
fn stereo_puts_different_tones_in_their_own_channels() {
    let left = tone(240_000, 480_000, 1_000.0, 0.4);
    let right = tone(240_000, 480_000, 3_000.0, 0.4);
    let iq = wfm_stereo_modulate(&left, &right, 240_000, 75_000.0, STEREO_PILOT_LEVEL);
    let (out, _, _) = demod_stereo(&iq, 240_000, 2_400);
    let (l, r) = split(&out);
    let (l, r) = (settled(&l), settled(&r));
    let peak_hz = |x: &[f32]| {
        // 4800-point spectrum: 10 Hz bins, the positive half from bin 2400.
        let cx: Vec<Complex<f32>> = x.iter().map(|&s| Complex::new(s, 0.0)).collect();
        let db = power_spectrum_db(&cx, 4_800, Window::Hann);
        let (bin, _) = db
            .iter()
            .enumerate()
            .skip(2_400)
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        (bin - 2_400) * 10
    };
    assert_eq!((peak_hz(l), peak_hz(r)), (1_000, 3_000));
    let in_l = db(
        amplitude(l, 48_000.0, 1_000.0),
        amplitude(l, 48_000.0, 3_000.0),
    );
    let in_r = db(
        amplitude(r, 48_000.0, 3_000.0),
        amplitude(r, 48_000.0, 1_000.0),
    );
    eprintln!("stereo 1 kHz L, 3 kHz R: each {in_l:.1} / {in_r:.1} dB over the other's tone");
    assert!(
        in_l >= 30.0 && in_r >= 30.0,
        "L {in_l:.1} dB, R {in_r:.1} dB"
    );
}

#[test]
fn stereo_without_a_pilot_gives_two_identical_channels() {
    // A mono broadcast: L+R only, no pilot, nothing at 38 kHz.
    let audio = tone(48_000, 48_000, 1_000.0, 0.5);
    let iq = wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, false);
    let (out, d, states) = demod_stereo(&iq, 240_000, 2_400);
    let reading = d.pilot().unwrap();
    eprintln!("no pilot: {reading:?}");
    assert!(!reading.stereo && states.iter().all(|s| !s), "{reading:?}");
    assert!(reading.pilot_db.unwrap() < STEREO_OFF_DB, "{reading:?}");
    let (l, r) = split(&out);
    assert_eq!(l, r);
    // The mono audio `stereo: false` gives, at the same level.
    let mono = demod(&iq, 240_000, &params(75_000), 2_400);
    assert_eq!(l.len(), mono.len());
    let a = amplitude(settled(&l), 48_000.0, 1_000.0);
    let b = amplitude(settled(&mono), 48_000.0, 1_000.0);
    assert!((a - b).abs() <= 0.01 * b, "stereo's mono {a} vs mono {b}");
}

#[test]
fn stereo_switches_once_each_way_as_a_pilot_comes_and_goes() {
    // 10 s: no pilot for 0.5 s; then the pilot from 30 dB under the standard level up
    // to it over 4 s (7.5 dB/s), held for 1 s, back down over 4 s; gone for the last
    // 0.5 s. The programme's loudness steps by 4.4 dB every 0.2 s, as speech and music
    // do, so the reading (pilot against MPX) wanders that much as it crosses each
    // threshold: a single threshold would switch back and forth there. The signal is
    // strong (45 dB SNR), so the pilot's SNR is over its own threshold wherever the
    // presence crosses its thresholds, and it is the presence's hysteresis on test.
    let fs = 240_000;
    let len = 10 * fs as usize;
    let loudness = |n: usize| {
        if (n / (fs as usize / 5)).is_multiple_of(2) {
            1.0
        } else {
            0.6
        }
    };
    let left: Vec<f32> = tone(fs, len, 1_000.0, 0.4)
        .iter()
        .enumerate()
        .map(|(n, s)| s * loudness(n))
        .collect();
    let right: Vec<f32> = tone(fs, len, 2_500.0, 0.3)
        .iter()
        .enumerate()
        .map(|(n, s)| s * loudness(n))
        .collect();
    let mut mpx = stereo_mpx(&left, &right, fs, 0.0);
    let pilot = tone(fs, len, 19_000.0, STEREO_PILOT_LEVEL);
    let level = |t: f64| {
        if !(0.5..=9.5).contains(&t) {
            return 0.0;
        }
        let up = ((t - 0.5) / 4.0).min((9.5 - t) / 4.0).clamp(0.0, 1.0);
        10f64.powf((up - 1.0) * 30.0 / 20.0)
    };
    for (n, (m, p)) in mpx.iter_mut().zip(&pilot).enumerate() {
        *m += level(n as f64 / fs as f64) as f32 * p;
    }
    let mut iq = wfm_modulate_composite(&mpx, fs, 75_000.0);
    awgn(&mut iq, 45.0, 7);
    let (_, _, states) = demod_stereo(&iq, fs as u64, 2_400);
    let changes: Vec<(usize, bool)> = states
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] != w[1])
        .map(|(i, w)| (i + 1, w[1]))
        .collect();
    eprintln!("pilot ramp: stereo changes at 10 ms blocks {changes:?}");
    assert!(!states[0], "starts mono");
    let to: Vec<bool> = changes.iter().map(|c| c.1).collect();
    assert_eq!(to, [true, false], "{changes:?}");
}

#[test]
fn stereo_under_its_lowest_rate_gives_two_mono_channels_and_no_reading() {
    // At 96 kHz L-R (to 53 kHz) cannot be carried: still two channels, both L+R.
    let left = tone(96_000, 96_000, 1_000.0, 0.4);
    let iq = wfm_stereo_modulate(&left, &left, 96_000, 75_000.0, STEREO_PILOT_LEVEL);
    let (out, d, states) = demod_stereo(&iq, 96_000, 960);
    assert_eq!(d.pilot().unwrap().pilot_db, None);
    assert!(states.iter().all(|s| !s));
    let (l, r) = split(&out);
    assert!(l.len().abs_diff(48_000) <= 2, "len {}", l.len());
    assert_eq!(l, r);
}

#[test]
fn stereo_false_is_one_channel_with_no_reading() {
    let audio = tone(48_000, 4_800, 1_000.0, 0.5);
    let iq = wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, true);
    let mut d = WfmDemod::new(&params(75_000)).unwrap();
    let y = d.process(&iq, 240_000).unwrap();
    assert_eq!(d.channels(), 1);
    assert!(d.pilot().is_none());
    assert!(y.len().abs_diff(4_800) <= 2, "len {}", y.len());
    // The param is optional and off by default.
    let p: WfmParams = serde_json::from_value(
        serde_json::json!({"deviation_hz": 75_000, "audio_rate_hz": 48_000, "deemphasis_us": 50}),
    )
    .unwrap();
    assert!(!p.stereo);
}

#[test]
fn stereo_output_is_two_channels_at_the_audio_rate() {
    // Each block gives two samples for every one mono gives, at the stage rate and
    // through the resampler.
    for (rate, block) in [
        (240_000u32, 2_400usize),
        (480_000, 9_600),
        (250_000, 2_500),
        (200_000, 2_000),
    ] {
        let left = tone(rate, rate as usize, 1_000.0, 0.4);
        let iq = wfm_stereo_modulate(&left, &left, rate, 75_000.0, STEREO_PILOT_LEVEL);
        let mut d = WfmDemod::new(&stereo()).unwrap();
        let mut m = WfmDemod::new(&params(75_000)).unwrap();
        let (mut n_stereo, mut n_mono) = (0, 0);
        for b in iq.chunks(block) {
            let y = d.process(b, rate as u64).unwrap();
            let z = m.process(b, rate as u64).unwrap();
            assert_eq!(y.len(), 2 * z.len(), "{rate} Hz: a block's pairs");
            n_stereo += y.len();
            n_mono += z.len();
        }
        assert_eq!(d.channels(), 2);
        assert_eq!(n_stereo, 2 * n_mono);
        assert!(
            n_mono.abs_diff(48_000) <= 2,
            "{rate} Hz: {n_mono} a channel"
        );
    }
}
