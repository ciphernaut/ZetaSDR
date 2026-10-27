//! Signal generators: NBFM, WFM, AM, SSB, AFSK1200 / AX.25 / APRS, POCSAG, RDS, Mode S,
//! ACARS, AWGN, and the registry's synthesiser ([`SynthSpec`], [`synthesise`]). Ungated:
//! `zeta modes check` runs the rows' tests on them. The test-only helpers stay in
//! `fixtures`, behind feature `test-fixtures`.

use std::f64::consts::PI;

use num_complex::Complex;
use rustfft::FftPlanner;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
/// resampling and no further filtering. Used by [`rds_station_iq`]; [`wfm_modulate`] does not use it.
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
pub fn resample_linear(audio: &[f32], audio_rate_hz: u32, iq_rate_hz: u32) -> Vec<f32> {
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

/// Split `CALL` or `CALL-SSID`; the reason when it is not a valid AX.25 call.
fn check_call(s: &str) -> Result<(&str, u8), String> {
    let (call, ssid) = match s.split_once('-') {
        Some((c, n)) => (
            c,
            n.parse::<u8>()
                .map_err(|_| format!("ax25: bad SSID in {s:?}"))?,
        ),
        None => (s, 0),
    };
    if call.is_empty() || call.len() > 6 || !call.is_ascii() {
        return Err(format!("ax25: bad callsign {s:?}"));
    }
    if ssid > 15 {
        return Err(format!("ax25: SSID {ssid} > 15"));
    }
    Ok((call, ssid))
}

/// [`check_call`]; panics on an invalid call (fixtures only).
fn parse_call(s: &str) -> (&str, u8) {
    check_call(s).unwrap_or_else(|e| panic!("{e}"))
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

/// Mode S / ADS-B character set, indexed by 6-bit code (`#` = unused).
const MODES_CHARS: &[u8; 64] = b"#ABCDEFGHIJKLMNOPQRSTUVWXYZ##### ###############0123456789######";

/// Mode S CRC-24 (generator `0xFFF409`) over `bytes`.
pub fn modes_crc24(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &b in bytes {
        crc ^= u32::from(b) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x100_0000 != 0 {
                crc ^= 0x1FF_F409;
            }
        }
    }
    crc & 0xFF_FFFF
}

/// The 112-bit DF17 (CA 5) aircraft identification message for `icao`
/// (24 bits) and `callsign` (at most 8 characters from the Mode S set,
/// space-padded): type code 4, category 0, CRC-24 parity in bytes 11..14.
pub fn modes_ident_frame(icao: u32, callsign: &str) -> [u8; 14] {
    assert!(icao <= 0xFF_FFFF, "icao is 24 bits");
    assert!(callsign.len() <= 8, "callsign is at most 8 characters");
    let mut f = [0u8; 14];
    f[0] = (17 << 3) | 5;
    f[1..4].copy_from_slice(&icao.to_be_bytes()[1..]);
    f[4] = 4 << 3;
    let mut chars: u64 = 0;
    for i in 0..8 {
        let c = callsign.as_bytes().get(i).copied().unwrap_or(b' ');
        let code = MODES_CHARS
            .iter()
            .position(|&m| m == c && m != b'#')
            .unwrap_or_else(|| panic!("{:?} is not in the Mode S character set", c as char));
        chars = (chars << 6) | code as u64;
    }
    f[5..11].copy_from_slice(&chars.to_be_bytes()[2..]);
    let crc = modes_crc24(&f[..11]);
    f[11..14].copy_from_slice(&crc.to_be_bytes()[1..]);
    f
}

/// [`modes_ident_frame`] as pulse-position-modulated baseband at `iq_rate_hz`:
/// preamble pulses at 0, 1.0, 3.5 and 4.5 us, each 0.5 us; bit i at `8 + i` us,
/// a 0.5 us pulse in the first half for 1 and the second half for 0. Pulses
/// have amplitude 0.6 on the real axis; each sample holds the pulse energy
/// falling within its interval (so pulses need not align with samples). The
/// frame is sent `repeats` times, 1 ms apart, with 10 ms of silence before
/// the first and after the last 1 ms slot.
pub fn modes_frame_iq(
    icao: u32,
    callsign: &str,
    repeats: usize,
    iq_rate_hz: u32,
) -> Vec<Complex<f32>> {
    let frame = modes_ident_frame(icao, callsign);
    // Pulse start times in microseconds from the frame start.
    let mut pulses: Vec<f64> = vec![0.0, 1.0, 3.5, 4.5];
    for i in 0..112 {
        let bit = frame[i / 8] >> (7 - i % 8) & 1;
        pulses.push(8.0 + i as f64 + if bit == 1 { 0.0 } else { 0.5 });
    }
    let rate = f64::from(iq_rate_hz);
    let us = rate / 1e6;
    let lead = (rate * 0.010).round() as usize;
    let slot = (rate * 0.001).round() as usize;
    let mut out = vec![Complex::new(0.0f32, 0.0); 2 * lead + repeats * slot];
    for r in 0..repeats {
        let start = (lead + r * slot) as f64;
        for &p in &pulses {
            // The pulse spans [a, b) in samples; sample n covers [n, n + 1).
            let (a, b) = (start + p * us, start + (p + 0.5) * us);
            for n in a.floor() as usize..(b.ceil() as usize).min(out.len()) {
                let overlap = (b.min(n as f64 + 1.0) - a.max(n as f64)).max(0.0);
                out[n].re += (0.6 * overlap) as f32;
            }
        }
    }
    out
}

/// One ACARS character with odd parity in bit 7.
fn acars_parity(c: u8) -> u8 {
    let c = c & 0x7F;
    if c.count_ones().is_multiple_of(2) {
        c | 0x80
    } else {
        c
    }
}

/// A whole ACARS block as transmitted (ARINC 618), each character with odd
/// parity in bit 7: `+` `*` SYN SYN SOH, `mode`, `reg` left-padded with `.`
/// to 7, NAK (no acknowledgement), `label` (2), `block_id`, STX, `text`, ETX,
/// the [`crate::acars::crc16`] of mode..ETX as transmitted (parity bits
/// included), low byte first, then DEL.
pub fn acars_block_bytes(
    mode: char,
    reg: &str,
    label: &str,
    block_id: char,
    text: &str,
) -> Vec<u8> {
    use crate::acars::{DEL, ETX, NAK, SOH, STX, SYN};
    let mut body = vec![mode as u8];
    body.extend(format!("{reg:.>7}").bytes());
    body.push(NAK);
    body.extend(label.bytes());
    body.push(block_id as u8);
    body.push(STX);
    body.extend(text.bytes());
    body.push(ETX);
    let body: Vec<u8> = body.into_iter().map(acars_parity).collect();
    let crc = crate::acars::crc16(&body);
    let mut out: Vec<u8> = [b'+', b'*', SYN, SYN, SOH].map(acars_parity).to_vec();
    out.extend_from_slice(&body);
    out.extend(crc.to_le_bytes());
    out.push(acars_parity(DEL));
    out
}

/// ACARS audio: 128 pre-key ones then `bytes`, each LSB first, as 2400 bps
/// MSK at amplitude 0.8, phase-continuous across bits; the tone of each bit
/// from [`crate::acars::bit_tones`] (the pre-key is a steady
/// [`MARK_HZ`](crate::acars::MARK_HZ) tone). Bit `k` covers samples
/// `k * rate / 2400 .. (k + 1) * rate / 2400`. acarsdec needs at least 12
/// whole bytes of pre-key after its loop locks, so 64 bits is too few.
pub fn acars_modulate(bytes: &[u8], audio_rate_hz: u32) -> Vec<f32> {
    use crate::acars::BAUD;
    const PREKEY_BITS: usize = 128;
    let mut bits = vec![1u8; PREKEY_BITS];
    bits.extend(
        bytes
            .iter()
            .flat_map(|&b| (0..8).map(move |i| (b >> i) & 1)),
    );
    let tones = crate::acars::bit_tones(&bits);
    let rate = audio_rate_hz as u64;
    let len = (bits.len() as u64 * rate / BAUD as u64) as usize;
    let mut phase = 0.0f64;
    (0..len)
        .map(|n| {
            let k = ((n as u64 * BAUD as u64) / rate) as usize;
            let s = 0.8 * phase.sin() as f32;
            phase = (phase + 2.0 * PI * tones[k] / rate as f64).rem_euclid(2.0 * PI);
            s
        })
        .collect()
}

/// One ACARS uplink-style block (mode `2`, block id `A`, so the text is
/// kept whole) as complex baseband: 0.2 s silence, [`acars_modulate`] at
/// 48 kHz, 0.2 s silence, through [`am_modulate`] at index 0.9 and
/// `offset_hz` from centre.
pub fn acars_packet_iq(
    reg: &str,
    label: &str,
    text: &str,
    iq_rate_hz: u32,
    offset_hz: f64,
) -> Vec<Complex<f32>> {
    const AUDIO_RATE: u32 = 48_000;
    let silence = vec![0.0f32; AUDIO_RATE as usize / 5];
    let mut audio = silence.clone();
    audio.extend(acars_modulate(
        &acars_block_bytes('2', reg, label, 'A', text),
        AUDIO_RATE,
    ));
    audio.extend_from_slice(&silence);
    am_modulate(&audio, AUDIO_RATE, iq_rate_hz, 0.9, offset_hz)
}

/// What a mode row's `test` block synthesises (`family` names the variant); the
/// parameters of [`synthesise`]'s signal. Echo and other multipath impairments are
/// deferred to plan 17.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "family", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum SynthSpec {
    /// An APRS UI packet, AFSK1200 over NBFM.
    Aprs {
        src: String,
        dst: String,
        info: String,
    },
    /// One POCSAG page at `baud`, over NBFM.
    Pocsag {
        address: u32,
        function: u8,
        text: String,
        baud: u32,
    },
    /// A broadcast FM station carrying RDS (`pi`, 8-character `ps`, `rt`).
    Rds { pi: u16, ps: String, rt: String },
    /// One ACARS block, MSK over AM.
    Acars {
        reg: String,
        label: String,
        text: String,
    },
    /// Mode S identification frames (`icao`, `callsign`) sent `repeats` times.
    ModesIdent {
        icao: u32,
        callsign: String,
        repeats: usize,
    },
    /// A tone frequency-modulated onto a carrier: one second of `tone_hz`.
    NbfmTone { tone_hz: f64, deviation_hz: f64 },
}

/// Impairments applied to a synthesised signal; each defaults to none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Impairments {
    /// Complex white noise at this SNR below the signal's mean power; `None` for none.
    #[serde(default)]
    pub snr_db: Option<f32>,
    /// Where the signal sits from the capture's centre.
    #[serde(default)]
    pub offset_hz: f64,
    /// Sample-clock error in parts per million: the capture is resampled by
    /// `1 + ppm / 1e6` ([`resample_linear`]), so its length changes by that ratio.
    #[serde(default)]
    pub ppm: f64,
}

/// Seed of the noise [`Impairments::snr_db`] adds, so a check is repeatable.
const NOISE_SEED: u64 = 0x5EED;

impl SynthSpec {
    /// Check the parameters the generators assert on, so a bad row reads as an error,
    /// not a panic: AX.25 calls, POCSAG baud, function and numeric text, the Mode S
    /// ICAO (24 bits) and callsign (at most 8 characters of the Mode S set).
    pub fn validate(&self) -> Result<(), String> {
        match self {
            SynthSpec::Aprs { src, dst, .. } => {
                check_call(src)?;
                check_call(dst)?;
            }
            SynthSpec::Pocsag {
                function,
                text,
                baud,
                ..
            } => {
                if !crate::pocsag::BAUDS.contains(baud) {
                    return Err(format!("pocsag: {baud} baud is not standard"));
                }
                if *function > 3 {
                    return Err(format!("pocsag: function {function} > 3"));
                }
                if *function == 0
                    && let Some(c) = text.chars().find(|c| !"0123456789*U -)(".contains(*c))
                {
                    return Err(format!(
                        "pocsag: {c:?} is not numeric (function 0 is numeric; use 3 for text)"
                    ));
                }
            }
            SynthSpec::ModesIdent { icao, callsign, .. } => {
                if *icao > 0xFF_FFFF {
                    return Err(format!("icao {icao} is over 24 bits"));
                }
                if callsign.len() > 8 {
                    return Err("callsign is at most 8 characters".into());
                }
                if let Some(c) = callsign
                    .bytes()
                    .find(|c| *c == b'#' || !MODES_CHARS.contains(c))
                {
                    return Err(format!(
                        "{:?} is not in the Mode S character set",
                        c as char
                    ));
                }
            }
            SynthSpec::Rds { .. } | SynthSpec::Acars { .. } | SynthSpec::NbfmTone { .. } => {}
        }
        Ok(())
    }

    /// The sample rate the family's capture is synthesised at: the one each decoder is
    /// proven at (APRS, POCSAG and ACARS at 48 kHz, which the `channel` stage divides
    /// down; RDS at 240 kHz; Mode S at 2.4 MHz; a bare NBFM tone at 48 kHz).
    pub fn rate_hz(&self) -> u32 {
        match self {
            SynthSpec::Aprs { .. } | SynthSpec::Pocsag { .. } | SynthSpec::Acars { .. } => 48_000,
            SynthSpec::Rds { .. } => 240_000,
            SynthSpec::ModesIdent { .. } => 2_400_000,
            SynthSpec::NbfmTone { .. } => 48_000,
        }
    }
}

/// Split a row test block's synthesiser parameters (everything but `kind`, `expect` and
/// `within_ms`) into the [`SynthSpec`] and the [`Impairments`] (`snr_db`, `offset_hz`,
/// `ppm`, each optional). A block that does not parse is the error text.
pub fn parse_params(
    mut params: serde_json::Map<String, serde_json::Value>,
) -> Result<(SynthSpec, Impairments), String> {
    let mut imp = serde_json::Map::new();
    for key in ["snr_db", "offset_hz", "ppm"] {
        if let Some(v) = params.remove(key) {
            imp.insert(key.into(), v);
        }
    }
    let imp = serde_json::from_value(imp.into()).map_err(|e| e.to_string())?;
    let spec: SynthSpec = serde_json::from_value(params.into()).map_err(|e| e.to_string())?;
    spec.validate()?;
    Ok((spec, imp))
}

/// The baseband IQ of `spec` at `rate_hz`, as a receiver centred on `center_hz` sees it:
/// the signal sits `imp.offset_hz` from that centre (the centre itself only labels the
/// capture, so it does not change the samples), then the clock error `imp.ppm` and the
/// noise `imp.snr_db` are applied.
pub fn synthesise(
    spec: &SynthSpec,
    imp: &Impairments,
    rate_hz: u32,
    _center_hz: u64,
) -> Vec<Complex<f32>> {
    let off = imp.offset_hz;
    let mut iq = match spec {
        SynthSpec::Aprs { src, dst, info } => aprs_packet_iq(src, dst, info, rate_hz, off),
        SynthSpec::Pocsag {
            address,
            function,
            text,
            baud,
        } => pocsag_packet_iq(*address, *function, text, *baud, rate_hz, off),
        SynthSpec::Rds { pi, ps, rt } => {
            let mut iq = rds_station_iq(*pi, ps, rt, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::Acars { reg, label, text } => acars_packet_iq(reg, label, text, rate_hz, off),
        SynthSpec::ModesIdent {
            icao,
            callsign,
            repeats,
        } => {
            let mut iq = modes_frame_iq(*icao, callsign, *repeats, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::NbfmTone {
            tone_hz,
            deviation_hz,
        } => {
            let audio = tone(48_000, 48_000, *tone_hz, 1.0);
            nbfm_modulate(&audio, 48_000, rate_hz, *deviation_hz, off)
        }
    };
    if imp.ppm != 0.0 {
        let from = 1_000_000_000u32;
        let to = (f64::from(from) * (1.0 + imp.ppm / 1e6)).round() as u32;
        let re: Vec<f32> = iq.iter().map(|s| s.re).collect();
        let im: Vec<f32> = iq.iter().map(|s| s.im).collect();
        let (re, im) = (
            resample_linear(&re, from, to),
            resample_linear(&im, from, to),
        );
        iq = re
            .iter()
            .zip(&im)
            .map(|(&r, &i)| Complex::new(r, i))
            .collect();
    }
    if let Some(snr) = imp.snr_db {
        awgn(&mut iq, snr, NOISE_SEED);
    }
    iq
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_ident_frame_matches_known_frame() {
        // The well-known DF17 identification frame of KLM1023 (ICAO 4840D6).
        let hex: String = modes_ident_frame(0x4840D6, "KLM1023")
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        assert_eq!(hex, "8D4840D6202CC371C32CE0576098");
    }

    #[test]
    fn modes_frame_iq_layout() {
        let rate = 2_400_000;
        let iq = modes_frame_iq(0x4840D6, "ZETA1", 2, rate);
        // 10 ms + 2 x 1 ms + 10 ms.
        assert_eq!(iq.len(), 52_800);
        assert!(
            iq.iter()
                .all(|s| s.im == 0.0 && (0.0..=0.6001).contains(&s.re))
        );
        // Silence up to 10 ms, then the first preamble pulse.
        assert!(iq[..24_000].iter().all(|s| s.re == 0.0));
        assert!(iq[24_000].re > 0.5);
        // The second frame starts 1 ms (2400 samples) later.
        assert!(iq[26_400 - 10..26_400].iter().all(|s| s.re == 0.0));
        assert!(iq[26_400].re > 0.5);
        // Trailing 10 ms of silence.
        assert!(iq[iq.len() - 24_000..].iter().all(|s| s.re == 0.0));
    }

    fn aprs() -> SynthSpec {
        SynthSpec::Aprs {
            src: "VK3ABC".into(),
            dst: "APRS".into(),
            info: ">hi".into(),
        }
    }

    #[test]
    fn each_family_synthesises_nonempty_iq_at_the_rate() {
        let specs = [
            aprs(),
            SynthSpec::Pocsag {
                address: 1234567,
                function: 3,
                text: "HELLO ZETA".into(),
                baud: 1200,
            },
            SynthSpec::Rds {
                pi: 0x2AB1,
                ps: "ZETA FM ".into(),
                rt: "HELLO".into(),
            },
            SynthSpec::Acars {
                reg: "VH-ZET".into(),
                label: "H1".into(),
                text: "HELLO".into(),
            },
            SynthSpec::ModesIdent {
                icao: 0x7C6B28,
                callsign: "ZETA01  ".into(),
                repeats: 2,
            },
            SynthSpec::NbfmTone {
                tone_hz: 1000.0,
                deviation_hz: 2500.0,
            },
        ];
        for spec in specs {
            let rate = spec.rate_hz();
            let iq = synthesise(&spec, &Impairments::default(), rate, 100_000_000);
            assert!(
                iq.len() > rate as usize / 100,
                "{spec:?}: {} samples",
                iq.len()
            );
            assert!(iq.iter().any(|s| s.norm_sqr() > 0.0), "{spec:?} is silent");
        }
        // At the family's rate the synthesiser is the family's own generator.
        let iq = synthesise(&aprs(), &Impairments::default(), 48_000, 0);
        assert_eq!(iq, aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0));
    }

    #[test]
    fn ppm_changes_the_length_by_the_ratio() {
        let spec = aprs();
        let plain = synthesise(&spec, &Impairments::default(), 48_000, 0).len();
        let imp = Impairments {
            ppm: 50_000.0,
            ..Impairments::default()
        };
        let skewed = synthesise(&spec, &imp, 48_000, 0).len();
        let want = plain as f64 * 1.05;
        assert!((skewed as f64 - want).abs() <= 2.0, "{skewed} vs {want}");
    }

    #[test]
    fn snr_impairment_lowers_the_measured_snr() {
        let spec = SynthSpec::NbfmTone {
            tone_hz: 1000.0,
            deviation_hz: 2500.0,
        };
        let clean = synthesise(&spec, &Impairments::default(), 48_000, 0);
        let imp = Impairments {
            snr_db: Some(10.0),
            ..Impairments::default()
        };
        let noisy = synthesise(&spec, &imp, 48_000, 0);
        let ps: f64 = clean.iter().map(|s| f64::from(s.norm_sqr())).sum();
        let pn: f64 = clean
            .iter()
            .zip(&noisy)
            .map(|(c, n)| f64::from((n - c).norm_sqr()))
            .sum();
        let snr = 10.0 * (ps / pn).log10();
        assert!((snr - 10.0).abs() < 0.5, "measured {snr} dB");
    }

    #[test]
    fn params_split_into_spec_and_impairments() {
        let v = serde_json::json!({"family": "aprs", "src": "A", "dst": "B", "info": "c", "snr_db": 20.0});
        let (spec, imp) = parse_params(v.as_object().unwrap().clone()).unwrap();
        assert!(matches!(spec, SynthSpec::Aprs { .. }));
        assert_eq!(imp.snr_db, Some(20.0));
        let bad =
            serde_json::json!({"family": "aprs", "src": "A", "dst": "B", "info": "c", "bogus": 1});
        assert!(parse_params(bad.as_object().unwrap().clone()).is_err());
    }
}
