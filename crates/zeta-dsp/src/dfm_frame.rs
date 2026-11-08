//! Graw DFM radiosonde frame generator: builds the 280-bit frames a DFM-06/09/17 sends
//! back to back, for the `dfm` synthesiser family (`zeta modes check dfm_decode`).
//!
//! Written from public descriptions, not from any decoder's source: J. Frielingsdorf's
//! master thesis (TH Köln, 2021, "Redesign and Performance Evaluation of a Narrow-Band FM
//! Demodulator and Decoder for Radiosonde Telemetry", section 3.5 "Graw DFM Telemetry",
//! published at <https://gist.github.com/bazjo/ae6edb27840015d92416a622c4dc305a>), the
//! recessim wiki's DFM-17 page (GFSK at 1250 bit/s) and the radiosonde_auto_rx wiki
//! (the id is only known once the serial has been seen; until then the decoder prints a
//! placeholder). `dfm09mod` (sonde-decoders 1.8.1) served only as the oracle, run as a
//! black box on audio this module's conventions produced, with its `-r`, `-r --ecc`,
//! `-vv`, `--sat` and `--dbg` views.
//!
//! From the thesis:
//! - **Line**: 2-FSK at 2500 baud, 4 kHz deviation, Manchester coded (1250 bit/s); the
//!   frames run continuously, 224 ms each, with no gap and no preamble.
//! - **Frame**: a 2-byte header `0x45CF`, a 7-byte sensor field (the configuration
//!   block), two 13-byte data fields. Each field byte is one codeword of an extended
//!   systematic Hamming (8,4) code whose first four bits are the message nibble.
//! - **Interleaving**: per field, the most significant bit of every codeword is sent
//!   first, then the next bit of every codeword, down to the least significant bits.
//! - **Data set**: the sensor fields carry one measurement each under a 4-bit sensor
//!   id; the GNSS data set is 8-9 data fields over 4-5 frames, one set a second on average.
//!
//! What the descriptions leave open, and how each was settled:
//! - **Manchester phase and polarity** (oracle): a 1 bit is sent as the half-symbols
//!   `0 1` and a 0 as `1 0` (the thesis's "inverted Biphase-L"), a 1 half-symbol on the
//!   higher tone; `dfm09mod` without `-i` decodes exactly these fields (`-r`).
//! - **The header as sent** (thesis plus oracle): `0x45CF` most significant bit first,
//!   Manchester coded like the fields.
//! - **The Hamming parity bits** (oracle): codeword `d0 d1 d2 d3 p0 p1 p2 p3`, `d0` the
//!   nibble's most significant bit, each `p_i` the XOR of the three data bits other than
//!   `d_i`; `dfm09mod -r --ecc` reports every field `[OK]`.
//! - **Field layout** (oracle): the sensor field is the sensor id nibble, then 24 data
//!   bits (`--dbg` shows sensor `i` as `f<i>`: a 4-bit exponent `e` and a 20-bit
//!   mantissa `m`, the value `m / 2^e`); a data field is 48 data bits, then the field id
//!   nibble.
//! - **The GNSS data fields** (oracle, `-vv` and `--sat`): field 0 carries the frame
//!   counter in its fourth byte (the decoder prints a position each time it changes);
//!   field 1 a 32-bit satellite (PRN) mask and the UTC milliseconds of the minute;
//!   field 2 the latitude (i32, 1e-7 degree) and the horizontal speed (u16, cm/s);
//!   field 3 the longitude and the heading (u16, 0.01 degree); field 4 the altitude
//!   (i32, cm) and the climb (i16, cm/s); field 8 the date and time to the minute (year
//!   12 bits, month 4, day 5, hour 5, minute 6) and the satellites used (fifth byte).
//!   The decoder prints the position on the frame that carries field 8, and a JSON line
//!   (`--json`) only once the time has moved on between two of them. Fields 5-7 and the
//!   filler field 15 stay zero here.
//!
//! **Not settled: the serial.** Where the configuration block carries the serial was not
//! found. This module puts it on sensor channel [`SN_CHANNEL`] (0xA, after the
//! measurement channels 0-9; the channel auto_rx associates with a DFM-09), the 32-bit
//! number in two 16-bit halves, each as `half << 8 | index` (index 0 the high half, 1 the
//! low one). Through the row's own chain `dfm09mod --json --ecc` reports these frames'
//! position, climb, time and satellites, but its id stays the placeholder
//! `DFM-xxxxxxxx`, as it did for every other serial channel (4-15), half layout, sensor
//! sequence (sequential, a fixed set plus one alternating slot, each held for a GNSS
//! cycle) and run length tried. So the row's `test`, which expects the id, stays null
//! (see the TODO).

/// The frame header, before Manchester coding.
pub const SYNC: u16 = 0x45CF;
/// Manchester half-symbols a second.
pub const BAUD: u32 = 2500;
/// The FSK deviation, Hz (thesis, table 3.2).
pub const DEVIATION_HZ: f64 = 4000.0;
/// Bits in a frame before Manchester coding: the header, 7 + 13 + 13 codewords.
pub const FRAME_BITS: usize = 16 + 8 * (CONF_NIBBLES + 2 * DAT_NIBBLES);
/// A frame's length in milliseconds (`2 * FRAME_BITS` half-symbols at [`BAUD`]).
pub const FRAME_MS: u32 = 224;
/// Nibbles in the configuration (sensor) field: the id and 24 data bits.
pub const CONF_NIBBLES: usize = 7;
/// Nibbles in a data field: 48 data bits and the id.
pub const DAT_NIBBLES: usize = 13;
/// The sensor channel carrying the serial (unconfirmed, see the module comment).
pub const SN_CHANNEL: u8 = 0xA;
/// The GNSS data fields in the order sent, two a frame: one GNSS data set every
/// `DAT_CYCLE.len() / 2` frames.
pub const DAT_CYCLE: [u8; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, FILLER_DAT];
/// A data field id the decoder ignores, filling the last frame of a GNSS cycle.
const FILLER_DAT: u8 = 0xF;
/// The sensor value every measurement channel reports: 1.0 (mantissa 2, exponent 1).
const SENSOR_ONE: u32 = 0x10_0002;
/// The fixed date of the synthesised flight: 2026-11-08 12:00 UTC.
const DATE: (u32, u32, u32, u32, u32) = (2026, 11, 8, 12, 0);
/// Satellites used, as field 8 reports them.
const SATS: u8 = 8;
/// The satellite mask field 1 reports: PRNs 1, 3, 7, 11, 14, 17, 19 and 22.
const PRN_MASK: u32 =
    (1 << 31) | (1 << 29) | (1 << 25) | (1 << 21) | (1 << 18) | (1 << 15) | (1 << 13) | (1 << 10);
/// The horizontal speed and heading the generator reports: 5 m/s towards 90 degrees.
const VEL_H_CM_S: u16 = 500;
const HEADING_CENTIDEG: u16 = 9000;

/// What a frame carries: the serial as `dfm09mod` would print the id (`DFM-` and the
/// decimal number), the position (degrees, metres), the climb in m/s and the frame's
/// index in the transmission (which data fields and which sensor channel it carries).
#[derive(Clone, Debug, PartialEq)]
pub struct DfmParams {
    pub serial: String,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
    pub vel_v: f64,
    pub frame: u32,
}

/// The serial's number: the digits after `DFM-` (or the whole string), 0 when unreadable.
pub fn serial_number(serial: &str) -> u32 {
    serial
        .strip_prefix("DFM-")
        .unwrap_or(serial)
        .parse()
        .unwrap_or(0)
}

/// The extended Hamming (8,4) codeword of `nibble`'s low four bits, `d0 d1 d2 d3 p0 p1 p2
/// p3` from the most significant bit down, `d0` the nibble's most significant bit and each
/// parity bit `p_i` the XOR of the data bits other than `d_i`.
pub fn hamming84_encode(nibble: u8) -> u8 {
    let d = |i: u8| (nibble >> (3 - i)) & 1;
    let all = d(0) ^ d(1) ^ d(2) ^ d(3);
    let parity = (0..4).fold(0u8, |acc, i| (acc << 1) | (all ^ d(i)));
    ((nibble & 0xF) << 4) | parity
}

/// Manchester coding as DFM sends it: each bit becomes two half-symbols, a 1 `0 1` and a
/// 0 `1 0`.
pub fn manchester_encode(bits: &[u8]) -> Vec<u8> {
    bits.iter()
        .flat_map(|&b| if b & 1 == 1 { [0, 1] } else { [1, 0] })
        .collect()
}

/// One field's bits as sent: each nibble a Hamming codeword, then the codewords
/// interleaved, the most significant bit of every codeword first.
fn field_bits(nibbles: &[u8]) -> Vec<u8> {
    let words: Vec<u8> = nibbles.iter().map(|&n| hamming84_encode(n)).collect();
    (0..8)
        .flat_map(|k| words.iter().map(move |w| (w >> (7 - k)) & 1))
        .collect()
}

/// `value`'s low `4 * n` bits as `n` nibbles, most significant first.
fn nibbles(value: u64, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| ((value >> (4 * (n - 1 - i))) & 0xF) as u8)
        .collect()
}

/// The sensor channel frame `frame` carries and its 24-bit value: channels 0 to
/// [`SN_CHANNEL`] in turn, the serial's halves on alternate rounds.
fn conf_field(p: &DfmParams) -> (u8, u32) {
    let round = u32::from(SN_CHANNEL) + 1;
    let channel = (p.frame % round) as u8;
    if channel != SN_CHANNEL {
        return (channel, SENSOR_ONE);
    }
    let index = (p.frame / round) % 2;
    let sn = serial_number(&p.serial);
    let half = if index == 0 { sn >> 16 } else { sn & 0xFFFF };
    (channel, (half << 8) | index)
}

/// Data field `id`'s 48 bits in GNSS cycle `cycle`.
fn dat_field(p: &DfmParams, id: u8, cycle: u32) -> u64 {
    // The UTC time: one GNSS cycle (5 frames) after another from DATE.
    let ms = u64::from(cycle) * u64::from(FRAME_MS) * (DAT_CYCLE.len() as u64 / 2);
    let minute = ms / 60_000;
    let (year, month, day, hour, min0) = DATE;
    let (hour, min) = (
        u64::from(hour) + (u64::from(min0) + minute) / 60,
        (u64::from(min0) + minute) % 60,
    );
    let i32_bits = |v: f64| u64::from(v.round() as i32 as u32);
    match id {
        0 => u64::from(cycle & 0xFF) << 16,
        1 => (u64::from(PRN_MASK) << 16) | (ms % 60_000),
        2 => (i32_bits(p.lat * 1e7) << 16) | u64::from(VEL_H_CM_S),
        3 => (i32_bits(p.lon * 1e7) << 16) | u64::from(HEADING_CENTIDEG),
        4 => (i32_bits(p.alt_m * 100.0) << 16) | u64::from((p.vel_v * 100.0).round() as i16 as u16),
        8 => {
            let date = (u64::from(year) << 20)
                | (u64::from(month) << 16)
                | (u64::from(day) << 11)
                | (hour % 24) << 6
                | min;
            (date << 16) | (u64::from(SATS) << 8)
        }
        _ => 0,
    }
}

/// One frame for `p`, before Manchester coding ([`FRAME_BITS`] bits): the header, the
/// sensor field, the frame's two GNSS data fields.
pub fn dfm_frame_raw(p: &DfmParams) -> Vec<u8> {
    let mut bits: Vec<u8> = (0..16).map(|k| ((SYNC >> (15 - k)) & 1) as u8).collect();
    let (channel, value) = conf_field(p);
    let mut conf = vec![channel];
    conf.extend(nibbles(u64::from(value), CONF_NIBBLES - 1));
    bits.extend(field_bits(&conf));
    let per_cycle = DAT_CYCLE.len() as u32 / 2;
    let (cycle, slot) = (p.frame / per_cycle, (p.frame % per_cycle) as usize);
    for id in &DAT_CYCLE[2 * slot..2 * slot + 2] {
        let mut dat = nibbles(dat_field(p, *id, cycle), DAT_NIBBLES - 1);
        dat.push(*id);
        bits.extend(field_bits(&dat));
    }
    bits
}

/// One frame for `p`, line-coded as sent: `2 * FRAME_BITS` Manchester half-symbols.
pub fn dfm_frame_bits(p: &DfmParams) -> Vec<u8> {
    manchester_encode(&dfm_frame_raw(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(frame: u32) -> DfmParams {
        DfmParams {
            serial: "DFM-19123456".into(),
            lat: -27.3842,
            lon: 153.1175,
            alt_m: 5000.0,
            vel_v: 4.5,
            frame,
        }
    }

    /// The test's own decoder: the nibble whose codeword is nearest to `word` (one flipped
    /// bit away at most for a correctable word).
    fn hamming84_decode(word: u8) -> u8 {
        (0..16u8)
            .min_by_key(|&n| (hamming84_encode(n) ^ word).count_ones())
            .unwrap()
    }

    fn manchester_decode(chips: &[u8]) -> Vec<u8> {
        chips
            .chunks(2)
            .map(|c| match c {
                [0, 1] => 1,
                [1, 0] => 0,
                _ => panic!("not Manchester: {c:?}"),
            })
            .collect()
    }

    /// Undo [`field_bits`]: deinterleave, decode each codeword.
    fn field_nibbles(bits: &[u8]) -> Vec<u8> {
        let n = bits.len() / 8;
        (0..n)
            .map(|i| {
                let word = (0..8).fold(0u8, |w, k| (w << 1) | bits[k * n + i]);
                hamming84_decode(word)
            })
            .collect()
    }

    fn value(nibbles: &[u8]) -> u64 {
        nibbles.iter().fold(0, |v, &n| (v << 4) | u64::from(n))
    }

    #[test]
    fn hamming84_corrects_one_bit() {
        for n in 0..16u8 {
            let word = hamming84_encode(n);
            assert_eq!(word >> 4, n, "systematic: the nibble leads");
            assert_eq!(hamming84_decode(word), n);
            for k in 0..8 {
                assert_eq!(hamming84_decode(word ^ (1 << k)), n, "nibble {n} bit {k}");
            }
        }
        // Distance 4: any two codewords differ in at least four bits.
        for a in 0..16u8 {
            for b in a + 1..16 {
                assert!((hamming84_encode(a) ^ hamming84_encode(b)).count_ones() >= 4);
            }
        }
        // The parity convention the oracle accepts: nibble 1 is 0001 1110.
        assert_eq!(hamming84_encode(1), 0b0001_1110);
    }

    #[test]
    fn manchester_doubles_and_round_trips() {
        let bits = [1, 0, 0, 1, 1, 1, 0];
        let chips = manchester_encode(&bits);
        assert_eq!(chips.len(), 2 * bits.len());
        assert_eq!(&chips[..4], &[0, 1, 1, 0]);
        assert_eq!(manchester_decode(&chips), bits);
    }

    #[test]
    fn a_frame_has_the_documented_layout() {
        let chips = dfm_frame_bits(&params(0));
        assert_eq!(FRAME_BITS, 280);
        assert_eq!(chips.len(), 2 * FRAME_BITS);
        // 560 half-symbols at 2500 baud: 224 ms.
        assert_eq!(chips.len() as u32 * 1000 / BAUD, FRAME_MS);
        let bits = manchester_decode(&chips);
        let sync = bits[..16]
            .iter()
            .fold(0u16, |v, &b| (v << 1) | u16::from(b));
        assert_eq!(sync, 0x45CF);
        let conf = field_nibbles(&bits[16..16 + 56]);
        assert_eq!(conf[0], 0, "frame 0 carries sensor channel 0");
        assert_eq!(value(&conf[1..]), u64::from(SENSOR_ONE));
        let dat1 = field_nibbles(&bits[72..72 + 104]);
        let dat2 = field_nibbles(&bits[176..280]);
        assert_eq!((dat1[12], dat2[12]), (0, 1), "the field id trails");
    }

    #[test]
    fn the_gnss_fields_carry_the_position() {
        let mut fields = std::collections::HashMap::new();
        for frame in 5..10 {
            let bits = manchester_decode(&dfm_frame_bits(&params(frame)));
            for at in [72, 176] {
                let f = field_nibbles(&bits[at..at + 104]);
                fields.insert(f[12], value(&f[..12]));
            }
        }
        let i32_hi = |v: u64| f64::from((v >> 16) as u32 as i32);
        assert_eq!((fields[&0] >> 16) & 0xFF, 1, "the second GNSS cycle");
        assert!((i32_hi(fields[&2]) / 1e7 - -27.3842).abs() < 1e-7);
        assert!((i32_hi(fields[&3]) / 1e7 - 153.1175).abs() < 1e-7);
        assert_eq!(i32_hi(fields[&4]) / 100.0, 5000.0);
        assert_eq!((fields[&4] & 0xFFFF) as u16 as i16, 450);
        assert_eq!(
            fields[&1] & 0xFFFF,
            1120,
            "one cycle (5 frames of 224 ms) on"
        );
        assert_eq!((fields[&8] >> 36) & 0xFFF, 2026);
        assert_eq!((fields[&8] >> 8) & 0xFF, u64::from(SATS));
        assert_eq!(fields[&FILLER_DAT], 0);
    }

    #[test]
    fn the_serial_spreads_over_two_rounds_of_the_sensor_channels() {
        let round = u32::from(SN_CHANNEL) + 1;
        let conf = |frame: u32| {
            let bits = manchester_decode(&dfm_frame_bits(&params(frame)));
            let n = field_nibbles(&bits[16..72]);
            (n[0], value(&n[1..]) as u32)
        };
        let (ch, hi) = conf(round - 1);
        let (ch2, lo) = conf(2 * round - 1);
        assert_eq!((ch, ch2), (SN_CHANNEL, SN_CHANNEL));
        assert_eq!((hi & 0xF, lo & 0xF), (0, 1));
        assert_eq!(((hi >> 8) << 16) | (lo >> 8), 19_123_456);
        assert_eq!(serial_number("DFM-19123456"), 19_123_456);
    }
}
