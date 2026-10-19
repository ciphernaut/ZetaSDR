//! Synthetic signal generators for tests: NBFM, WFM, AM, SSB, AFSK1200 / AX.25 /
//! APRS, POCSAG, RDS, AWGN.
//! Behind feature `test-fixtures`.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use num_complex::Complex;
use rustfft::FftPlanner;
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

/// Broadcast-style FM: [`nbfm_modulate`] of `audio` with, when `pilot`, a
/// 19 kHz stereo pilot at amplitude 0.1 added to it first. `audio_rate_hz`
/// must be at least 48 kHz so the pilot is below its Nyquist.
pub fn wfm_modulate(
    audio: &[f32],
    audio_rate_hz: u32,
    iq_rate_hz: u32,
    deviation_hz: f64,
    offset_hz: f64,
    pilot: bool,
) -> Vec<Complex<f32>> {
    assert!(
        audio_rate_hz >= 48_000,
        "wfm_modulate: audio rate {audio_rate_hz} Hz is below 48 kHz"
    );
    if !pilot {
        return nbfm_modulate(audio, audio_rate_hz, iq_rate_hz, deviation_hz, offset_hz);
    }
    let p = tone(audio_rate_hz, audio.len(), 19_000.0, 0.1);
    let mixed: Vec<f32> = audio.iter().zip(&p).map(|(a, b)| a + b).collect();
    nbfm_modulate(&mixed, audio_rate_hz, iq_rate_hz, deviation_hz, offset_hz)
}

/// FM of a ready-made broadcast `composite` (nominally within ±1) already at
/// the IQ rate: [`nbfm_modulate`] at equal rates and no offset, so no
/// resampling and no further filtering. The modulator [`wfm_modulate`] uses.
pub fn wfm_modulate_composite(
    composite: &[f32],
    rate_hz: u32,
    deviation_hz: f64,
) -> Vec<Complex<f32>> {
    nbfm_modulate(composite, rate_hz, rate_hz, deviation_hz, 0.0)
}

/// `audio` resampled from `audio_rate_hz` to `iq_rate_hz` by linear
/// interpolation, as [`nbfm_modulate`] does: `audio.len() * iq_rate_hz /
/// audio_rate_hz` samples.
pub(crate) fn resample_linear(audio: &[f32], audio_rate_hz: u32, iq_rate_hz: u32) -> Vec<f32> {
    if audio.is_empty() {
        return Vec::new();
    }
    let len = (audio.len() as u64 * iq_rate_hz as u64 / audio_rate_hz as u64) as usize;
    let ratio = audio_rate_hz as f64 / iq_rate_hz as f64;
    let last = audio.len() - 1;
    (0..len)
        .map(|n| {
            let t = n as f64 * ratio;
            let i = (t as usize).min(last);
            let frac = (t - i as f64) as f32;
            audio[i] * (1.0 - frac) + audio[(i + 1).min(last)] * frac
        })
        .collect()
}

/// `x[n]` times a unit carrier `exp(j 2 pi offset_hz n / rate_hz)`.
fn shift(x: &mut [Complex<f32>], offset_hz: f64, rate_hz: u32) {
    let step = offset_hz / rate_hz as f64;
    for (n, s) in x.iter_mut().enumerate() {
        let (sn, cs) = (2.0 * PI * (n as f64 * step).fract()).sin_cos();
        *s *= Complex::new(cs as f32, sn as f32);
    }
}

/// Double-sideband full-carrier AM: `(1 + modulation_index * a[n])` times a
/// unit carrier at `offset_hz` from centre, `audio` (nominally within ±1)
/// linearly resampled to `iq_rate_hz` as in [`nbfm_modulate`].
pub fn am_modulate(
    audio: &[f32],
    audio_rate_hz: u32,
    iq_rate_hz: u32,
    modulation_index: f32,
    offset_hz: f64,
) -> Vec<Complex<f32>> {
    let mut out: Vec<Complex<f32>> = resample_linear(audio, audio_rate_hz, iq_rate_hz)
        .iter()
        .map(|&a| Complex::new(1.0 + modulation_index * a, 0.0))
        .collect();
    shift(&mut out, offset_hz, iq_rate_hz);
    out
}

/// Single-sideband suppressed-carrier: the analytic signal of `audio`
/// (linearly resampled to `iq_rate_hz`), formed by FFT with the negative
/// frequencies zeroed for the upper sideband (`usb`) or the positive ones for
/// the lower, the kept side doubled so the amplitude matches the audio's; then
/// shifted to `offset_hz` from centre.
pub fn ssb_modulate(
    audio: &[f32],
    audio_rate_hz: u32,
    iq_rate_hz: u32,
    usb: bool,
    offset_hz: f64,
) -> Vec<Complex<f32>> {
    let mut buf: Vec<Complex<f32>> = resample_linear(audio, audio_rate_hz, iq_rate_hz)
        .iter()
        .map(|&a| Complex::new(a, 0.0))
        .collect();
    let n = buf.len();
    if n == 0 {
        return buf;
    }
    let mut planner = FftPlanner::<f32>::new();
    planner.plan_fft_forward(n).process(&mut buf);
    for (k, b) in buf.iter_mut().enumerate().skip(1) {
        // Bin k < n/2 is positive frequency, k > n/2 negative; DC and an even
        // length's Nyquist bin are kept as they are.
        let positive = 2 * k < n;
        if 2 * k == n {
            continue;
        }
        *b = if positive == usb {
            *b * 2.0
        } else {
            Complex::new(0.0, 0.0)
        };
    }
    planner.plan_fft_inverse(n).process(&mut buf);
    let scale = 1.0 / n as f32;
    buf.iter_mut().for_each(|c| *c *= scale);
    shift(&mut buf, offset_hz, iq_rate_hz);
    buf
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

/// POCSAG codeword for 21 data bits (bit 20 the message flag): the bits, the
/// 10 BCH(31,21) check bits (remainder modulo `x^10+x^9+x^8+x^6+x^5+x^3+1`),
/// then an even-parity bit. Computed here rather than with the decoder's
/// [`bch_ok`](crate::pocsag::bch_ok) so the two check each other.
fn pocsag_codeword(data21: u32) -> u32 {
    const POLY: u32 = 0x769;
    let mut r = (data21 & 0x1F_FFFF) << 10;
    for i in (10..31).rev() {
        if r & (1 << i) != 0 {
            r ^= POLY << (i - 10);
        }
    }
    let w = ((data21 & 0x1F_FFFF) << 11) | (r << 1);
    w | (w.count_ones() & 1)
}

/// Message codewords' 20-bit payloads for `text`: numeric (function 0) as
/// bit-reversed BCD nibbles of `0123456789*U -)(`, padded with spaces; alpha
/// otherwise, as 7-bit ASCII LSB first, then EOT and zero fill. Panics on a
/// character the numeric set lacks (fixtures only).
fn pocsag_payload(function: u8, text: &str) -> Vec<u32> {
    let mut bits = Vec::new();
    if function == 0 {
        for c in text.chars() {
            let v = "0123456789*U -)("
                .find(c)
                .unwrap_or_else(|| panic!("pocsag: {c:?} is not numeric"));
            bits.extend((0..4).map(|i| ((v >> i) & 1) as u8));
        }
        while !bits.len().is_multiple_of(20) {
            bits.extend([0, 0, 1, 1]); // space, 0xC bit-reversed
        }
    } else {
        for b in text.bytes().chain([0x04]) {
            bits.extend((0..7).map(|i| (b >> i) & 1));
        }
        bits.resize(bits.len().div_ceil(20) * 20, 0);
    }
    bits.chunks(20)
        .map(|c| c.iter().fold(0u32, |a, &b| (a << 1) | b as u32))
        .collect()
}

/// POCSAG bit stream for `messages` as `(address, function, text)`: 576
/// preamble bits `1010…`, then batches of [`SYNC`](crate::pocsag::SYNC) and 8
/// frames of 2 codewords. Each message's address codeword goes in the next
/// frame numbered `address & 7` (idle codewords fill the gap), followed by its
/// message codewords (numeric for function 0, alpha for 3, none for 1 or 2 with
/// empty text) which may run on across batches. An idle codeword follows the
/// last message and idles fill out its batch. One `u8` (0 or 1) per bit, the
/// `baud` only checked to be a POCSAG rate.
pub fn pocsag_encode(messages: &[(u32, u8, &str)], baud: u32) -> Vec<u8> {
    use crate::pocsag::{BAUDS, IDLE, SYNC};
    assert!(BAUDS.contains(&baud), "pocsag: {baud} baud is not standard");
    let mut words: Vec<u32> = Vec::new();
    for &(address, function, text) in messages {
        assert!(function < 4, "pocsag: function {function} > 3");
        while (words.len() % 16) / 2 != (address & 7) as usize {
            words.push(IDLE);
        }
        words.push(pocsag_codeword(
            (((address >> 3) & 0x3FFFF) << 2) | function as u32,
        ));
        if !(text.is_empty() && (function == 1 || function == 2)) {
            words.extend(
                pocsag_payload(function, text)
                    .into_iter()
                    .map(|d| pocsag_codeword((1 << 20) | d)),
            );
        }
    }
    words.push(IDLE);
    while !words.len().is_multiple_of(16) {
        words.push(IDLE);
    }
    let mut bits: Vec<u8> = (0..576).map(|i| u8::from(i % 2 == 0)).collect();
    for batch in words.chunks(16) {
        for &w in std::iter::once(&SYNC).chain(batch) {
            bits.extend((0..32).rev().map(|i| ((w >> i) & 1) as u8));
        }
    }
    bits
}

/// NRZ audio for POCSAG `bits`: `+0.8` for 1, `-0.8` for 0, each held for
/// `audio_rate_hz / baud` samples rounded to nearest. At 48 kHz, 512 baud is
/// 93.75 samples a bit, rounded to 94: the fixture then runs at 510.6 baud,
/// 0.3 % slow, well inside what [`BitSync`](crate::pocsag::BitSync) tracks.
pub fn pocsag_modulate(bits: &[u8], baud: u32, audio_rate_hz: u32) -> Vec<f32> {
    let spb = (audio_rate_hz as f64 / baud as f64).round() as usize;
    assert!(spb > 0, "pocsag_modulate: audio rate below the baud rate");
    bits.iter()
        .flat_map(|&b| std::iter::repeat_n(if b != 0 { 0.8 } else { -0.8 }, spb))
        .collect()
}

/// A single POCSAG page as complex baseband: [`pocsag_encode`] →
/// [`pocsag_modulate`] at 48 kHz with 0.3 s of silence before and after →
/// NBFM, deviation 4500 Hz, at `offset_hz` from centre.
pub fn pocsag_packet_iq(
    address: u32,
    function: u8,
    text: &str,
    baud: u32,
    iq_rate_hz: u32,
    offset_hz: f64,
) -> Vec<Complex<f32>> {
    const AUDIO_RATE: u32 = 48_000;
    let bits = pocsag_encode(&[(address, function, text)], baud);
    let silence = vec![0.0f32; AUDIO_RATE as usize * 3 / 10];
    let mut audio = silence.clone();
    audio.extend(pocsag_modulate(&bits, baud, AUDIO_RATE));
    audio.extend_from_slice(&silence);
    nbfm_modulate(&audio, AUDIO_RATE, iq_rate_hz, 4_500.0, offset_hz)
}

/// One RDS block: `data` then its 10-bit check word with `offset` added,
/// MSB first, as 26 bits.
fn rds_block_bits(out: &mut Vec<u8>, data: u16, offset: crate::rds::Offset) {
    let block = ((data as u32) << 10) | crate::rds::checkword(data, offset) as u32;
    out.extend((0..26).rev().map(|i| ((block >> i) & 1) as u8));
}

/// RDS bit stream for a station: 4 type 0A groups (PS, PTY 10, TP set, no
/// alternative frequencies) then 16 type 2A groups (radio text padded with
/// spaces to 64, A/B flag 0), each block data plus check word with its offset
/// (A, B, C, D), the 20 groups repeated 3 times; then differential-encoded
/// (`tx = bit ^ prev_tx`, from 0). One `u8` per bit, 3 × 20 × 104 of them.
/// `ps` and `rt` are space-padded or cut to 8 and 64 bytes.
pub fn rds_group_bits(pi: u16, ps: &str, rt: &str) -> Vec<u8> {
    use crate::rds::Offset;
    const HEAD: u16 = (1 << 10) | (10 << 5); // TP, PTY 10
    let ps = format!("{ps:<8}").into_bytes();
    let rt = format!("{rt:<64}").into_bytes();
    let mut groups: Vec<[u16; 4]> = Vec::with_capacity(20);
    for seg in 0..4u16 {
        let d = u16::from_be_bytes([ps[2 * seg as usize], ps[2 * seg as usize + 1]]);
        groups.push([pi, HEAD | seg, 0xE0E0, d]);
    }
    for seg in 0..16u16 {
        let c = &rt[4 * seg as usize..4 * seg as usize + 4];
        groups.push([
            pi,
            (2 << 12) | HEAD | seg,
            u16::from_be_bytes([c[0], c[1]]),
            u16::from_be_bytes([c[2], c[3]]),
        ]);
    }
    let mut bits = Vec::with_capacity(3 * 20 * 104);
    for _ in 0..3 {
        for g in &groups {
            for (&w, o) in g.iter().zip([Offset::A, Offset::B, Offset::C, Offset::D]) {
                rds_block_bits(&mut bits, w, o);
            }
        }
    }
    let mut prev = 0u8;
    for b in bits.iter_mut() {
        prev ^= *b;
        *b = prev;
    }
    bits
}

/// Samples [`rds_composite`] makes for `n_bits` at `rate_hz`.
fn rds_composite_len(n_bits: usize, rate_hz: u32) -> usize {
    (n_bits as f64 * rate_hz as f64 / crate::rds::BAUD).ceil() as usize
}

/// Broadcast FM composite at `rate_hz` carrying RDS: `0.6 * audio[n]` (0 past
/// its end) + a 19 kHz pilot `0.09 sin(theta)` + RDS at 0.03 on the carrier
/// `sin(3 theta)`, 57 kHz phase-locked to the pilot. Each entry of `bits` is
/// one biphase symbol at 1187.5 Bd: `+h -h` for 1, `-h +h` for 0, `h` a
/// raised-cosine half-symbol (`sin^2` over its half, peak 1). The output covers
/// the symbols exactly: `ceil(bits.len() * rate_hz / 1187.5)` samples.
pub fn rds_composite(bits: &[u8], rate_hz: u32, audio: &[f32]) -> Vec<f32> {
    use crate::rds::{BAUD, PILOT_HZ};
    let len = rds_composite_len(bits.len(), rate_hz);
    let sym_step = BAUD / rate_hz as f64;
    let pilot_step = PILOT_HZ / rate_hz as f64;
    (0..len)
        .map(|n| {
            let s = n as f64 * sym_step;
            let k = (s as usize).min(bits.len().saturating_sub(1));
            let u = (s - k as f64).clamp(0.0, 1.0);
            let first = u < 0.5;
            let h = (PI * (2.0 * u).fract()).sin().powi(2);
            let bit = bits.get(k).copied().unwrap_or(0) != 0;
            let level = if bit == first { h } else { -h };
            let theta = 2.0 * PI * (n as f64 * pilot_step).fract();
            let a = audio.get(n).copied().unwrap_or(0.0) as f64;
            (0.6 * a + 0.09 * theta.sin() + 0.03 * level * (3.0 * theta).sin()) as f32
        })
        .collect()
}

/// A complete RDS station as complex baseband at `iq_rate_hz`:
/// [`rds_group_bits`] → [`rds_composite`] with a 1 kHz unit tone as audio →
/// [`wfm_modulate_composite`] at 75 kHz deviation, carrier at centre.
pub fn rds_station_iq(pi: u16, ps: &str, rt: &str, iq_rate_hz: u32) -> Vec<Complex<f32>> {
    let bits = rds_group_bits(pi, ps, rt);
    let audio = tone(
        iq_rate_hz,
        rds_composite_len(bits.len(), iq_rate_hz),
        1_000.0,
        1.0,
    );
    let composite = rds_composite(&bits, iq_rate_hz, &audio);
    wfm_modulate_composite(&composite, iq_rate_hz, 75_000.0)
}
