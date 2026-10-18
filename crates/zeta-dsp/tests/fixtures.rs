#![cfg(feature = "test-fixtures")]

use num_complex::Complex;
use zeta_dsp::crc16_x25;
use zeta_dsp::fixtures::*;
use zeta_dsp::util::*;

/// Naive DFT power spectrum (bin k -> |X_k|^2).
fn dft_power(x: &[Complex<f32>]) -> Vec<f64> {
    let n = x.len();
    (0..n)
        .map(|k| {
            let mut acc = Complex::new(0.0f64, 0.0);
            for (i, s) in x.iter().enumerate() {
                let ph = -2.0 * std::f64::consts::PI * (k * i % n) as f64 / n as f64;
                acc += Complex::new(s.re as f64, s.im as f64) * Complex::from_polar(1.0, ph);
            }
            acc.norm_sqr()
        })
        .collect()
}

#[test]
fn crc16_x25_known_vector() {
    assert_eq!(crc16_x25(b"123456789"), 0x906E);
}

#[test]
fn ax25_frame_fcs_verifies() {
    let frame = ax25_ui_frame("VK3ABC", "APRS", &["WIDE1-1"], b">hi");
    let n = frame.len();
    // 3 addresses * 7 + control + pid + info + fcs
    assert_eq!(n, 21 + 2 + 3 + 2);
    assert_eq!(
        crc16_x25(&frame[..n - 2]),
        u16::from_le_bytes([frame[n - 2], frame[n - 1]])
    );
    // Destination first, shifted left one bit.
    assert_eq!(frame[0], b'A' << 1);
    assert_eq!(frame[4], b' ' << 1);
    assert_eq!(frame[6], 0x60);
    // Source next, SSID 0, not last.
    assert_eq!(frame[7], b'V' << 1);
    assert_eq!(frame[13], 0x60);
    // Digipeater WIDE1-1: SSID 1, last address bit set, H clear.
    assert_eq!(frame[14], b'W' << 1);
    assert_eq!(frame[19], b' ' << 1);
    assert_eq!(frame[20], 0x60 | (1 << 1) | 1);
    assert_eq!(&frame[21..23], &[0x03, 0xF0]);
    assert_eq!(&frame[23..26], b">hi");
}

#[test]
fn hdlc_stuffs_after_five_ones() {
    let bits = hdlc_frame_bits(&[0xFF], 0, 0);
    // 8 ones become 1 1 1 1 1 0 1 1 1
    assert_eq!(bits, vec![1, 1, 1, 1, 1, 0, 1, 1, 1]);
    assert!(!bits.windows(6).any(|w| w.iter().all(|&b| b == 1)));

    let framed = hdlc_frame_bits(&[0xFF, 0xFF], 1, 1);
    let flag = [0u8, 1, 1, 1, 1, 1, 1, 0];
    assert_eq!(&framed[..8], &flag);
    assert_eq!(&framed[framed.len() - 8..], &flag);
    let body = &framed[8..framed.len() - 8];
    assert!(!body.windows(6).any(|w| w.iter().all(|&b| b == 1)));
    // LSB first: 0x01 -> 1 0 0 0 0 0 0 0
    assert_eq!(hdlc_frame_bits(&[0x01], 0, 0), vec![1, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn nrzi_zero_toggles_one_holds() {
    assert_eq!(nrzi_encode(&[1, 0, 0, 1, 0]), vec![1, 0, 1, 1, 0]);
}

#[test]
fn afsk_tones_are_mark_and_space() {
    let rate = 48_000u32;
    let per_bit = (rate / 1200) as usize;
    for (level, freq) in [(1u8, 1200.0f64), (0u8, 2200.0f64)] {
        let audio = afsk1200_modulate(&[level; 120], rate);
        assert_eq!(audio.len(), 120 * per_bit);
        let iq: Vec<Complex<f32>> = audio[..4800]
            .iter()
            .map(|&a| Complex::new(a, 0.0))
            .collect();
        let p = dft_power(&iq);
        let peak = (1..2400).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap();
        let peak_hz = peak as f64 * rate as f64 / 4800.0;
        assert!(
            (peak_hz - freq).abs() <= 10.0,
            "level {level}: peak {peak_hz}"
        );
    }
    // Continuous phase: no sample-to-sample jump larger than the 2200 Hz step.
    let audio = afsk1200_modulate(&[1, 0, 1, 1, 0, 0, 1, 0], rate);
    let max_step = 2.0 * std::f64::consts::PI * 2200.0 / rate as f64 * 1.01;
    for w in audio.windows(2) {
        assert!(((w[1] - w[0]) as f64).abs() <= max_step);
    }
}

#[test]
fn nbfm_modulate_has_unit_magnitude_and_right_bandwidth() {
    let audio_rate = 48_000u32;
    let iq_rate = 96_000u32;
    let (dev, audio_hz, offset) = (3000.0f64, 1000.0f64, 10_000.0f64);
    let audio = tone(audio_rate, 4800, audio_hz, 1.0);
    let iq = nbfm_modulate(&audio, audio_rate, iq_rate, dev, offset);
    assert_eq!(iq.len(), 9600);
    for s in &iq {
        assert!((s.norm() - 1.0).abs() < 1e-3);
    }
    let n = 4096;
    let p = dft_power(&iq[..n]);
    let total: f64 = p.iter().sum();
    let half = (dev + audio_hz) * 1.2;
    let inband: f64 = p
        .iter()
        .enumerate()
        .filter(|(k, _)| {
            let k = *k as f64;
            let f = if k < n as f64 / 2.0 { k } else { k - n as f64 } * iq_rate as f64 / n as f64;
            (f - offset).abs() <= half
        })
        .map(|(_, v)| v)
        .sum();
    assert!(
        inband / total >= 0.99,
        "in-band fraction {}",
        inband / total
    );
}

#[test]
fn nco_is_unit_tone() {
    let x = nco(4096, 1500.0, 48_000.0);
    assert!(x.iter().all(|s| (s.norm() - 1.0).abs() < 1e-5));
    let p = dft_power(&x);
    let peak = (0..4096).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap();
    assert_eq!(peak, 128); // 1500 / 48000 * 4096
    assert!(power_db(&x).abs() < 0.01);
}

#[test]
fn lowpass_taps_unity_dc_gain() {
    let taps = lowpass_taps(5_000.0, 48_000.0, 63);
    assert_eq!(taps.len(), 63);
    let sum: f32 = taps.iter().sum();
    assert!((sum - 1.0).abs() < 1e-4, "sum {sum}");
    let h = hann(5);
    assert!(h[0].abs() < 1e-6 && (h[2] - 1.0).abs() < 1e-6);
}

#[test]
fn fir_decimate_passes_dc_and_rejects_stopband() {
    let taps = lowpass_taps(2_000.0, 48_000.0, 101);
    let dc = vec![Complex::new(1.0f32, 0.0); 4800];
    let y = fir_decimate(&dc, &taps, 4);
    assert_eq!(y.len(), 1200);
    assert!((y[600] - Complex::new(1.0, 0.0)).norm() < 1e-3);
    let hi = nco(4800, 12_000.0, 48_000.0);
    let y = fir_decimate(&hi, &taps, 4);
    assert!(power_db(&y[100..]) < -40.0);
}

#[test]
fn awgn_is_deterministic() {
    let base = nco(2048, 1000.0, 48_000.0);
    let (mut a, mut b, mut c) = (base.clone(), base.clone(), base.clone());
    awgn(&mut a, 10.0, 42);
    awgn(&mut b, 10.0, 42);
    awgn(&mut c, 10.0, 43);
    assert_eq!(a, b);
    assert_ne!(a, c);
    // Noise power about 10 dB below the unit-power signal.
    let noise: Vec<Complex<f32>> = a.iter().zip(&base).map(|(x, y)| x - y).collect();
    assert!(
        (power_db(&noise) + 10.0).abs() < 0.5,
        "{}",
        power_db(&noise)
    );
    // Zero signal: noise relative to unit power.
    let mut z = vec![Complex::new(0.0f32, 0.0); 4096];
    awgn(&mut z, 0.0, 1);
    assert!(power_db(&z).abs() < 0.5);
}

#[test]
fn aprs_packet_iq_is_padded_nbfm() {
    let rate = 96_000u32;
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", rate, 5_000.0);
    // 1 s of silence plus (32 + 4 flags + stuffed frame) bits at 1200 baud.
    assert!(iq.len() > rate as usize);
    assert!(iq.iter().all(|s| (s.norm() - 1.0).abs() < 1e-3));
}

#[test]
fn write_fixture_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let iq = nco(480, 1000.0, 48_000.0);
    let p = write_fixture(dir.path(), "fx", &iq, 48_000, 144_390_000);
    let m = zeta_source_file::read_meta(&p).unwrap();
    assert_eq!(m.sample_rate_hz.0, 48_000);
    assert_eq!(m.center_hz.0, 144_390_000);
}

#[test]
fn nbfm_instantaneous_frequency_tracks_audio() {
    let rate = 48_000u32;
    let audio = tone(rate, 2000, 700.0, 0.9);
    let iq = nbfm_modulate(&audio, rate, rate, 3000.0, -2000.0);
    // Phase step from sample n to n+1 encodes audio[n].
    for n in 0..iq.len() - 1 {
        let f =
            (iq[n + 1] * iq[n].conj()).arg() as f64 * rate as f64 / (2.0 * std::f64::consts::PI);
        let want = -2000.0 + 3000.0 * audio[n] as f64;
        assert!((f - want).abs() < 1.0, "n {n}: {f} vs {want}");
    }
}
