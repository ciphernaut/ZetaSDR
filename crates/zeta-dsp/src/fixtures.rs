//! Synthetic signal generators for tests: NBFM, AFSK1200 / AX.25 / APRS, AWGN.
//! Behind feature `test-fixtures`.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use num_complex::Complex;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::types::{Hz, Timestamp};

/// Frequency-modulate real `audio` (nominally within ±1) onto a unit-magnitude
/// carrier at `offset_hz` from centre. Audio is resampled to `iq_rate_hz` by
/// linear interpolation; the output has `audio.len() * iq_rate_hz / audio_rate_hz`
/// samples. Instantaneous frequency is `offset_hz + deviation_hz * audio`.
pub fn nbfm_modulate(
    audio: &[f32],
    audio_rate_hz: u32,
    iq_rate_hz: u32,
    deviation_hz: f64,
    offset_hz: f64,
) -> Vec<Complex<f32>> {
    if audio.is_empty() {
        return Vec::new();
    }
    let len = (audio.len() as u64 * iq_rate_hz as u64 / audio_rate_hz as u64) as usize;
    let ratio = audio_rate_hz as f64 / iq_rate_hz as f64;
    let last = audio.len() - 1;
    let mut phase = 0.0f64;
    let mut out = Vec::with_capacity(len);
    for n in 0..len {
        let t = n as f64 * ratio;
        let i = (t as usize).min(last);
        let frac = (t - i as f64) as f32;
        let a = audio[i] * (1.0 - frac) + audio[(i + 1).min(last)] * frac;
        let (s, c) = phase.sin_cos();
        out.push(Complex::new(c as f32, s as f32));
        phase += 2.0 * PI * (offset_hz + deviation_hz * a as f64) / iq_rate_hz as f64;
        phase = phase.rem_euclid(2.0 * PI);
    }
    out
}

/// `len` samples of `amp * sin(2 pi freq_hz n / rate_hz)`.
pub fn tone(rate_hz: u32, len: usize, freq_hz: f64, amp: f32) -> Vec<f32> {
    let step = freq_hz / rate_hz as f64;
    (0..len)
        .map(|n| amp * (2.0 * PI * (n as f64 * step).fract()).sin() as f32)
        .collect()
}

/// Bell 202 AFSK at 1200 baud: one entry of `bits` per bit period, taken as the
/// NRZI line level (1 → mark 1200 Hz, 0 → space 2200 Hz). Produces
/// `audio_rate_hz / 1200` samples per bit with continuous phase, amplitude 1.
pub fn afsk1200_modulate(bits: &[u8], audio_rate_hz: u32) -> Vec<f32> {
    let spb = (audio_rate_hz / 1200) as usize;
    assert!(spb > 0, "afsk1200_modulate: audio rate below 1200 Hz");
    let mut phase = 0.0f64;
    let mut out = Vec::with_capacity(bits.len() * spb);
    for &b in bits {
        let f = if b != 0 { 1200.0 } else { 2200.0 };
        let step = 2.0 * PI * f / audio_rate_hz as f64;
        for _ in 0..spb {
            out.push(phase.sin() as f32);
            phase = (phase + step).rem_euclid(2.0 * PI);
        }
    }
    out
}

/// Split `CALL` or `CALL-SSID`; panics on an invalid call (fixtures only).
fn parse_call(s: &str) -> (&str, u8) {
    let (call, ssid) = match s.split_once('-') {
        Some((c, n)) => (c, n.parse::<u8>().expect("ax25: bad SSID")),
        None => (s, 0),
    };
    assert!(
        !call.is_empty() && call.len() <= 6 && call.is_ascii(),
        "ax25: bad callsign {s:?}"
    );
    assert!(ssid <= 15, "ax25: SSID {ssid} > 15");
    (call, ssid)
}

/// An AX.25 UI frame: destination, source, then digipeaters (each 6 space-padded
/// characters shifted left one bit, then SSID byte `0x60 | ssid << 1`, H bit clear,
/// bit 0 set on the last address); control 0x03; PID 0xF0; `info`; FCS
/// ([`crc16_x25`](crate::crc16_x25)) appended little-endian.
pub fn ax25_ui_frame(src: &str, dst: &str, path: &[&str], info: &[u8]) -> Vec<u8> {
    let addrs: Vec<&str> = [dst, src].into_iter().chain(path.iter().copied()).collect();
    let mut f = Vec::with_capacity(addrs.len() * 7 + 4 + info.len());
    for (i, a) in addrs.iter().enumerate() {
        let (call, ssid) = parse_call(a);
        let padded = format!("{call:<6}");
        f.extend(padded.bytes().map(|c| c << 1));
        let last = u8::from(i + 1 == addrs.len());
        f.push(0x60 | (ssid << 1) | last);
    }
    f.extend_from_slice(&[0x03, 0xF0]);
    f.extend_from_slice(info);
    let fcs = crate::crc16_x25(&f);
    f.extend_from_slice(&fcs.to_le_bytes());
    f
}

/// HDLC bit stream: `flags_before` 0x7E flags, `frame` bytes LSB-first with a 0
/// stuffed after every five consecutive 1s, then `flags_after` flags. Flags are
/// not stuffed. One `u8` (0 or 1) per bit.
pub fn hdlc_frame_bits(frame: &[u8], flags_before: usize, flags_after: usize) -> Vec<u8> {
    const FLAG: [u8; 8] = [0, 1, 1, 1, 1, 1, 1, 0];
    let mut out = Vec::with_capacity((flags_before + flags_after) * 8 + frame.len() * 10);
    for _ in 0..flags_before {
        out.extend_from_slice(&FLAG);
    }
    let mut ones = 0;
    for &byte in frame {
        for i in 0..8 {
            let bit = (byte >> i) & 1;
            out.push(bit);
            if bit == 1 {
                ones += 1;
                if ones == 5 {
                    out.push(0);
                    ones = 0;
                }
            } else {
                ones = 0;
            }
        }
    }
    for _ in 0..flags_after {
        out.extend_from_slice(&FLAG);
    }
    out
}

/// NRZI encode from line level 1: a 0 bit toggles the level, a 1 bit holds it.
/// Returns one level per input bit.
pub fn nrzi_encode(bits: &[u8]) -> Vec<u8> {
    let mut level = 1u8;
    bits.iter()
        .map(|&b| {
            if b == 0 {
                level ^= 1;
            }
            level
        })
        .collect()
}

/// A complete APRS packet as complex baseband: UI frame (no digipeaters) →
/// HDLC (32 leading, 4 trailing flags) → NRZI → AFSK1200 at 48 kHz, amplitude
/// 0.8, with 0.5 s of silence before and after → NBFM, deviation 3000 Hz, at
/// `offset_hz` from centre.
pub fn aprs_packet_iq(
    src: &str,
    dst: &str,
    info: &str,
    iq_rate_hz: u32,
    offset_hz: f64,
) -> Vec<Complex<f32>> {
    const AUDIO_RATE: u32 = 48_000;
    let frame = ax25_ui_frame(src, dst, &[], info.as_bytes());
    let levels = nrzi_encode(&hdlc_frame_bits(&frame, 32, 4));
    let silence = vec![0.0f32; AUDIO_RATE as usize / 2];
    let mut audio = silence.clone();
    audio.extend(
        afsk1200_modulate(&levels, AUDIO_RATE)
            .iter()
            .map(|a| a * 0.8),
    );
    audio.extend_from_slice(&silence);
    nbfm_modulate(&audio, AUDIO_RATE, iq_rate_hz, 3000.0, offset_hz)
}

/// Add complex white Gaussian noise at `snr_db` below the signal's measured mean
/// power. A deterministic 64-bit LCG plus Box-Muller generates it, so `seed`
/// fixes the sequence. If the signal power is 0 (e.g. an all-zero buffer for a
/// "noise only" fixture) it is taken as 1.0, so noise power is `10^(-snr_db/10)`.
pub fn awgn(x: &mut [Complex<f32>], snr_db: f32, seed: u64) {
    if x.is_empty() {
        return;
    }
    let p: f64 = x.iter().map(|s| s.norm_sqr() as f64).sum::<f64>() / x.len() as f64;
    let p = if p > 0.0 { p } else { 1.0 };
    let sigma = (p / 10f64.powf(snr_db as f64 / 10.0) / 2.0).sqrt();
    let mut state = seed;
    let mut uniform = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        // Top 53 bits, shifted into (0, 1] so ln() is finite.
        ((state >> 11) + 1) as f64 / (1u64 << 53) as f64
    };
    for s in x.iter_mut() {
        let (u1, u2) = (uniform(), uniform());
        let r = sigma * (-2.0 * u1.ln()).sqrt();
        let (sn, cs) = (2.0 * PI * u2).sin_cos();
        *s += Complex::new((r * cs) as f32, (r * sn) as f32);
    }
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
