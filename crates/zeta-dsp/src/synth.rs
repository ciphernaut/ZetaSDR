//! Signal generators: NBFM, WFM, AM, SSB, AFSK1200 / AX.25 / APRS, POCSAG, RDS, Mode S,
//! ACARS, AIS, FLEX, M17, AWGN, and the registry's synthesiser ([`SynthSpec`], [`synthesise`]). Ungated:
//! `zeta modes check` runs the rows' tests on them. The test-only helpers stay in
//! `fixtures`, behind feature `test-fixtures`.

use std::f64::consts::PI;

use num_complex::Complex;
use rustfft::FftPlanner;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::framing::Framing;

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
        words.push(crate::bch::bch3121_word(
            (((address >> 3) & 0x3FFFF) << 2) | function as u32,
        ));
        if !(text.is_empty() && (function == 1 || function == 2)) {
            words.extend(
                pocsag_payload(function, text)
                    .into_iter()
                    .map(|d| crate::bch::bch3121_word((1 << 20) | d)),
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

/// Gap between the bursts of a generic family's `repeat`, in milliseconds.
const BURST_GAP_MS: u64 = 50;
/// Longest `gap_ms` a generic family accepts.
const BURST_GAP_MAX_MS: u64 = 60_000;

fn default_gap_ms() -> u64 {
    BURST_GAP_MS
}

/// The sample rate of the generic `fsk`/`psk`/`ook` families: 48 kHz up to 4800
/// baud, 240 kHz up to 24 000 baud, else 2.4 MHz.
fn generic_rate_hz(symbol_rate_baud: u32) -> u32 {
    match symbol_rate_baud {
        0..=4800 => 48_000,
        4801..=24_000 => 240_000,
        _ => 2_400_000,
    }
}

/// Hex digits as bits, most significant first; `None` for an empty string, an odd
/// length or a non-hex character.
fn hex_bits(hex: &str) -> Option<Vec<u8>> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() * 4);
    for c in hex.chars() {
        let v = c.to_digit(16)?;
        out.extend((0..4).rev().map(|b| ((v >> b) & 1) as u8));
    }
    Some(out)
}

/// The longest `bits` hex string a row may give.
const MAX_BITS_HEX: usize = 65_536;

/// A generic family's `bits` as symbols `0..order`: a plain hex string is the exact
/// bit pattern, `pattern:<hex>` repeats it until one second of symbols; `order` 4
/// consumes bit pairs (a lone last bit is padded with 0).
fn generic_symbols(bits: &str, order: u8, symbol_rate_baud: u32) -> Result<Vec<u8>, String> {
    const ERR: &str = "bits: hex or pattern:<hex>";
    if bits.strip_prefix("pattern:").unwrap_or(bits).len() > MAX_BITS_HEX {
        return Err(format!("bits: at most {MAX_BITS_HEX} hex characters"));
    }
    let per_symbol = if order == 4 { 2 } else { 1 };
    let mut b: Vec<u8> = match bits.strip_prefix("pattern:") {
        Some(hex) => {
            let one = hex_bits(hex).ok_or(ERR)?;
            let want = (symbol_rate_baud as usize * per_symbol).max(one.len());
            one.iter().copied().cycle().take(want).collect()
        }
        None => hex_bits(bits).ok_or(ERR)?,
    };
    Ok(bit_symbols(&mut b, order))
}

/// `b` as symbols `0..order`, MSB first; a lone last bit of a 4-level stream is padded
/// with 0.
fn bit_symbols(b: &mut Vec<u8>, order: u8) -> Vec<u8> {
    let per_symbol = if order == 4 { 2 } else { 1 };
    if !b.len().is_multiple_of(per_symbol) {
        b.push(0);
    }
    b.chunks(per_symbol)
        .map(|c| c.iter().fold(0, |a, &x| a << 1 | x))
        .collect()
}

/// Gaussian-filtered FSK (BT 0.5, the pulse spanning 2 symbol periods) of `symbols`
/// (each `0..order`) at `symbol_rate_baud`, sampled at `rate_hz`. Order 2 maps 0/1 to
/// -`deviation_hz`/+`deviation_hz`; order 4 maps 0..3 to -3d, -d, +d, +3d with
/// `d = deviation_hz / 3`, so the outer tones sit at ±`deviation_hz`. The Gaussian has
/// unit DC gain, so a run of one symbol reaches its tone.
pub fn gfsk_modulate(
    symbols: &[u8],
    order: u8,
    symbol_rate_baud: u32,
    deviation_hz: f64,
    rate_hz: u32,
) -> Vec<Complex<f32>> {
    if symbols.is_empty() {
        return Vec::new();
    }
    let sps = f64::from(rate_hz) / f64::from(symbol_rate_baud);
    let freq_of = |s: u8| match order {
        4 => (f64::from(s) * 2.0 - 3.0) * deviation_hz / 3.0,
        _ => f64::from(s) * 2.0 * deviation_hz - deviation_hz,
    };
    let n = (symbols.len() as f64 * sps) as usize;
    let raw: Vec<f64> = (0..n)
        .map(|i| freq_of(symbols[((i as f64 / sps) as usize).min(symbols.len() - 1)]))
        .collect();
    let sigma = 2f64.ln().sqrt() / (2.0 * PI * 0.5) * sps;
    let half = sps.round() as isize;
    let mut kernel: Vec<f64> = (-half..=half)
        .map(|k| (-0.5 * (k as f64 / sigma).powi(2)).exp())
        .collect();
    let norm: f64 = kernel.iter().sum();
    kernel.iter_mut().for_each(|k| *k /= norm);
    let mut phase = 0.0f64;
    (0..n)
        .map(|i| {
            let f: f64 = kernel
                .iter()
                .enumerate()
                .map(|(j, k)| {
                    let idx = (i as isize + j as isize - half).clamp(0, n as isize - 1);
                    k * raw[idx as usize]
                })
                .sum();
            phase += 2.0 * PI * f / f64::from(rate_hz);
            let (sn, cs) = phase.sin_cos();
            Complex::new(cs as f32, sn as f32)
        })
        .collect()
}

/// PSK of `symbols` with a raised-cosine pulse (roll-off 0.35, ±3 symbols): BPSK for
/// order 2 (±1), QPSK for order 4 (phase pi/4 + k pi/2).
fn psk_modulate(
    symbols: &[u8],
    order: u8,
    symbol_rate_baud: u32,
    rate_hz: u32,
) -> Vec<Complex<f32>> {
    const BETA: f64 = 0.35;
    let sps = f64::from(rate_hz) / f64::from(symbol_rate_baud);
    let point = |s: u8| match order {
        4 => Complex::from_polar(1.0, PI / 4.0 + f64::from(s) * PI / 2.0),
        _ => Complex::new(f64::from(s) * 2.0 - 1.0, 0.0),
    };
    let pulse = |t: f64| {
        let sinc = if t == 0.0 {
            1.0
        } else {
            (PI * t).sin() / (PI * t)
        };
        let d = 1.0 - (2.0 * BETA * t).powi(2);
        if d.abs() < 1e-9 {
            PI / 4.0 * sinc
        } else {
            sinc * (PI * BETA * t).cos() / d
        }
    };
    let n = (symbols.len() as f64 * sps) as usize;
    (0..n)
        .map(|i| {
            let c = (i as f64 / sps - 0.5).round() as isize;
            let mut acc = Complex::new(0.0f64, 0.0);
            for k in (c - 3).max(0)..=(c + 3).min(symbols.len() as isize - 1) {
                acc += point(symbols[k as usize]) * pulse(i as f64 / sps - k as f64 - 0.5);
            }
            Complex::new(acc.re as f32, acc.im as f32)
        })
        .collect()
}

/// On-off keying: full amplitude for a 1, silence for a 0.
fn ook_modulate(symbols: &[u8], symbol_rate_baud: u32, rate_hz: u32) -> Vec<Complex<f32>> {
    if symbols.is_empty() {
        return Vec::new();
    }
    let sps = f64::from(rate_hz) / f64::from(symbol_rate_baud);
    let n = (symbols.len() as f64 * sps) as usize;
    (0..n)
        .map(|i| {
            let s = symbols[((i as f64 / sps) as usize).min(symbols.len() - 1)];
            Complex::new(f32::from(s), 0.0)
        })
        .collect()
}

/// `repeat` copies of `burst` with `gap_ms` of silence between them.
fn repeat_bursts(
    burst: Vec<Complex<f32>>,
    repeat: u32,
    gap_ms: u64,
    rate_hz: u32,
) -> Vec<Complex<f32>> {
    let gap = (u64::from(rate_hz) * gap_ms / 1000) as usize;
    let mut out = Vec::with_capacity((burst.len() + gap) * repeat as usize);
    for r in 0..repeat {
        if r > 0 {
            out.resize(out.len() + gap, Complex::new(0.0, 0.0));
        }
        out.extend_from_slice(&burst);
    }
    out
}

/// AIS's line, as the receiver ([`crate::ais`]) reads it.
pub use crate::ais::{AIS_BAUD, AIS_DEVIATION_HZ};
/// The training sequence before each frame's start flag, `0101...`.
pub const AIS_TRAINING_BITS: usize = 24;
/// MMSIs are nine digits.
const AIS_MMSI: std::ops::RangeInclusive<u32> = 100_000_000..=999_999_999;
/// The family's speed is under this (102.2 kt is "102.2 or more", 102.3 not available).
const AIS_SOG_MAX_KT: f64 = 102.2;
/// A vessel name's most characters.
const AIS_NAME_MAX: usize = 20;
/// Most repeats of the `ais` family.
const AIS_REPEAT_MAX: u32 = 60;

/// One AIS frame's line levels: the training, then the HDLC frame (flags and stuffing
/// from [`hdlc_frame_bits`]) of the payload's bytes least significant bit first with the
/// CRC-16/X.25 appended little-endian, as [`ax25_ui_frame`] appends it; NRZI-encoded.
fn ais_frame_levels(payload: &[u8]) -> Vec<u8> {
    let mut frame = crate::ais_msg::payload_to_hdlc_bytes(payload);
    let fcs = crate::crc16_x25(&frame);
    frame.extend_from_slice(&fcs.to_le_bytes());
    let mut bits: Vec<u8> = (0..AIS_TRAINING_BITS).map(|i| (i % 2) as u8).collect();
    bits.extend(hdlc_frame_bits(&frame, 1, 1));
    nrzi_encode(&bits)
}

/// The `ais` family's capture at `rate_hz`: half a second of silence, then per repeat
/// the type 1 frame and (with a name) the type 5 frame, every frame GMSK at
/// [`AIS_BAUD`] and [`AIS_DEVIATION_HZ`], `gap_ms` after the last; half a second of
/// silence after.
fn ais_iq(spec: &SynthSpec, rate_hz: u32) -> Vec<Complex<f32>> {
    let SynthSpec::Ais {
        mmsi,
        lat,
        lon,
        sog_kt,
        cog_deg,
        name,
        repeat,
        gap_ms,
    } = spec
    else {
        return Vec::new();
    };
    use crate::ais_msg::{position_report_bits, static_report_bits};
    let mut payloads = vec![position_report_bits(*mmsi, *lat, *lon, *sog_kt, *cog_deg)];
    if let Some(n) = name {
        payloads.push(static_report_bits(*mmsi, n));
    }
    let frames: Vec<Vec<Complex<f32>>> = payloads
        .iter()
        .map(|p| {
            gfsk_modulate(
                &ais_frame_levels(p),
                2,
                AIS_BAUD,
                AIS_DEVIATION_HZ as f64,
                rate_hz,
            )
        })
        .collect();
    let silence = rate_hz as usize / 2;
    let gap = (u64::from(rate_hz) * gap_ms / 1000) as usize;
    let mut out = vec![Complex::new(0.0, 0.0); silence];
    for r in 0..*repeat {
        for (i, f) in frames.iter().enumerate() {
            if r > 0 || i > 0 {
                out.resize(out.len() + gap, Complex::new(0.0, 0.0));
            }
            out.extend_from_slice(f);
        }
    }
    out.resize(out.len() + silence, Complex::new(0.0, 0.0));
    out
}

/// The RS41's line: 4800 baud GFSK at 2400 Hz deviation.
const RS41_BAUD: u32 = 4800;
const RS41_DEVIATION_HZ: f64 = 2400.0;

/// `frames` RS41 frames (numbered from 1), each starting a second after the last with
/// silence between, at `rate_hz`.
fn rs41_iq(
    serial: &str,
    lat: f64,
    lon: f64,
    alt_m: f64,
    vel_v: f64,
    frames: u32,
    rate_hz: u32,
) -> Vec<Complex<f32>> {
    let second = rate_hz as usize;
    let mut out = Vec::with_capacity(second * frames as usize);
    for n in 0..frames {
        let p = crate::rs41_frame::Rs41Params {
            serial: serial.into(),
            lat,
            lon,
            alt_m,
            vel_v,
            frame_no: n + 1,
        };
        let bits = crate::rs41_frame::rs41_frame_bits(&p);
        let burst = gfsk_modulate(&bits, 2, RS41_BAUD, RS41_DEVIATION_HZ, rate_hz);
        let start = out.len();
        out.extend(burst);
        out.resize(
            start + second.max(out.len() - start),
            Complex::new(0.0, 0.0),
        );
    }
    out
}

/// The bounds a radiosonde family's position and climb share.
fn sonde_position(lat: f64, lon: f64, alt_m: f64, vel_v: f64) -> Result<(), String> {
    if !(lat.is_finite() && (-90.0..=90.0).contains(&lat)) {
        return Err("lat: -90..=90".into());
    }
    if !(lon.is_finite() && (-180.0..=180.0).contains(&lon)) {
        return Err("lon: -180..=180".into());
    }
    if !(alt_m.is_finite() && (-1000.0..=60_000.0).contains(&alt_m)) {
        return Err("alt_m: -1000..=60000".into());
    }
    if !(vel_v.is_finite() && (-100.0..=100.0).contains(&vel_v)) {
        return Err("vel_v: -100..=100".into());
    }
    Ok(())
}

/// Most frames the `dfm` family sends (13.4 s at 224 ms a frame).
const DFM_FRAMES_MAX: u32 = 60;

/// `frames` DFM frames (numbered from 0) back to back, as the sonde sends them: no gap,
/// no preamble.
fn dfm_iq(
    serial: &str,
    lat: f64,
    lon: f64,
    alt_m: f64,
    vel_v: f64,
    frames: u32,
    rate_hz: u32,
) -> Vec<Complex<f32>> {
    use crate::dfm_frame::{BAUD, DEVIATION_HZ, DfmParams, dfm_frame_bits};
    let mut chips = Vec::new();
    for frame in 0..frames {
        chips.extend(dfm_frame_bits(&DfmParams {
            serial: serial.into(),
            lat,
            lon,
            alt_m,
            vel_v,
            frame,
        }));
    }
    gfsk_modulate(&chips, 2, BAUD, DEVIATION_HZ, rate_hz)
}

/// Most frames the `m20` family sends (one a second).
const M20_FRAMES_MAX: u32 = 60;

/// `frames` M20 frames (numbered from 0), one a second: each second the line idles on
/// alternating half-symbols, then sends the header and the frame.
fn m20_iq(
    serial: &str,
    lat: f64,
    lon: f64,
    alt_m: f64,
    vel_v: f64,
    frames: u32,
    rate_hz: u32,
) -> Vec<Complex<f32>> {
    use crate::m20_frame::{BAUD, DEVIATION_HZ, FRAME_CHIPS, M20Params, m20_frame_bits};
    let second = BAUD as usize;
    let mut chips = Vec::with_capacity(second * frames as usize);
    for frame in 0..frames {
        chips.extend((0..second - FRAME_CHIPS).map(|k| (k & 1) as u8));
        chips.extend(m20_frame_bits(&M20Params {
            serial: serial.into(),
            lat,
            lon,
            alt_m,
            vel_v,
            frame,
        }));
    }
    gfsk_modulate(&chips, 2, BAUD, DEVIATION_HZ, rate_hz)
}

/// The FLEX family's FSK deviation, Hz: the tones sit this far either side of the carrier.
pub const FLEX_DEVIATION_HZ: f64 = 4800.0;
/// The only line rate the `flex` family synthesises, bit/s.
const FLEX_BAUD: u32 = crate::flex_frame::BAUD;
/// Most frames the `flex` family sends.
const FLEX_REPEAT_MAX: u32 = 10;

/// `repeat` FLEX frames back to back (frames 0.. of cycle 0), each carrying the page
/// for `capcode` with `text` ([`crate::flex_frame`]): NRZ 2-FSK at 1600 baud, a 1 at
/// `+FLEX_DEVIATION_HZ`, over NBFM at `rate_hz`.
fn flex_iq(capcode: u32, text: &str, repeat: u32, rate_hz: u32) -> Vec<Complex<f32>> {
    use crate::flex_frame::{FRAMES_PER_CYCLE, FlexParams, flex_frame_bits};
    let mut bits = Vec::new();
    for n in 0..repeat {
        bits.extend(flex_frame_bits(&FlexParams {
            capcode,
            text: text.into(),
            cycle: 0,
            frame: (n % u32::from(FRAMES_PER_CYCLE)) as u8,
        }));
    }
    let spb = (rate_hz / FLEX_BAUD) as usize;
    let audio: Vec<f32> = bits
        .iter()
        .flat_map(|&b| std::iter::repeat_n(if b != 0 { 1.0 } else { -1.0 }, spb))
        .collect();
    nbfm_modulate(&audio, rate_hz, rate_hz, FLEX_DEVIATION_HZ, 0.0)
}

/// M17's symbol rate, baud (the M17 specification: 4FSK at 4800 symbols/s).
pub const M17_BAUD: u32 = 4800;
/// M17's outer deviation, Hz: the +3 and -3 symbols sit at ±2.4 kHz.
pub const M17_DEVIATION_HZ: f64 = 2400.0;
/// M17's root-raised-cosine roll-off.
pub const M17_RRC_ALPHA: f64 = 0.5;
/// Symbols the root-raised-cosine pulse spans (the specification's minimum).
const RRC_SPAN_SYMBOLS: f64 = 8.0;
/// Stream frames the `m17` family sends at most (10 s).
const M17_FRAMES_MAX: u32 = 250;

/// The root-raised-cosine impulse response at `t` symbol periods from its centre, for
/// roll-off `alpha` (unnormalised; the removable points taken at their limits).
fn rrc(t: f64, alpha: f64) -> f64 {
    if t.abs() < 1e-9 {
        return 1.0 - alpha + 4.0 * alpha / PI;
    }
    let quarter = 1.0 / (4.0 * alpha);
    if (t.abs() - quarter).abs() < 1e-9 {
        let a = PI / (4.0 * alpha);
        return alpha / 2f64.sqrt() * ((1.0 + 2.0 / PI) * a.sin() + (1.0 - 2.0 / PI) * a.cos());
    }
    ((PI * t * (1.0 - alpha)).sin() + 4.0 * alpha * t * (PI * t * (1.0 + alpha)).cos())
        / (PI * t * (1.0 - (4.0 * alpha * t).powi(2)))
}

/// 4FSK of `symbols` (each `0..=3` for -3, -1, +1, +3) at `symbol_rate_baud`, sampled at
/// `rate_hz`: impulses at the symbol centres through a root-raised-cosine filter
/// (roll-off `alpha`, spanning 8 symbols, unit DC gain), frequency modulated so a run of
/// one symbol sits at its level times `deviation_hz / 3` (the outer symbols at
/// ±`deviation_hz`). M17's modulator: the receiver's matched filter completes the
/// raised cosine. `symbols.len() * rate_hz / symbol_rate_baud` samples.
pub fn rrc_modulate_4fsk(
    symbols: &[u8],
    symbol_rate_baud: u32,
    deviation_hz: f64,
    rate_hz: u32,
    alpha: f64,
) -> Vec<Complex<f32>> {
    if symbols.is_empty() {
        return Vec::new();
    }
    let sps = f64::from(rate_hz) / f64::from(symbol_rate_baud);
    let half = RRC_SPAN_SYMBOLS / 2.0;
    let reach = (half * sps).floor() as isize;
    // Unit DC gain: the pulse's area in symbol periods (its taps at whole-sample
    // offsets, per sample period).
    let gain: f64 = (-reach..=reach)
        .map(|j| rrc(j as f64 / sps, alpha))
        .sum::<f64>()
        / sps;
    let level = |s: u8| f64::from(s.min(3)) * 2.0 - 3.0;
    let n = (symbols.len() as f64 * sps) as usize;
    let last = symbols.len() - 1;
    let mut phase = 0.0f64;
    (0..n)
        .map(|i| {
            // Time in symbol periods; symbol k's centre is at k + 0.5.
            let t = i as f64 / sps;
            let lo = (t - 0.5 - half).ceil().max(0.0) as usize;
            let hi = ((t - 0.5 + half).floor().max(0.0) as usize).min(last);
            let shaped: f64 = (lo..=hi)
                .map(|k| level(symbols[k]) * rrc(t - k as f64 - 0.5, alpha))
                .sum::<f64>()
                / gain;
            phase += 2.0 * PI * shaped * deviation_hz / 3.0 / f64::from(rate_hz);
            let (sn, cs) = phase.sin_cos();
            Complex::new(cs as f32, sn as f32)
        })
        .collect()
}

/// An M17 voice stream ([`crate::m17`]): the preamble, the LSF (`dst`, `src`, TYPE voice
/// at 3200 bps with `can`, META zero), `frames` stream frames with a zero payload (the
/// last with the end-of-transmission bit), then the EOT marker; 4FSK at 4800 baud,
/// ±2400 Hz, root-raised-cosine (alpha 0.5).
fn m17_iq(src: &str, dst: &str, can: u8, frames: u32, rate_hz: u32) -> Vec<Complex<f32>> {
    use crate::m17::{
        DATA_TYPE_VOICE, ENCRYPTION_NONE, Lsf, META_BYTES, STREAM_PAYLOAD_BYTES, eot_symbols,
        lsf_frame_symbols, preamble_symbols, stream_frame_symbols,
    };
    let lsf = Lsf {
        dst: dst.into(),
        src: src.into(),
        stream: true,
        data_type: DATA_TYPE_VOICE,
        encryption: ENCRYPTION_NONE,
        encryption_subtype: 0,
        can,
        meta: [0; META_BYTES],
    };
    let mut symbols = preamble_symbols();
    symbols.extend(lsf_frame_symbols(&lsf));
    let payload = [0u8; STREAM_PAYLOAD_BYTES];
    for n in 0..frames {
        symbols.extend(stream_frame_symbols(
            &lsf,
            n as u16,
            &payload,
            n + 1 == frames,
        ));
    }
    symbols.extend(eot_symbols());
    rrc_modulate_4fsk(&symbols, M17_BAUD, M17_DEVIATION_HZ, rate_hz, M17_RRC_ALPHA)
}

/// Bounds on the generic families' numbers, so a bad row is an error, not an abort or
/// a huge allocation. `deviation_hz` is given for `fsk`, whose tones must fit the rate.
fn generic_params(
    symbol_rate_baud: u32,
    repeat: u32,
    gap_ms: u64,
    deviation_hz: Option<f64>,
) -> Result<(), String> {
    if !(1..=600_000).contains(&symbol_rate_baud) {
        return Err("symbol_rate_baud: 1..=600000".into());
    }
    if !(1..=100).contains(&repeat) {
        return Err("repeat: 1..=100".into());
    }
    if gap_ms > BURST_GAP_MAX_MS {
        return Err(format!("gap_ms: 0..={BURST_GAP_MAX_MS}"));
    }
    if let Some(d) = deviation_hz {
        let half = f64::from(generic_rate_hz(symbol_rate_baud)) / 2.0;
        if !(d.is_finite() && d > 0.0 && d < half) {
            return Err("deviation_hz: finite, 0 < d < rate/2".into());
        }
    }
    Ok(())
}

/// How the `framing` family sends its bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FramingModulation {
    Fsk,
    Psk,
    Ook,
}

fn default_order() -> u8 {
    2
}

/// Most body bytes in all the frames of a `framing` spec.
const MAX_FRAMING_BYTES: u64 = 65_536;

/// A hex string as bytes; `None` for an empty string, an odd length or a non-hex
/// character.
fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    let bits = hex_bits(hex)?;
    Some(
        bits.chunks(8)
            .map(|b| b.iter().fold(0, |a, &x| a << 1 | x))
            .collect(),
    )
}

/// The frame bodies of a `framing` spec: `bodies` decoded when given, else `frames` of
/// `body_bytes` bytes from xorshift64 seeded by `seed`. `Err` names the bad field.
fn framing_bodies(
    frames: u32,
    body_bytes: u32,
    seed: u64,
    bodies: &[String],
) -> Result<Vec<Vec<u8>>, String> {
    if !bodies.is_empty() {
        let out = bodies
            .iter()
            .enumerate()
            .map(|(i, h)| hex_bytes(h).ok_or(format!("bodies[{i}]: hex, whole bytes")))
            .collect::<Result<Vec<_>, _>>()?;
        if out.iter().map(|b| b.len() as u64).sum::<u64>() > MAX_FRAMING_BYTES {
            return Err(format!("bodies: at most {MAX_FRAMING_BYTES} bytes in all"));
        }
        return Ok(out);
    }
    if frames == 0 {
        return Err("frames: at least 1".into());
    }
    if body_bytes == 0 {
        return Err("body_bytes: at least 1 (or give bodies)".into());
    }
    if u64::from(frames) * u64::from(body_bytes) > MAX_FRAMING_BYTES {
        return Err(format!(
            "frames x body_bytes: at most {MAX_FRAMING_BYTES} bytes in all"
        ));
    }
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    Ok((0..frames)
        .map(|_| {
            (0..body_bytes)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    (s >> 24) as u8
                })
                .collect()
        })
        .collect())
}

/// The planted bits of a [`SynthSpec::Framing`] (`None` for any other family or a spec
/// that does not validate): every frame's bits as `encode_frames` lays them on the line.
pub fn framing_bits(spec: &SynthSpec) -> Option<Vec<u8>> {
    let SynthSpec::Framing {
        frames,
        body_bytes,
        seed,
        bodies,
        framing,
        ..
    } = spec
    else {
        return None;
    };
    spec.validate().ok()?;
    let bodies = framing_bodies(*frames, *body_bytes, *seed, bodies).ok()?;
    Some(crate::framing::encode_frames(&bodies, framing))
}

/// What a mode row's `test` block synthesises (`family` names the variant); the
/// parameters of [`synthesise`]'s signal.
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
    /// Gaussian FSK (BT 0.5) of `bits` (hex, or `pattern:<hex>` repeated to one
    /// second), `order` 2 or 4, in `repeat` bursts `gap_ms` apart (50 by default).
    Fsk {
        symbol_rate_baud: u32,
        deviation_hz: f64,
        order: u8,
        bits: String,
        repeat: u32,
        #[serde(default = "default_gap_ms")]
        gap_ms: u64,
    },
    /// PSK (raised cosine, roll-off 0.35) of `bits`, `order` 2 (BPSK) or 4 (QPSK).
    Psk {
        symbol_rate_baud: u32,
        order: u8,
        bits: String,
        repeat: u32,
        #[serde(default = "default_gap_ms")]
        gap_ms: u64,
    },
    /// On-off keying of `bits`, in `repeat` bursts `gap_ms` apart (50 by default).
    Ook {
        symbol_rate_baud: u32,
        bits: String,
        repeat: u32,
        #[serde(default = "default_gap_ms")]
        gap_ms: u64,
    },
    /// `frames` frames framed by `framing` (what `frame_decode` cuts), their bits sent as
    /// `modulation` says (`fsk`: Gaussian FSK at `deviation_hz`; `psk`; `ook`; `order`
    /// 2 or 4 for fsk and psk). A frame's body is `body_bytes` pseudo-random bytes from
    /// `seed`, or the `bodies` (hex, one per frame) when given; the CRC and a `field`
    /// length are added by the framing. `symbol_rate_baud` is the line rate, a
    /// Manchester chip rate included.
    Framing {
        modulation: FramingModulation,
        symbol_rate_baud: u32,
        #[serde(default = "default_order")]
        order: u8,
        #[serde(default)]
        deviation_hz: f64,
        frames: u32,
        body_bytes: u32,
        #[serde(default)]
        seed: u64,
        #[serde(default)]
        bodies: Vec<String>,
        framing: Framing,
        repeat: u32,
        #[serde(default = "default_gap_ms")]
        gap_ms: u64,
    },
    /// `frames` Vaisala RS41 frames one second apart ([`crate::rs41_frame`]): GFSK at
    /// 4800 baud and 2400 Hz deviation carrying `serial`, the position and the climb.
    Rs41 {
        serial: String,
        lat: f64,
        lon: f64,
        alt_m: f64,
        vel_v: f64,
        frames: u32,
    },
    /// `frames` Graw DFM frames back to back ([`crate::dfm_frame`]): 2-FSK at 2500 baud
    /// (Manchester coded) and 4 kHz deviation carrying `serial` (`DFM-` and the number),
    /// the position and the climb.
    Dfm {
        serial: String,
        lat: f64,
        lon: f64,
        alt_m: f64,
        vel_v: f64,
        frames: u32,
    },
    /// `frames` Meteomodem M20 frames one second apart ([`crate::m20_frame`]): 2-FSK at
    /// 9600 baud (Manchester coded) carrying `serial` (`M20-YMM-S-NNNNN`, as `m20mod`
    /// prints the id), the position and the climb; the line idles between frames.
    M20 {
        serial: String,
        lat: f64,
        lon: f64,
        alt_m: f64,
        vel_v: f64,
        frames: u32,
    },
    /// `repeat` FLEX frames back to back ([`crate::flex_frame`]), each paging the short
    /// address `capcode` with the alphanumeric `text`: 2-FSK at `baud` (1600 only in this
    /// wave) and 4800 Hz deviation over NBFM.
    Flex {
        capcode: u32,
        text: String,
        baud: u32,
        repeat: u32,
    },
    /// An M17 voice stream ([`crate::m17`]): the LSF from `src` to `dst` (callsigns of at
    /// most nine base-40 characters; `ALL` is broadcast) with channel access number
    /// `can`, then `frames` stream frames (1..=250, 40 ms each) of a zero Codec 2
    /// payload; 4FSK at 4800 baud, ±2400 Hz.
    M17 {
        src: String,
        dst: String,
        can: u8,
        frames: u32,
    },
    /// AIS: per repeat a type 1 position report (`mmsi`, the position, `sog_kt`,
    /// `cog_deg`; [`crate::ais_msg::position_report_bits`]) and, with `name`, a type 5
    /// static report, GMSK at 9600 baud, frames `gap_ms` apart (50 by default).
    Ais {
        mmsi: u32,
        lat: f64,
        lon: f64,
        sog_kt: f64,
        cog_deg: f64,
        #[serde(default)]
        name: Option<String>,
        repeat: u32,
        #[serde(default = "default_gap_ms")]
        gap_ms: u64,
    },
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
    /// A delayed copy of the signal added to it (multipath); `None` for none.
    #[serde(default)]
    pub echo: Option<Echo>,
}

/// An echo: the signal delayed by `delay_us` (rounded to whole samples) and added at
/// `gain_db` relative to it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Echo {
    pub delay_us: u32,
    pub gain_db: f32,
}

impl Impairments {
    /// Bound the echo so a bad row is an error: `delay_us` 1..=100000 and at least one
    /// sample at `rate_hz`, `gain_db` finite in -60..=0.
    pub fn validate(&self, rate_hz: u32) -> Result<(), String> {
        let Some(echo) = self.echo else {
            return Ok(());
        };
        if !(1..=100_000).contains(&echo.delay_us) {
            return Err("echo.delay_us: 1..=100000".into());
        }
        if (f64::from(echo.delay_us) * f64::from(rate_hz) / 1e6).round() < 1.0 {
            return Err(format!(
                "echo.delay_us: at least one sample at {rate_hz} Hz"
            ));
        }
        if !(echo.gain_db.is_finite() && (-60.0..=0.0).contains(&echo.gain_db)) {
            return Err("echo.gain_db: -60..=0".into());
        }
        Ok(())
    }
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
            SynthSpec::Fsk {
                symbol_rate_baud,
                deviation_hz,
                order,
                bits,
                repeat,
                gap_ms,
            } => {
                if !matches!(order, 2 | 4) {
                    return Err("order: 2 or 4".into());
                }
                generic_params(*symbol_rate_baud, *repeat, *gap_ms, Some(*deviation_hz))?;
                generic_symbols(bits, *order, *symbol_rate_baud)?;
            }
            SynthSpec::Psk {
                symbol_rate_baud,
                order,
                bits,
                repeat,
                gap_ms,
            } => {
                if !matches!(order, 2 | 4) {
                    return Err("order: 2 or 4".into());
                }
                generic_params(*symbol_rate_baud, *repeat, *gap_ms, None)?;
                generic_symbols(bits, *order, *symbol_rate_baud)?;
            }
            SynthSpec::Ook {
                symbol_rate_baud,
                bits,
                repeat,
                gap_ms,
            } => {
                generic_params(*symbol_rate_baud, *repeat, *gap_ms, None)?;
                generic_symbols(bits, 2, *symbol_rate_baud)?;
            }
            SynthSpec::Framing {
                modulation,
                symbol_rate_baud,
                order,
                deviation_hz,
                frames,
                body_bytes,
                seed,
                bodies,
                framing,
                repeat,
                gap_ms,
            } => {
                framing.check().map_err(|e| format!("framing.{e}"))?;
                let fsk = *modulation == FramingModulation::Fsk;
                if *modulation != FramingModulation::Ook && !matches!(order, 2 | 4) {
                    return Err("order: 2 or 4".into());
                }
                generic_params(
                    *symbol_rate_baud,
                    *repeat,
                    *gap_ms,
                    fsk.then_some(*deviation_hz),
                )?;
                let bodies = framing_bodies(*frames, *body_bytes, *seed, bodies)?;
                for (i, b) in bodies.iter().enumerate() {
                    crate::framing::check_body(framing, b.len())
                        .map_err(|e| format!("body {i}: {e}"))?;
                }
            }
            SynthSpec::Rs41 {
                serial,
                lat,
                lon,
                alt_m,
                vel_v,
                frames,
            } => {
                if !(1..=60).contains(frames) {
                    return Err("frames: 1..=60".into());
                }
                if serial.len() != 8
                    || !serial
                        .bytes()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                {
                    return Err("serial: 8 characters of A-Z and 0-9".into());
                }
                sonde_position(*lat, *lon, *alt_m, *vel_v)?;
            }
            SynthSpec::Dfm {
                serial,
                lat,
                lon,
                alt_m,
                vel_v,
                frames,
            } => {
                if !(1..=DFM_FRAMES_MAX).contains(frames) {
                    return Err(format!("frames: 1..={DFM_FRAMES_MAX}"));
                }
                let digits = serial.strip_prefix("DFM-").unwrap_or("");
                if digits.is_empty()
                    || !digits.bytes().all(|c| c.is_ascii_digit())
                    || digits.parse::<u32>().is_err()
                {
                    return Err("serial: DFM- and a decimal number under 2^32".into());
                }
                sonde_position(*lat, *lon, *alt_m, *vel_v)?;
            }
            SynthSpec::M20 {
                serial,
                lat,
                lon,
                alt_m,
                vel_v,
                frames,
            } => {
                if !(1..=M20_FRAMES_MAX).contains(frames) {
                    return Err(format!("frames: 1..={M20_FRAMES_MAX}"));
                }
                if crate::m20_frame::serial_parts(serial).is_none() {
                    return Err(
                        "serial: M20-YMM-S-NNNNN (Y 0-9, MM 01-12, S 1-8, NNNNN 0-8191)".into(),
                    );
                }
                sonde_position(*lat, *lon, *alt_m, *vel_v)?;
            }
            SynthSpec::Ais {
                mmsi,
                lat,
                lon,
                sog_kt,
                cog_deg,
                name,
                repeat,
                gap_ms,
            } => {
                if !AIS_MMSI.contains(mmsi) {
                    return Err("mmsi: nine digits".into());
                }
                if !(lat.is_finite() && (-90.0..=90.0).contains(lat)) {
                    return Err("lat: -90..=90".into());
                }
                if !(lon.is_finite() && (-180.0..=180.0).contains(lon)) {
                    return Err("lon: -180..=180".into());
                }
                if !(sog_kt.is_finite() && (0.0..AIS_SOG_MAX_KT).contains(sog_kt)) {
                    return Err(format!("sog_kt: 0 to under {AIS_SOG_MAX_KT}"));
                }
                if !(cog_deg.is_finite() && (0.0..360.0).contains(cog_deg)) {
                    return Err("cog_deg: 0 to under 360".into());
                }
                if let Some(n) = name
                    && (n.chars().count() > AIS_NAME_MAX
                        || !n.chars().all(crate::ais_msg::is_six_bit_text))
                {
                    return Err(format!(
                        "name: at most {AIS_NAME_MAX} characters of the AIS set (A-Z, 0-9, space, !\"#$%&'()*+,-./:;<=>?)"
                    ));
                }
                if !(1..=AIS_REPEAT_MAX).contains(repeat) {
                    return Err(format!("repeat: 1..={AIS_REPEAT_MAX}"));
                }
                if *gap_ms > BURST_GAP_MAX_MS {
                    return Err(format!("gap_ms: 0..={BURST_GAP_MAX_MS}"));
                }
            }
            SynthSpec::Flex {
                capcode,
                text,
                baud,
                repeat,
            } => {
                use crate::flex_frame::{CAPCODE_MAX, CAPCODE_MIN, TEXT_MAX};
                if *baud != FLEX_BAUD {
                    return Err(format!("baud: this wave synthesises {FLEX_BAUD} bps"));
                }
                if !(CAPCODE_MIN..=CAPCODE_MAX).contains(capcode) {
                    return Err(format!(
                        "capcode: {CAPCODE_MIN}..={CAPCODE_MAX} (a short address)"
                    ));
                }
                if text.len() > TEXT_MAX || !text.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
                {
                    return Err(format!(
                        "text: at most {TEXT_MAX} characters of printable ASCII"
                    ));
                }
                if !(1..=FLEX_REPEAT_MAX).contains(repeat) {
                    return Err(format!("repeat: 1..={FLEX_REPEAT_MAX}"));
                }
            }
            SynthSpec::M17 {
                src,
                dst,
                can,
                frames,
            } => {
                use crate::m17::{CAN_MAX, encode_callsign};
                if src == "ALL" {
                    return Err("src: ALL is the broadcast address, a destination only".into());
                }
                for (key, call) in [("src", src), ("dst", dst)] {
                    if encode_callsign(call).is_none() {
                        return Err(format!(
                            "{key}: at most nine characters of A-Z, 0-9, space, -, / and ."
                        ));
                    }
                }
                if *can > CAN_MAX {
                    return Err(format!("can: 0..={CAN_MAX}"));
                }
                if !(1..=M17_FRAMES_MAX).contains(frames) {
                    return Err(format!("frames: 1..={M17_FRAMES_MAX}"));
                }
            }
            SynthSpec::Rds { .. } | SynthSpec::Acars { .. } | SynthSpec::NbfmTone { .. } => {}
        }
        Ok(())
    }

    /// The sample rate the family's capture is synthesised at: the one each decoder is
    /// proven at (APRS, POCSAG, ACARS, AIS, FLEX and M17 at 48 kHz, which the `channel` stage divides
    /// down; RDS at 240 kHz; Mode S at 2.4 MHz; a bare NBFM tone at 48 kHz).
    pub fn rate_hz(&self) -> u32 {
        match self {
            SynthSpec::Aprs { .. } | SynthSpec::Pocsag { .. } | SynthSpec::Acars { .. } => 48_000,
            SynthSpec::Rds { .. } => 240_000,
            SynthSpec::ModesIdent { .. } => 2_400_000,
            SynthSpec::NbfmTone { .. }
            | SynthSpec::Rs41 { .. }
            | SynthSpec::Dfm { .. }
            | SynthSpec::M20 { .. }
            | SynthSpec::Flex { .. }
            | SynthSpec::Ais { .. }
            | SynthSpec::M17 { .. } => 48_000,
            SynthSpec::Fsk {
                symbol_rate_baud, ..
            }
            | SynthSpec::Psk {
                symbol_rate_baud, ..
            }
            | SynthSpec::Ook {
                symbol_rate_baud, ..
            }
            | SynthSpec::Framing {
                symbol_rate_baud, ..
            } => generic_rate_hz(*symbol_rate_baud),
        }
    }
}

/// Split a row test block's synthesiser parameters (everything but `kind`, `expect` and
/// `within_ms`) into the [`SynthSpec`] and the [`Impairments`] (`snr_db`, `offset_hz`,
/// `ppm`, `echo`, each optional). A block that does not parse is the error text.
pub fn parse_params(
    mut params: serde_json::Map<String, serde_json::Value>,
) -> Result<(SynthSpec, Impairments), String> {
    let mut imp = serde_json::Map::new();
    for key in ["snr_db", "offset_hz", "ppm", "echo"] {
        if let Some(v) = params.remove(key) {
            imp.insert(key.into(), v);
        }
    }
    let imp: Impairments = serde_json::from_value(imp.into()).map_err(|e| e.to_string())?;
    let spec: SynthSpec = serde_json::from_value(params.into()).map_err(|e| e.to_string())?;
    spec.validate()?;
    imp.validate(spec.rate_hz())?;
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
        SynthSpec::Fsk {
            symbol_rate_baud,
            deviation_hz,
            order,
            bits,
            repeat,
            gap_ms,
        } => {
            let sym = generic_symbols(bits, *order, *symbol_rate_baud)
                .expect("synth spec validated before synthesise");
            let burst = gfsk_modulate(&sym, *order, *symbol_rate_baud, *deviation_hz, rate_hz);
            let mut iq = repeat_bursts(burst, *repeat, *gap_ms, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::Psk {
            symbol_rate_baud,
            order,
            bits,
            repeat,
            gap_ms,
        } => {
            let sym = generic_symbols(bits, *order, *symbol_rate_baud)
                .expect("synth spec validated before synthesise");
            let burst = psk_modulate(&sym, *order, *symbol_rate_baud, rate_hz);
            let mut iq = repeat_bursts(burst, *repeat, *gap_ms, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::Ook {
            symbol_rate_baud,
            bits,
            repeat,
            gap_ms,
        } => {
            let sym = generic_symbols(bits, 2, *symbol_rate_baud)
                .expect("synth spec validated before synthesise");
            let burst = ook_modulate(&sym, *symbol_rate_baud, rate_hz);
            let mut iq = repeat_bursts(burst, *repeat, *gap_ms, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::Framing {
            modulation,
            symbol_rate_baud,
            order,
            deviation_hz,
            repeat,
            gap_ms,
            ..
        } => {
            let mut bits = framing_bits(spec).expect("synth spec validated before synthesise");
            let burst = match modulation {
                FramingModulation::Fsk => {
                    let sym = bit_symbols(&mut bits, *order);
                    gfsk_modulate(&sym, *order, *symbol_rate_baud, *deviation_hz, rate_hz)
                }
                FramingModulation::Psk => {
                    let sym = bit_symbols(&mut bits, *order);
                    psk_modulate(&sym, *order, *symbol_rate_baud, rate_hz)
                }
                FramingModulation::Ook => ook_modulate(&bits, *symbol_rate_baud, rate_hz),
            };
            let mut iq = repeat_bursts(burst, *repeat, *gap_ms, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::Rs41 {
            serial,
            lat,
            lon,
            alt_m,
            vel_v,
            frames,
        } => {
            let mut iq = rs41_iq(serial, *lat, *lon, *alt_m, *vel_v, *frames, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::Dfm {
            serial,
            lat,
            lon,
            alt_m,
            vel_v,
            frames,
        } => {
            let mut iq = dfm_iq(serial, *lat, *lon, *alt_m, *vel_v, *frames, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::M20 {
            serial,
            lat,
            lon,
            alt_m,
            vel_v,
            frames,
        } => {
            let mut iq = m20_iq(serial, *lat, *lon, *alt_m, *vel_v, *frames, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::Flex {
            capcode,
            text,
            repeat,
            ..
        } => {
            let mut iq = flex_iq(*capcode, text, *repeat, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::Ais { .. } => {
            let mut iq = ais_iq(spec, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
        SynthSpec::M17 {
            src,
            dst,
            can,
            frames,
        } => {
            let mut iq = m17_iq(src, dst, *can, *frames, rate_hz);
            shift(&mut iq, off, rate_hz);
            iq
        }
    };
    if let Some(echo) = imp.echo {
        let delay = (f64::from(echo.delay_us) * f64::from(rate_hz) / 1e6).round() as usize;
        let gain = 10f32.powf(echo.gain_db / 20.0);
        let clean = iq.clone();
        iq.resize(clean.len() + delay, Complex::new(0.0, 0.0));
        for (n, s) in clean.iter().enumerate() {
            iq[n + delay] += *s * gain;
        }
    }
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

    fn fsk(rate: u32, dev: f64, order: u8, bits: &str) -> SynthSpec {
        SynthSpec::Fsk {
            symbol_rate_baud: rate,
            deviation_hz: dev,
            order,
            bits: bits.into(),
            repeat: 1,
            gap_ms: 50,
        }
    }

    /// Power spectrum (positive and negative bins, DC at `n / 2`) of the first
    /// `n` samples, and the bin width.
    fn spectrum(iq: &[Complex<f32>], rate_hz: u32, n: usize) -> (Vec<f32>, f64) {
        let mut buf: Vec<Complex<f32>> = iq[..n].to_vec();
        FftPlanner::new().plan_fft_forward(n).process(&mut buf);
        let mut shifted = vec![0.0; n];
        for (k, v) in buf.iter().enumerate() {
            shifted[(k + n / 2) % n] = v.norm_sqr();
        }
        (shifted, f64::from(rate_hz) / n as f64)
    }

    fn peak_hz(p: &[f32], bin: f64, lo: usize, hi: usize) -> f64 {
        let k = (lo..hi).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap();
        (k as f64 - (p.len() / 2) as f64) * bin
    }

    #[test]
    fn fsk_tones_sit_at_plus_minus_deviation() {
        let spec = fsk(4800, 2400.0, 2, "pattern:f0f0");
        let iq = synthesise(&spec, &Impairments::default(), 48_000, 0);
        let (p, bin) = spectrum(&iq, 48_000, 4096);
        let (neg, pos) = (peak_hz(&p, bin, 0, 2048), peak_hz(&p, bin, 2048, 4096));
        assert!((pos - 2400.0).abs() <= 2.0 * bin, "{pos}");
        assert!((neg + 2400.0).abs() <= 2.0 * bin, "{neg}");
    }

    #[test]
    fn fsk4_has_four_tones() {
        // Each of the four symbols held for 16 symbol periods.
        let spec = fsk(4800, 3000.0, 4, "pattern:0000000055555555aaaaaaaaffffffff");
        let iq = synthesise(&spec, &Impairments::default(), 48_000, 0);
        let (p, bin) = spectrum(&iq, 48_000, 4096);
        let at = |hz: f64| {
            let k = (2048.0 + hz / bin).round() as usize;
            p[k - 2..=k + 2].iter().cloned().fold(0.0, f32::max)
        };
        let mut sorted = p.clone();
        sorted.sort_by(f32::total_cmp);
        let floor = sorted[sorted.len() / 2];
        for hz in [-3000.0, -1000.0, 1000.0, 3000.0] {
            assert!(
                at(hz) > 20.0 * floor,
                "no tone at {hz}: {} vs floor {floor}",
                at(hz)
            );
        }
        assert!(at(0.0) < at(1000.0) / 4.0);
    }

    #[test]
    fn psk_has_a_carrier_null_and_the_symbol_rate_line() {
        let spec = SynthSpec::Psk {
            symbol_rate_baud: 4800,
            order: 2,
            bits: "pattern:aa".into(),
            repeat: 1,
            gap_ms: 50,
        };
        let iq = synthesise(&spec, &Impairments::default(), 48_000, 0);
        let (p, bin) = spectrum(&iq[1000..], 48_000, 4096);
        let line = p[2048 + (2400.0 / bin).round() as usize];
        let mirror = p[2048 - (2400.0 / bin).round() as usize];
        assert!(p[2048] < line / 1000.0, "carrier not nulled");
        assert!((line / mirror - 1.0).abs() < 0.2, "spectrum not symmetric");
        // Squared, the BPSK signal's envelope carries the symbol rate.
        let sq: Vec<Complex<f32>> = iq[1000..].iter().map(|s| s * s).collect();
        let (q, qbin) = spectrum(&sq, 48_000, 4096);
        let at_rate = q[2048 + (4800.0 / qbin).round() as usize];
        assert!(at_rate > 100.0 * q[2048 + (1000.0 / qbin).round() as usize]);
    }

    #[test]
    fn ook_is_silent_between_ones() {
        // 80 = 1 then seven 0s, 4800 baud at 48 kHz: 10 samples per symbol.
        let spec = SynthSpec::Ook {
            symbol_rate_baud: 4800,
            bits: "80".into(),
            repeat: 1,
            gap_ms: 50,
        };
        let iq = synthesise(&spec, &Impairments::default(), 48_000, 0);
        assert_eq!(iq.len(), 80);
        assert!(iq[..10].iter().all(|s| s.re == 1.0));
        assert!(iq[10..].iter().all(|s| s.norm() == 0.0));
    }

    #[test]
    fn echo_adds_a_delayed_copy() {
        // 100 kbaud synthesises at 2.4 MHz: a 24-sample pulse, echo 240 samples later.
        let spec = SynthSpec::Ook {
            symbol_rate_baud: 100_000,
            bits: "80".into(),
            repeat: 1,
            gap_ms: 50,
        };
        let imp = Impairments {
            echo: Some(Echo {
                delay_us: 100,
                gain_db: -6.0,
            }),
            ..Default::default()
        };
        let iq = synthesise(&spec, &imp, 2_400_000, 0);
        assert!((iq[0].re - 1.0).abs() < 1e-6);
        assert!((iq[240].re - 0.5012).abs() < 1e-3);
        assert!(iq[100].norm() == 0.0 && iq[300].norm() == 0.0);
    }

    #[test]
    fn rates_follow_the_symbol_rate() {
        let rate = |b| fsk(b, 1000.0, 2, "aa").rate_hz();
        assert_eq!(rate(4800), 48_000);
        assert_eq!(rate(9600), 240_000);
        assert_eq!(rate(100_000), 2_400_000);
    }

    #[test]
    fn validate_rejects_bad_order_and_bits() {
        assert_eq!(
            fsk(4800, 2400.0, 3, "aa").validate(),
            Err("order: 2 or 4".into())
        );
        for bits in ["a", "zz", "", "pattern:", "pattern:abc"] {
            assert_eq!(
                fsk(4800, 2400.0, 2, bits).validate(),
                Err("bits: hex or pattern:<hex>".into()),
                "{bits}"
            );
        }
        assert!(fsk(4800, 2400.0, 4, "pattern:1b").validate().is_ok());
    }

    #[test]
    fn rs41_family_is_bounded_and_one_frame_a_second() {
        let parse = |extra: serde_json::Value| {
            let mut v = serde_json::json!({"family": "rs41", "serial": "S1234567",
                "lat": -27.4, "lon": 153.1, "alt_m": 12000, "vel_v": 5.0, "frames": 2});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            parse_params(v.as_object().unwrap().clone()).map(|(s, _)| s)
        };
        let cases = [
            (serde_json::json!({"frames": 0}), "frames: 1..=60"),
            (serde_json::json!({"frames": 61}), "frames: 1..=60"),
            (
                serde_json::json!({"serial": "s1234567"}),
                "serial: 8 characters of A-Z and 0-9",
            ),
            (
                serde_json::json!({"serial": "S123456"}),
                "serial: 8 characters of A-Z and 0-9",
            ),
            (serde_json::json!({"lat": 91.0}), "lat: -90..=90"),
            (serde_json::json!({"lon": -181.0}), "lon: -180..=180"),
            (serde_json::json!({"alt_m": 70000}), "alt_m: -1000..=60000"),
            (serde_json::json!({"vel_v": 500.0}), "vel_v: -100..=100"),
        ];
        for (extra, err) in cases {
            assert_eq!(parse(extra.clone()), Err(err.into()), "{extra}");
        }
        let spec = parse(serde_json::json!({})).unwrap();
        assert_eq!(spec.rate_hz(), 48_000);
        let iq = synthesise(&spec, &Impairments::default(), 48_000, 403_000_000);
        // Two frames, each padded with silence to a second; a frame is 2880 bits at
        // 4800 baud, 0.6 s.
        assert_eq!(iq.len(), 96_000);
        assert!(iq[28_000].norm() > 0.99 && iq[30_000].norm() == 0.0);
        assert!(iq[48_000].norm() > 0.99);
    }

    #[test]
    fn m20_family_is_bounded_and_one_frame_a_second() {
        let parse = |extra: serde_json::Value| {
            let mut v = serde_json::json!({"family": "m20", "serial": "M20-611-2-01234",
                "lat": -27.3842, "lon": 153.1175, "alt_m": 5000, "vel_v": 4.5, "frames": 3});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            parse_params(v.as_object().unwrap().clone()).map(|(s, _)| s)
        };
        let serial_err = "serial: M20-YMM-S-NNNNN (Y 0-9, MM 01-12, S 1-8, NNNNN 0-8191)";
        let cases = [
            (serde_json::json!({"frames": 0}), "frames: 1..=60"),
            (serde_json::json!({"frames": 61}), "frames: 1..=60"),
            (serde_json::json!({"serial": "M20-611201234"}), serial_err),
            (serde_json::json!({"serial": "M20-611-2-09999"}), serial_err),
            (serde_json::json!({"lat": 91.0}), "lat: -90..=90"),
            (serde_json::json!({"lon": -181.0}), "lon: -180..=180"),
            (serde_json::json!({"alt_m": 70000}), "alt_m: -1000..=60000"),
            (serde_json::json!({"vel_v": 500.0}), "vel_v: -100..=100"),
        ];
        for (extra, err) in cases {
            assert_eq!(parse(extra.clone()), Err(err.into()), "{extra}");
        }
        let spec = parse(serde_json::json!({})).unwrap();
        assert_eq!(spec.rate_hz(), 48_000);
        let iq = synthesise(&spec, &Impairments::default(), 48_000, 403_000_000);
        // One frame a second: each second is 9600 half-symbols at 5 samples each, the
        // line idling between frames, so the carrier never stops.
        assert_eq!(iq.len(), 3 * 48_000);
        assert!(iq.iter().all(|s| (s.norm() - 1.0).abs() < 1e-3));
    }

    #[test]
    fn dfm_family_is_bounded_and_at_the_line_rate() {
        let parse = |extra: serde_json::Value| {
            let mut v = serde_json::json!({"family": "dfm", "serial": "DFM-19123456",
                "lat": -27.3842, "lon": 153.1175, "alt_m": 5000, "vel_v": 4.5, "frames": 3});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            parse_params(v.as_object().unwrap().clone()).map(|(s, _)| s)
        };
        let cases = [
            (serde_json::json!({"frames": 0}), "frames: 1..=60"),
            (serde_json::json!({"frames": 61}), "frames: 1..=60"),
            (
                serde_json::json!({"serial": "19123456"}),
                "serial: DFM- and a decimal number under 2^32",
            ),
            (
                serde_json::json!({"serial": "DFM-99999999999"}),
                "serial: DFM- and a decimal number under 2^32",
            ),
            (serde_json::json!({"lat": 91.0}), "lat: -90..=90"),
            (serde_json::json!({"lon": -181.0}), "lon: -180..=180"),
            (serde_json::json!({"alt_m": 70000}), "alt_m: -1000..=60000"),
            (serde_json::json!({"vel_v": 500.0}), "vel_v: -100..=100"),
        ];
        for (extra, err) in cases {
            assert_eq!(parse(extra.clone()), Err(err.into()), "{extra}");
        }
        let spec = parse(serde_json::json!({})).unwrap();
        assert_eq!(spec.rate_hz(), 48_000);
        let iq = synthesise(&spec, &Impairments::default(), 48_000, 403_000_000);
        // Three frames back to back, 224 ms each (560 half-symbols at 2500 baud), with no
        // gap: 3 x 10 752 samples at 48 kHz.
        assert_eq!(iq.len(), 3 * 10_752);
        assert!(iq.iter().all(|s| (s.norm() - 1.0).abs() < 1e-3));
    }

    #[test]
    fn gap_ms_spaces_the_bursts() {
        let spec = SynthSpec::Fsk {
            symbol_rate_baud: 4800,
            deviation_hz: 4000.0,
            order: 2,
            bits: "ff00".into(),
            repeat: 3,
            gap_ms: 995,
        };
        let iq = synthesise(&spec, &Impairments::default(), 48_000, 0);
        // 3 bursts of 16 bits at 4800 Bd = 3.33 ms each, two gaps of 995 ms
        let burst = 16 * 48_000 / 4800;
        assert_eq!(iq.len(), 3 * burst + 2 * 995 * 48);
        assert!(iq[burst + 48 * 500].norm() == 0.0, "the gap is silent");
    }

    #[test]
    fn gap_ms_defaults_to_50_and_is_bounded() {
        let parse = |v: serde_json::Value| parse_params(v.as_object().unwrap().clone());
        let (spec, _) = parse(
            serde_json::json!({"family":"ook","symbol_rate_baud":1200,"bits":"a5","repeat":2}),
        )
        .unwrap();
        assert!(matches!(spec, SynthSpec::Ook { gap_ms: 50, .. }));
        let err = parse(serde_json::json!({"family":"ook","symbol_rate_baud":1200,"bits":"a5","repeat":2,"gap_ms":60001})).unwrap_err();
        assert!(err.starts_with("gap_ms: "), "{err}");
    }

    #[test]
    fn bounds_are_errors_naming_the_field() {
        let base = |extra: serde_json::Value| {
            let mut v = serde_json::json!({"family": "fsk", "symbol_rate_baud": 4800,
                "deviation_hz": 2400.0, "order": 2, "bits": "aa", "repeat": 1});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            parse_params(v.as_object().unwrap().clone()).map(|_| ())
        };
        let long = "a".repeat(65_538);
        let cases = [
            (serde_json::json!({"repeat": 0}), "repeat: 1..=100"),
            (serde_json::json!({"repeat": 101}), "repeat: 1..=100"),
            (
                serde_json::json!({"symbol_rate_baud": 0}),
                "symbol_rate_baud: 1..=600000",
            ),
            (
                serde_json::json!({"symbol_rate_baud": 600_001}),
                "symbol_rate_baud: 1..=600000",
            ),
            (
                serde_json::json!({"deviation_hz": 0.0}),
                "deviation_hz: finite, 0 < d < rate/2",
            ),
            (
                serde_json::json!({"deviation_hz": 24_000.0}),
                "deviation_hz: finite, 0 < d < rate/2",
            ),
            (
                serde_json::json!({"echo": {"delay_us": 0, "gain_db": -6.0}}),
                "echo.delay_us: 1..=100000",
            ),
            (
                serde_json::json!({"echo": {"delay_us": 100_001, "gain_db": -6.0}}),
                "echo.delay_us: 1..=100000",
            ),
            (
                serde_json::json!({"echo": {"delay_us": 10, "gain_db": -6.0}}),
                "echo.delay_us: at least one sample at 48000 Hz",
            ),
            (
                serde_json::json!({"echo": {"delay_us": 100, "gain_db": 3.0}}),
                "echo.gain_db: -60..=0",
            ),
            (
                serde_json::json!({"echo": {"delay_us": 100, "gain_db": -61.0}}),
                "echo.gain_db: -60..=0",
            ),
            (
                serde_json::json!({"bits": long}),
                "bits: at most 65536 hex characters",
            ),
        ];
        for (extra, want) in cases {
            assert_eq!(base(extra.clone()), Err(want.into()), "{extra}");
        }
        assert!(base(serde_json::json!({"echo": {"delay_us": 100, "gain_db": -6.0}})).is_ok());
    }

    #[test]
    fn parse_params_takes_the_echo() {
        let v = serde_json::json!({"family": "ook", "symbol_rate_baud": 1000,
            "bits": "80", "repeat": 2, "echo": {"delay_us": 100, "gain_db": -6.0}});
        let (spec, imp) = parse_params(v.as_object().unwrap().clone()).unwrap();
        assert!(matches!(spec, SynthSpec::Ook { repeat: 2, .. }));
        assert_eq!(imp.echo.unwrap().delay_us, 100);
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

    fn framing_json() -> serde_json::Value {
        serde_json::json!({"family": "framing", "modulation": "fsk", "symbol_rate_baud": 4800,
            "deviation_hz": 2400.0, "frames": 8, "body_bytes": 32,
            "framing": {"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272},
                "crc": "crc16-ccitt"}, "repeat": 1})
    }

    #[test]
    fn the_framing_family_parses_and_plants_its_frames() {
        let (spec, _) = parse_params(framing_json().as_object().unwrap().clone()).unwrap();
        let bits = framing_bits(&spec).expect("a framing spec");
        assert_eq!(bits.len(), 8 * (16 + 272));
        let SynthSpec::Framing { framing, .. } = &spec else {
            panic!("the framing family")
        };
        let (frames, _) = crate::framing::decode_frames(&bits, framing);
        assert_eq!(frames.len(), 8);
        assert!(frames.iter().all(|f| f.crc_ok == Some(true)));
    }

    #[test]
    fn the_framing_family_refuses_bodies_that_cannot_decode() {
        // 272 bits is 34 B: a 30-byte body and its CRC make 32 B.
        let mut v = framing_json();
        v["body_bytes"] = 30.into();
        let e = parse_params(v.as_object().unwrap().clone()).unwrap_err();
        assert!(e.contains("length.fixed_bits"), "{e}");
        // A body over what a frame event may hold.
        let mut v = framing_json();
        let over = crate::framing::MAX_FRAME_BITS / 8 + 1;
        v["framing"]["length"] = serde_json::json!({ "fixed_bits": 272 });
        v["frames"] = 1.into();
        v["body_bytes"] = over.into();
        assert!(parse_params(v.as_object().unwrap().clone()).is_err());
    }

    #[test]
    fn the_framing_family_refuses_a_bad_framing_naming_the_field() {
        let mut v = framing_json();
        v["framing"]["sync"]["bits"] = 70.into();
        let e = parse_params(v.as_object().unwrap().clone()).unwrap_err();
        assert!(e.starts_with("framing.sync.bits"), "{e}");
        for key in ["frames", "body_bytes"] {
            let mut v = framing_json();
            v[key] = 0.into();
            assert!(
                parse_params(v.as_object().unwrap().clone()).is_err(),
                "{key}"
            );
        }
        let mut v = framing_json();
        v["bodies"] = serde_json::json!(["zz"]);
        assert!(parse_params(v.as_object().unwrap().clone()).is_err());
        let mut v = framing_json();
        v["deviation_hz"] = 0.0.into();
        assert!(parse_params(v.as_object().unwrap().clone()).is_err());
    }

    fn ais_json() -> serde_json::Value {
        serde_json::json!({
            "family": "ais", "mmsi": 503000001, "lat": -27.3842, "lon": 153.1175,
            "sog_kt": 12.3, "cog_deg": 45.0, "name": "ZETA TEST", "repeat": 3
        })
    }

    #[test]
    fn ais_family_validates_and_is_48_khz() {
        let parse = |v: serde_json::Value| parse_params(v.as_object().unwrap().clone());
        let (spec, _) = parse(ais_json()).unwrap();
        assert_eq!(spec.rate_hz(), 48_000);
        let bad = |key: &str, val: serde_json::Value| {
            let mut v = ais_json();
            v[key] = val;
            parse(v).unwrap_err()
        };
        assert!(bad("mmsi", 1.into()).contains("mmsi"));
        assert!(bad("sog_kt", 150.0.into()).contains("sog_kt"));
        assert!(bad("name", "x".repeat(21).into()).contains("name"));
        assert!(bad("name", "zeta".into()).contains("name"));
        assert!(bad("lat", 91.0.into()).contains("lat"));
        assert!(bad("lon", (-181.0).into()).contains("lon"));
        assert!(bad("cog_deg", 360.0.into()).contains("cog_deg"));
        assert!(bad("repeat", 0.into()).contains("repeat"));
        let mut v = ais_json();
        v.as_object_mut().unwrap().remove("name");
        assert!(parse(v).is_ok());
    }

    #[test]
    fn ais_frames_deframe_to_their_parameters() {
        // The bit-order self-check: the chain `ais_decode` runs.
        let (spec, imp) = parse_params(ais_json().as_object().unwrap().clone()).unwrap();
        let iq = synthesise(&spec, &imp, 48_000, 161_975_000);
        let mut prev = Complex::new(0.0, 0.0);
        let audio = crate::nbfm::nbfm_demod(&iq, 48_000, 2400, &mut prev);
        let line = crate::pocsag::BitSync::new(48_000, 9600).feed(&audio);
        let mut last = 0u8;
        let bits: Vec<u8> = line
            .iter()
            .map(|&b| {
                let d = u8::from(b == last);
                last = b;
                d
            })
            .collect();
        let frames = crate::ax25::HdlcDeframer::new().feed(&bits);
        assert!(!frames.is_empty(), "no frame deframed");
        let msgs: Vec<_> = frames
            .iter()
            .filter_map(|f| crate::ais_msg::decode(&crate::ais_msg::hdlc_bytes_to_payload(f)))
            .collect();
        let pos = msgs.iter().find(|m| m.msg_type == 1).expect("a type 1");
        assert_eq!(pos.mmsi, 503_000_001);
        assert!((pos.lat.unwrap() - -27.3842).abs() < 1e-4, "{:?}", pos.lat);
        assert!((pos.lon.unwrap() - 153.1175).abs() < 1e-4, "{:?}", pos.lon);
        assert_eq!(
            msgs.iter().filter(|m| m.msg_type == 1).count(),
            3,
            "{msgs:?}"
        );
        let st = msgs.iter().find(|m| m.msg_type == 5).expect("a type 5");
        assert_eq!(st.name.as_deref(), Some("ZETA TEST"));
    }

    fn flex_json() -> serde_json::Value {
        serde_json::json!({
            "family": "flex", "capcode": 1234567, "text": "HELLO ZETA", "baud": 1600, "repeat": 2
        })
    }

    #[test]
    fn flex_family_refuses_other_rates() {
        let parse = |v: serde_json::Value| parse_params(v.as_object().unwrap().clone());
        let bad = |key: &str, val: serde_json::Value| {
            let mut v = flex_json();
            v[key] = val;
            parse(v).unwrap_err()
        };
        for baud in [3200, 6400, 1200] {
            assert_eq!(
                bad("baud", baud.into()),
                "baud: this wave synthesises 1600 bps"
            );
        }
        assert_eq!(
            bad("capcode", 0.into()),
            "capcode: 1..=1933312 (a short address)"
        );
        assert_eq!(
            bad("capcode", 1_933_313.into()),
            "capcode: 1..=1933312 (a short address)"
        );
        let text_err = "text: at most 251 characters of printable ASCII";
        assert_eq!(bad("text", "x".repeat(252).into()), text_err);
        assert_eq!(bad("text", "caf\u{e9}".into()), text_err);
        assert_eq!(bad("text", "a\nb".into()), text_err);
        assert!(
            parse({
                let mut v = flex_json();
                v["text"] = "x".repeat(251).into();
                v
            })
            .is_ok()
        );
        assert_eq!(bad("repeat", 0.into()), "repeat: 1..=10");
        assert_eq!(bad("repeat", 11.into()), "repeat: 1..=10");
    }

    #[test]
    fn flex_family_is_48_khz_and_frames_back_to_back() {
        let (spec, imp) = parse_params(flex_json().as_object().unwrap().clone()).unwrap();
        assert_eq!(spec.rate_hz(), 48_000);
        let iq = synthesise(&spec, &imp, 48_000, 152_000_000);
        // Two 1.875 s frames, the carrier never stopping.
        assert_eq!(iq.len(), 2 * 90_000);
        assert!(iq.iter().all(|s| (s.norm() - 1.0).abs() < 1e-3));
        // 2-FSK: the tones sit at the deviation either side.
        let n = 1 << 16;
        let (p, bin) = spectrum(&iq, 48_000, n);
        let (neg, pos) = (peak_hz(&p, bin, 0, n / 2), peak_hz(&p, bin, n / 2, n));
        assert!((pos - FLEX_DEVIATION_HZ).abs() <= 2.0 * bin, "{pos}");
        assert!((neg + FLEX_DEVIATION_HZ).abs() <= 2.0 * bin, "{neg}");
    }

    #[test]
    fn pocsag_words_come_from_the_shared_bch_encoder() {
        // The POCSAG encoder's address word is the shared BCH(31,21) word.
        let bits = pocsag_encode(&[(8, 3, "")], 1200);
        let word = |n: usize| {
            bits[576 + 32 * n..576 + 32 * (n + 1)]
                .iter()
                .fold(0u32, |a, &b| (a << 1) | b as u32)
        };
        assert_eq!(word(1), crate::bch::bch3121_word((1 << 2) | 3));
    }

    fn m17_json() -> serde_json::Value {
        serde_json::json!({
            "family": "m17", "src": "VK4ZET", "dst": "ALL", "can": 0, "frames": 25
        })
    }

    #[test]
    fn m17_family_validates() {
        let parse = |v: serde_json::Value| parse_params(v.as_object().unwrap().clone());
        let bad = |key: &str, val: serde_json::Value| {
            let mut v = m17_json();
            v[key] = val;
            parse(v).unwrap_err()
        };
        let (spec, _) = parse(m17_json()).unwrap();
        assert_eq!(spec.rate_hz(), 48_000);
        assert!(bad("src", "VK4ZETABCD".into()).starts_with("src:"));
        assert!(bad("src", "vk4zet".into()).starts_with("src:"));
        assert!(bad("src", "ALL".into()).starts_with("src:"));
        assert!(bad("dst", "TOO-LONG-1".into()).starts_with("dst:"));
        assert_eq!(bad("frames", 0.into()), "frames: 1..=250");
        assert_eq!(bad("frames", 251.into()), "frames: 1..=250");
        assert_eq!(bad("can", 16.into()), "can: 0..=15");
        assert!(
            parse({
                let mut v = m17_json();
                v["dst"] = "M17-M17 C".into();
                v["can"] = 15.into();
                v["frames"] = 250.into();
                v
            })
            .is_ok()
        );
    }

    #[test]
    fn m17_iq_has_the_expected_length() {
        let (spec, imp) = parse_params(m17_json().as_object().unwrap().clone()).unwrap();
        let iq = synthesise(&spec, &imp, 48_000, 145_000_000);
        // 40 ms of preamble, the LSF and 25 stream frames at 40 ms each, 40 ms of EOT.
        let sps = 48_000 / M17_BAUD as usize;
        let want = (1 + (25 + 1) + 1) * 48_000 * 40 / 1000;
        assert!(iq.len().abs_diff(want) <= sps, "{} vs {want}", iq.len());
        assert!(iq.iter().all(|s| (s.norm() - 1.0).abs() < 1e-3));
    }

    #[test]
    fn rrc_4fsk_puts_a_symbol_run_on_its_tone() {
        // A long run of each symbol settles on its frequency: -3d, -d, +d, +3d. Averaged
        // over a symbol period it is exact; within the period the pulse's truncation to 8
        // symbols (the specification's 81 taps at 10 samples a symbol) leaves a ripple
        // under 1 % of the deviation.
        let rate = 48_000;
        let hz = |a: Complex<f32>, b: Complex<f32>| {
            (b * a.conj()).arg() as f64 * f64::from(rate) / (2.0 * PI)
        };
        for (sym, want) in [(0u8, -2400.0), (1, -800.0), (2, 800.0), (3, 2400.0)] {
            let iq = rrc_modulate_4fsk(&[sym; 48], M17_BAUD, M17_DEVIATION_HZ, rate, M17_RRC_ALPHA);
            assert_eq!(iq.len(), 480);
            let sps = (rate / M17_BAUD) as usize;
            let inst: Vec<f64> = (240..240 + sps).map(|i| hz(iq[i], iq[i + 1])).collect();
            let mean = inst.iter().sum::<f64>() / sps as f64;
            assert!((mean - want).abs() < 1.0, "symbol {sym}: mean {mean}");
            for f in inst {
                assert!(
                    (f - want).abs() < 0.01 * M17_DEVIATION_HZ,
                    "symbol {sym}: {f}"
                );
            }
        }
    }

    #[test]
    fn m17_iq_demodulates_back_to_its_frames() {
        // A plain receiver: the discriminator, the matched root-raised-cosine filter,
        // a sample at each symbol centre sliced to the nearest level. The LSF decodes
        // with its CRC and every stream frame gives its number and LICH chunk.
        use crate::m17::{
            FRAME_BITS, FRAME_SYMBOLS, LICH_CHUNKS, Lsf, SYNC_STREAM, SYNC_SYMBOLS,
            decode_lsf_frame, decode_stream_frame, symbols_to_bits, word_symbols,
        };
        let frames = 8u32;
        let spec = SynthSpec::M17 {
            src: "VK4ZET".into(),
            dst: "ALL".into(),
            can: 3,
            frames,
        };
        let rate = spec.rate_hz();
        let iq = synthesise(&spec, &Impairments::default(), rate, 0);
        let sps = (rate / M17_BAUD) as usize;
        // Frequency in units of deviation / 3, so the levels sit at -3, -1, +1, +3.
        let scale = f64::from(rate) / (2.0 * PI) * 3.0 / M17_DEVIATION_HZ;
        let mut f = vec![0.0f64; iq.len()];
        for i in 1..iq.len() {
            f[i] = (iq[i] * iq[i - 1].conj()).arg() as f64 * scale;
        }
        let reach = (RRC_SPAN_SYMBOLS / 2.0 * sps as f64) as isize;
        let taps: Vec<f64> = (-reach..=reach)
            .map(|j| rrc(j as f64 / sps as f64, M17_RRC_ALPHA))
            .collect();
        let gain: f64 = taps.iter().sum();
        let symbols: Vec<u8> = (0..iq.len() / sps)
            .map(|k| {
                let c = (k * sps + sps / 2) as isize;
                let y: f64 = taps
                    .iter()
                    .enumerate()
                    .map(|(n, h)| {
                        let i = c + n as isize - reach;
                        h * f.get(i.max(0) as usize).copied().unwrap_or(0.0)
                    })
                    .sum::<f64>()
                    / gain;
                ((y + 3.0) / 2.0).round().clamp(0.0, 3.0) as u8
            })
            .collect();
        let frame_bits = |at: usize| -> [u8; FRAME_BITS] {
            symbols_to_bits(&symbols[at + SYNC_SYMBOLS..at + FRAME_SYMBOLS])
                .try_into()
                .unwrap()
        };
        let lsf =
            Lsf::parse(&decode_lsf_frame(&frame_bits(FRAME_SYMBOLS)).expect("the LSF")).unwrap();
        assert_eq!(
            (lsf.src.as_str(), lsf.dst.as_str(), lsf.can),
            ("VK4ZET", "ALL", 3)
        );
        for n in 0..frames as usize {
            let at = (2 + n) * FRAME_SYMBOLS;
            assert_eq!(
                symbols[at..at + SYNC_SYMBOLS],
                word_symbols(SYNC_STREAM),
                "{n}"
            );
            let frame = decode_stream_frame(&frame_bits(at));
            assert_eq!(usize::from(frame.frame_number), n);
            assert_eq!(frame.last, n + 1 == frames as usize);
            assert_eq!(frame.lich.unwrap().1 as usize, n % LICH_CHUNKS);
            assert_eq!(frame.payload, [0; 16]);
        }
    }
}
