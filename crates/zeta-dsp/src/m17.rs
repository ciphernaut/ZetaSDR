//! M17 codes: the forward error correction, framing and link setup of the M17 digital
//! voice protocol, for the `m17` synthesiser family and the `m17_decode` stage.
//!
//! Written from the M17 Protocol Specification (the M17 Project, revision 2.0.10 of
//! 2026-10-16, <https://github.com/M17-Project/M17_spec>, `M17_spec.tex`; the document is
//! under the GNU FDL 1.3). Only its prose and tables were used: no M17 implementation's
//! source was read (libm17 and m17-cxx-demod are GPL), nor the C examples of its address
//! appendix. Every constant below is the specification's, from the section named:
//!
//! - **Symbols** (Physical Layer, "Dibit, Symbol, and Frequency-shift"): 4FSK at 4800
//!   symbols/s, dibits most significant bit first, `01` → +3, `00` → +1, `10` → -1,
//!   `11` → -3 (+2.4, +0.8, -0.8, -2.4 kHz). Here a symbol is its level's index `0..=3`
//!   for -3, -1, +1, +3, as [`crate::synth::gfsk_modulate`] and
//!   [`crate::synth::rrc_modulate_4fsk`] take them. Pulses are root-raised-cosine,
//!   alpha 0.5, spanning at least 8 symbols.
//! - **Transmission**: a 40 ms preamble (192 symbols of +3, -3 before an LSF: the byte
//!   `0x77` repeated), the frames, then a 40 ms EOT marker (`0x555D` repeated). A frame is
//!   a 16-bit sync burst and 368 payload bits, 40 ms. Sync bursts: LSF `0x55F7`, BERT
//!   `0xDF55`, stream `0xFF5D`, packet `0x75FF`.
//! - **Randomizer**: payload bits XORed with the 46-byte sequence of the "Randomizer
//!   Sequence" appendix, bit 7 of byte 0 first ([`decorrelate`]).
//! - **Interleaver** (appendix "Interleaving"): the quadratic permutation polynomial
//!   `pi(x) = (45x + 92x^2) mod 368`; the appendix's table maps input index `x` to output
//!   index `pi(x)`. The permutation is its own inverse.
//! - **Convolutional code** (appendix "Convolutional Encoder"): rate 1/2, K = 5,
//!   `G1(D) = 1 + D^3 + D^4` (`0x19`), `G2(D) = 1 + D + D^2 + D^4` (`0x17`), outputs read
//!   alternately G1 then G2, 4 zero tail bits appended.
//! - **Puncturing** (appendix "Code Puncturing"): P1 (61 entries, 46 ones: a leading 1
//!   then `1 0 1 1` fifteen times) for the LSF, 488 → 368; P2 (12 entries, the last 0)
//!   for stream contents, 296 → 272; P3 (8 entries, the last 0) for packet frames.
//! - **Golay(24,12)** (appendix "Golay Encoder"): systematic, the data in bits 23..12,
//!   then the 12 columns of `P` from the specification's generator matrix `G = [I | P]`
//!   (its rows are [`GOLAY_P`]; the first is `0xC75`, the generator polynomial). The code
//!   has distance 8, so [`golay24_decode`] corrects 3 errors and detects 4.
//! - **CRC** ("CRC"): [`crate::crc::CRC16_M17`], poly `0x5935`, init `0xFFFF`, over the
//!   28 LSD bytes, stored big endian; a whole valid LSF checks to zero.
//! - **LSF** ("Link Setup", "LSF", "Stream Mode: TYPE Field"): DST (6), SRC (6), TYPE (2),
//!   META (14), CRC (2), big endian. TYPE bit 0 packet/stream, bits 1-2 data type
//!   (`01` data, `10` voice, `11` voice + data), bits 3-4 encryption type (`00` none, `01`
//!   scrambler, `10` AES, `11` other), bits 5-6 the encryption subtype, bits 7-10 the
//!   channel access number, bit 11 signed stream (not modelled), the rest reserved.
//!   Encoded: 240 bits + 4 tail, convolutional, P1, interleaved, randomized.
//! - **Stream frames** ("Stream Frames"): the LICH (a 40-bit chunk of the LSF, chunk
//!   `n` being LSF bits `40n..40n+40`, then the 3-bit LICH counter in the top bits of
//!   byte 5) as four Golay words (96 bits), then the frame number (16 bits, its top bit
//!   set in the last frame) and 128 bits of stream data, 144 bits + 4 tail, convolutional
//!   and P2 (272 bits). The 368 bits are interleaved and randomized.
//! - **Addresses** (appendix "Address Encoding"): base 40 over
//!   ` ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-/.`, the first character in the least
//!   significant digit, at most nine characters, big endian in 6 bytes; `0` is reserved,
//!   `0xEE6B28000000..=0xFFFFFFFFFFFE` is the extended range left to applications and
//!   `0xFFFFFFFFFFFF` is BROADCAST (a destination only).
//! - **GNSS META** ("GNSS Data", encryption type none with subtype `01`): byte 0 data
//!   source and station type, byte 1 the validity nibble (`1000` position valid), the
//!   radius and the bearing's top bit, byte 2 the bearing, bytes 3-5 latitude and 6-8
//!   longitude as 24-bit two's complement fractions of 90 and 180 degrees
//!   (±8388607 = ±90/±180), then altitude, speed and reserved bits.
//!
//! Spellings that are ours, not the specification's: BROADCAST reads as `ALL` (and `ALL`
//! encodes to it, so the standard-range address of the letters `ALL` is never written);
//! an extended-range address reads as `#` and the base-40 digits of its offset above
//! `0xEE6B28000000` (the specification leaves that range to applications); the reserved
//! zero address reads as the empty string. A data type of `00` (reserved) is named `data`.

use std::sync::OnceLock;

use crate::crc::{CRC16_M17, crc};

/// The LSF sync burst.
pub const SYNC_LSF: u16 = 0x55F7;
/// The stream frame sync burst.
pub const SYNC_STREAM: u16 = 0xFF5D;
/// The packet frame sync burst.
pub const SYNC_PACKET: u16 = 0x75FF;
/// The BERT frame sync burst.
pub const SYNC_BERT: u16 = 0xDF55;
/// The end-of-transmission marker, repeated for 40 ms.
pub const EOT: u16 = 0x555D;
/// The byte an LSF's preamble repeats: +3, -3 alternating.
pub const PREAMBLE_BYTE: u8 = 0x77;

/// Payload bits in a frame (after the sync burst).
pub const FRAME_BITS: usize = 368;
/// Symbols in a whole frame, sync burst included: 40 ms at 4800 baud.
pub const FRAME_SYMBOLS: usize = 192;
/// Symbols in a sync burst.
pub const SYNC_SYMBOLS: usize = 8;
/// Bytes in an LSF, CRC included.
pub const LSF_BYTES: usize = 30;
/// Bytes of the LSF the CRC covers (the LSD).
pub const LSD_BYTES: usize = 28;
/// Bytes of an address field.
pub const ADDRESS_BYTES: usize = 6;
/// Bytes of the META field.
pub const META_BYTES: usize = 14;
/// LICH chunks that make an LSF.
pub const LICH_CHUNKS: usize = 6;
/// Bytes of the LSF in one LICH chunk.
pub const LICH_CHUNK_BYTES: usize = 5;
/// Bytes of stream data in a stream frame.
pub const STREAM_PAYLOAD_BYTES: usize = 16;
/// Zero tail bits the convolutional encoder appends.
pub const FLUSH_BITS: usize = 4;
/// The frame number's end-of-transmission bit; the rest counts frames.
pub const FN_LAST: u16 = 0x8000;

/// TYPE data type `01`: data.
pub const DATA_TYPE_DATA: u8 = 1;
/// TYPE data type `10`: voice (Codec 2 at 3200 bps).
pub const DATA_TYPE_VOICE: u8 = 2;
/// TYPE data type `11`: voice (Codec 2 at 1600 bps) and data.
pub const DATA_TYPE_VOICE_DATA: u8 = 3;
/// TYPE encryption type `00`: none.
pub const ENCRYPTION_NONE: u8 = 0;
/// TYPE encryption type `01`: scrambler.
pub const ENCRYPTION_SCRAMBLER: u8 = 1;
/// TYPE encryption type `10`: AES.
pub const ENCRYPTION_AES: u8 = 2;
/// TYPE encryption type `11`: other or reserved.
pub const ENCRYPTION_OTHER: u8 = 3;
/// With no encryption, the subtype saying META holds GNSS data.
pub const META_SUBTYPE_GNSS: u8 = 1;
/// The largest channel access number.
pub const CAN_MAX: u8 = 15;

/// TYPE field positions and widths.
const TYPE_DATA_SHIFT: u16 = 1;
const TYPE_ENCRYPTION_SHIFT: u16 = 3;
const TYPE_SUBTYPE_SHIFT: u16 = 5;
const TYPE_CAN_SHIFT: u16 = 7;
const TYPE_TWO_BITS: u16 = 0b11;
const TYPE_CAN_BITS: u16 = 0xF;

/// The LICH counter's place in the LICH's sixth byte.
const LICH_COUNT_SHIFT: u8 = 5;
/// LICH bits before and after Golay coding.
const LICH_BITS: usize = 48;
const LICH_CODED_BITS: usize = 96;
/// Golay data and code word widths.
const GOLAY_DATA_BITS: usize = 12;
const GOLAY_WORD_BITS: usize = 24;
const GOLAY_DATA_MASK: u16 = (1 << GOLAY_DATA_BITS) - 1;
const GOLAY_WORD_MASK: u32 = (1 << GOLAY_WORD_BITS) - 1;
/// Stream contents bits (frame number and data) before coding.
const STREAM_CONTENT_BITS: usize = 16 + 8 * STREAM_PAYLOAD_BYTES;

/// The convolutional code's polynomials (bit `k` is the tap `D^k`) and register mask.
const CONV_G1: u8 = 0x19;
const CONV_G2: u8 = 0x17;
const CONV_REG_MASK: u8 = 0x1F;
const CONV_STATES: usize = 16;

/// The interleaver's quadratic permutation polynomial.
const QPP_F1: usize = 45;
const QPP_F2: usize = 92;

/// The rows of `P` in the specification's Golay generator matrix `G = [I12 | P]`, the
/// first row for the data word's most significant bit.
pub const GOLAY_P: [u16; 12] = [
    0xC75, 0x63B, 0xF68, 0x7B4, 0x3DA, 0xD99, 0x6CD, 0x367, 0xDC6, 0xA97, 0x93E, 0x8EB,
];

/// Puncture pattern P1 (the LSF): a 1, then `1 0 1 1` fifteen times.
pub const P1: &[u8] = &[
    1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1,
    1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1,
];
/// Puncture pattern P2 (stream contents).
pub const P2: &[u8] = &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0];
/// Puncture pattern P3 (packet frames).
pub const P3: &[u8] = &[1, 1, 1, 1, 1, 1, 1, 0];

/// The randomizer sequence, 46 bytes.
pub const RANDOMIZER: [u8; FRAME_BITS / 8] = [
    0xD6, 0xB5, 0xE2, 0x30, 0x82, 0xFF, 0x84, 0x62, 0xBA, 0x4E, 0x96, 0x90, 0xD8, 0x98, 0xDD, 0x5D,
    0x0C, 0xC8, 0x52, 0x43, 0x91, 0x1D, 0xF8, 0x6E, 0x68, 0x2F, 0x35, 0xDA, 0x14, 0xEA, 0xCD, 0x76,
    0x19, 0x8D, 0xD5, 0x80, 0xD1, 0x33, 0x87, 0x13, 0x57, 0x18, 0x2D, 0x29, 0x78, 0xC3,
];

/// The address alphabet, by value.
const ALPHABET: &[u8; 40] = b" ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-/.";
/// Characters in an address at most.
pub const CALLSIGN_MAX: usize = 9;
const BASE: u64 = 40;
/// The BROADCAST address and its spelling.
const BROADCAST: u64 = 0xFFFF_FFFF_FFFF;
const BROADCAST_NAME: &str = "ALL";
/// The first extended-range address and the prefix its spelling takes.
const EXTENDED_BASE: u64 = 0xEE6B_2800_0000;
const EXTENDED_PREFIX: char = '#';

/// GNSS META: the validity nibble's position bit, and full scale of a 24-bit fraction.
const GNSS_POSITION_VALID: u8 = 0b1000;
const GNSS_VALIDITY_SHIFT: u8 = 4;
const GNSS_FULL_SCALE: f64 = 8_388_607.0;
const GNSS_LAT_SCALE_DEG: f64 = 90.0;
const GNSS_LON_SCALE_DEG: f64 = 180.0;
const GNSS_LAT_AT: usize = 3;
const GNSS_LON_AT: usize = 6;

/// Symbol index (0..=3 for -3, -1, +1, +3) of each dibit value.
const SYMBOL_OF_DIBIT: [u8; 4] = [2, 3, 1, 0];
/// Dibit value of each symbol index.
const DIBIT_OF_SYMBOL: [u8; 4] = [3, 2, 0, 1];

// ---- bits, bytes and symbols ----

/// `bytes` as bits, most significant first.
pub fn bytes_to_bits(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .flat_map(|&b| (0..8).rev().map(move |k| (b >> k) & 1))
        .collect()
}

/// `bits` (most significant first) packed into bytes; a short last byte is zero-padded.
pub fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8)
        .map(|c| {
            c.iter()
                .chain(std::iter::repeat(&0))
                .take(8)
                .fold(0, |a, &b| a << 1 | (b & 1))
        })
        .collect()
}

/// `bits` as symbols (0..=3 for -3, -1, +1, +3), two bits each, per the dibit table.
pub fn bits_to_symbols(bits: &[u8]) -> Vec<u8> {
    bits.chunks(2)
        .map(|d| {
            let dibit = (d[0] & 1) << 1 | d.get(1).map_or(0, |b| b & 1);
            SYMBOL_OF_DIBIT[usize::from(dibit)]
        })
        .collect()
}

/// Symbols (0..=3) back to bits, two each.
pub fn symbols_to_bits(symbols: &[u8]) -> Vec<u8> {
    symbols
        .iter()
        .flat_map(|&s| {
            let d = DIBIT_OF_SYMBOL[usize::from(s & 3)];
            [d >> 1, d & 1]
        })
        .collect()
}

/// A 16-bit word (a sync burst or the EOT) as its 8 symbols.
pub fn word_symbols(word: u16) -> [u8; SYNC_SYMBOLS] {
    bits_to_symbols(&bytes_to_bits(&word.to_be_bytes()))
        .try_into()
        .expect("16 bits are 8 symbols")
}

/// The 40 ms preamble before an LSF: +3, -3 alternating, ending on -3.
pub fn preamble_symbols() -> Vec<u8> {
    bits_to_symbols(&bytes_to_bits(&[PREAMBLE_BYTE; FRAME_SYMBOLS / 4]))
}

/// The 40 ms end-of-transmission marker: [`EOT`] repeated.
pub fn eot_symbols() -> Vec<u8> {
    word_symbols(EOT)
        .iter()
        .copied()
        .cycle()
        .take(FRAME_SYMBOLS)
        .collect()
}

// ---- Golay(24,12) ----

/// The 12 check bits of `data` (its low 12 bits): the XOR of the rows of `P` its set
/// bits select.
fn golay_check(data: u16) -> u16 {
    GOLAY_P
        .iter()
        .enumerate()
        .filter(|(i, _)| data >> (GOLAY_DATA_BITS - 1 - i) & 1 == 1)
        .fold(0, |a, (_, &row)| a ^ row)
}

/// The Golay(24,12) code word of `data` (its low 12 bits): data in bits 23..12, the
/// check bits below.
pub fn golay24_encode(data: u16) -> u32 {
    let data = data & GOLAY_DATA_MASK;
    u32::from(data) << GOLAY_DATA_BITS | u32::from(golay_check(data))
}

/// The syndrome of a 24-bit word: zero for a code word.
fn golay_syndrome(word: u32) -> u16 {
    golay_check((word >> GOLAY_DATA_BITS) as u16) ^ (word as u16 & GOLAY_DATA_MASK)
}

/// Every error pattern of weight 3 or less, by its syndrome (`u32::MAX` for none).
fn golay_table() -> &'static [u32] {
    static TABLE: OnceLock<Vec<u32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = vec![u32::MAX; 1 << GOLAY_DATA_BITS];
        t[0] = 0;
        let n = GOLAY_WORD_BITS as u32;
        for a in 0..n {
            for b in a..n {
                for c in b..n {
                    // Repeated positions give the weight 1 and 2 patterns.
                    let e = (1u32 << a) ^ (1 << b) ^ (1 << c);
                    t[usize::from(golay_syndrome(e))] = e;
                }
            }
        }
        t
    })
}

/// The data of a received Golay(24,12) word with up to 3 bit errors corrected; `None`
/// when the syndrome is no pattern of 3 or fewer errors (4 errors are always detected).
pub fn golay24_decode(word: u32) -> Option<u16> {
    let word = word & GOLAY_WORD_MASK;
    let e = golay_table()[usize::from(golay_syndrome(word))];
    (e != u32::MAX).then_some(((word ^ e) >> GOLAY_DATA_BITS) as u16)
}

// ---- the convolutional code ----

/// The two outputs (G1, G2) of the encoder register `r` (bit `k` the input `k` ago).
fn conv_out(r: u8) -> (u8, u8) {
    (
        ((r & CONV_G1).count_ones() & 1) as u8,
        ((r & CONV_G2).count_ones() & 1) as u8,
    )
}

/// `bits` and 4 zero tail bits through the rate 1/2, K = 5 encoder: G1 and G2 for each.
pub fn conv_encode(bits: &[u8]) -> Vec<u8> {
    let mut r = 0u8;
    bits.iter()
        .map(|b| b & 1)
        .chain(std::iter::repeat_n(0, FLUSH_BITS))
        .flat_map(|b| {
            r = (r << 1 | b) & CONV_REG_MASK;
            let (g1, g2) = conv_out(r);
            [g1, g2]
        })
        .collect()
}

/// The bits of `bits` whose place in `pattern`, repeated, is 1.
pub fn puncture(bits: &[u8], pattern: &[u8]) -> Vec<u8> {
    bits.iter()
        .zip(pattern.iter().cycle())
        .filter(|&(_, &keep)| keep == 1)
        .map(|(&b, _)| b)
        .collect()
}

/// Hard-decision Viterbi decoding of punctured bits: the punctured places are restored
/// as erasures (no cost either way), the path ending in state 0 (the tail) is traced
/// back and the 4 tail bits are dropped.
pub fn viterbi_decode(bits: &[u8], pattern: &[u8]) -> Vec<u8> {
    const UNREACHED: u32 = u32::MAX / 2;
    if pattern.is_empty() {
        return Vec::new();
    }
    let mut coded: Vec<Option<u8>> = Vec::with_capacity(bits.len() * 2);
    let mut it = bits.iter();
    for &keep in pattern.iter().cycle() {
        if keep == 1 {
            match it.next() {
                Some(&b) => coded.push(Some(b & 1)),
                None => break,
            }
        } else {
            coded.push(None);
        }
    }
    while coded.last() == Some(&None) {
        coded.pop();
    }
    if coded.len() % 2 == 1 {
        coded.push(None);
    }
    let steps = coded.len() / 2;
    let mut metric = [UNREACHED; CONV_STATES];
    metric[0] = 0;
    // For each step and new state, the oldest bit of the chosen predecessor.
    let mut history: Vec<[u8; CONV_STATES]> = Vec::with_capacity(steps);
    for pair in coded.chunks(2) {
        let mut next = [UNREACHED; CONV_STATES];
        let mut from = [0u8; CONV_STATES];
        for (ns, slot) in next.iter_mut().enumerate() {
            for x in 0..2u8 {
                let s = (ns >> 1) | usize::from(x) << 3;
                let (g1, g2) = conv_out(((s << 1) | (ns & 1)) as u8);
                let cost = u32::from(pair[0].is_some_and(|b| b != g1))
                    + u32::from(pair[1].is_some_and(|b| b != g2));
                let m = metric[s].saturating_add(cost);
                if m < *slot {
                    *slot = m;
                    from[ns] = x;
                }
            }
        }
        metric = next;
        history.push(from);
    }
    let mut state = 0usize;
    let mut out = Vec::with_capacity(steps);
    for from in history.iter().rev() {
        out.push((state & 1) as u8);
        state = (state >> 1) | usize::from(from[state]) << 3;
    }
    out.reverse();
    out.truncate(steps.saturating_sub(FLUSH_BITS));
    out
}

// ---- interleaver and randomizer ----

/// The interleaver's permutation: input index `x` goes to output index `pi(x)`.
fn qpp(x: usize) -> usize {
    (QPP_F1 * x + QPP_F2 * x * x) % FRAME_BITS
}

/// Interleave a frame's 368 bits: bit `x` moves to `pi(x)`.
pub fn interleave(bits: &[u8; FRAME_BITS]) -> [u8; FRAME_BITS] {
    let mut out = [0u8; FRAME_BITS];
    for (x, &b) in bits.iter().enumerate() {
        out[qpp(x)] = b;
    }
    out
}

/// Undo [`interleave`]: output bit `x` is input bit `pi(x)`.
pub fn deinterleave(bits: &[u8; FRAME_BITS]) -> [u8; FRAME_BITS] {
    core::array::from_fn(|x| bits[qpp(x)])
}

/// XOR a frame's 368 bits with the randomizer sequence; its own inverse.
pub fn decorrelate(bits: &mut [u8; FRAME_BITS]) {
    for (i, b) in bits.iter_mut().enumerate() {
        *b ^= (RANDOMIZER[i / 8] >> (7 - i % 8)) & 1;
    }
}

/// Type 3 bits to a frame's symbols: interleaved, randomized, after `sync`.
fn frame_symbols(sync: u16, type3: &[u8]) -> Vec<u8> {
    let bits: [u8; FRAME_BITS] = type3.try_into().expect("a frame's 368 bits");
    let mut bits = interleave(&bits);
    decorrelate(&mut bits);
    let mut out = word_symbols(sync).to_vec();
    out.extend(bits_to_symbols(&bits));
    out
}

/// A received frame's 368 payload bits, derandomized and deinterleaved.
fn frame_type3(bits: &[u8; FRAME_BITS]) -> [u8; FRAME_BITS] {
    let mut b = *bits;
    decorrelate(&mut b);
    deinterleave(&b)
}

// ---- addresses ----

/// The base-40 value of `chars`, the first character least significant; `None` for a
/// character outside the alphabet or more than nine.
fn base40(chars: &str) -> Option<u64> {
    if chars.chars().count() > CALLSIGN_MAX {
        return None;
    }
    chars.bytes().rev().try_fold(0u64, |v, c| {
        let digit = ALPHABET.iter().position(|&a| a == c)?;
        Some(v * BASE + digit as u64)
    })
}

/// The base-40 digits of `v`, least significant first.
fn base40_text(mut v: u64) -> String {
    let mut s = String::new();
    while v > 0 {
        s.push(char::from(ALPHABET[(v % BASE) as usize]));
        v /= BASE;
    }
    s
}

/// The 6-byte address of `call`: at most nine characters of the M17 alphabet (upper
/// case), not all spaces; `ALL` is BROADCAST; `#` and up to nine characters is an
/// extended-range address. `None` for anything else.
pub fn encode_callsign(call: &str) -> Option<[u8; ADDRESS_BYTES]> {
    let addr = if call == BROADCAST_NAME {
        BROADCAST
    } else if let Some(rest) = call.strip_prefix(EXTENDED_PREFIX) {
        let v = base40(rest).filter(|&v| v > 0)?;
        Some(EXTENDED_BASE + v).filter(|&a| a < BROADCAST)?
    } else {
        base40(call).filter(|&v| v > 0)?
    };
    addr.to_be_bytes()[2..].try_into().ok()
}

/// The text of a 6-byte address: the callsign, `ALL` for BROADCAST, `#…` for an
/// extended-range address, empty for the reserved zero address.
pub fn decode_callsign(bytes: &[u8; ADDRESS_BYTES]) -> String {
    let addr = bytes.iter().fold(0u64, |a, &b| a << 8 | u64::from(b));
    match addr {
        BROADCAST => BROADCAST_NAME.into(),
        a if a >= EXTENDED_BASE => {
            format!("{EXTENDED_PREFIX}{}", base40_text(a - EXTENDED_BASE))
        }
        a => base40_text(a),
    }
}

// ---- the LSF ----

/// A link setup frame's contents.
#[derive(Clone, Debug, PartialEq)]
pub struct Lsf {
    pub dst: String,
    pub src: String,
    /// Stream mode (else packet mode).
    pub stream: bool,
    /// [`DATA_TYPE_DATA`], [`DATA_TYPE_VOICE`] or [`DATA_TYPE_VOICE_DATA`].
    pub data_type: u8,
    /// [`ENCRYPTION_NONE`], [`ENCRYPTION_SCRAMBLER`], [`ENCRYPTION_AES`] or
    /// [`ENCRYPTION_OTHER`].
    pub encryption: u8,
    pub encryption_subtype: u8,
    /// Channel access number, 0..=15.
    pub can: u8,
    pub meta: [u8; META_BYTES],
}

impl Lsf {
    /// The 30 LSF bytes, the CRC appended. An address [`encode_callsign`] refuses is
    /// written as the reserved zero address (which reads back as the empty string).
    pub fn to_bytes(&self) -> [u8; LSF_BYTES] {
        let mut b = [0u8; LSF_BYTES];
        let addr = |c: &str| encode_callsign(c).unwrap_or([0; ADDRESS_BYTES]);
        b[..6].copy_from_slice(&addr(&self.dst));
        b[6..12].copy_from_slice(&addr(&self.src));
        let two = |v: u8| u16::from(v) & TYPE_TWO_BITS;
        let ty = u16::from(self.stream)
            | two(self.data_type) << TYPE_DATA_SHIFT
            | two(self.encryption) << TYPE_ENCRYPTION_SHIFT
            | two(self.encryption_subtype) << TYPE_SUBTYPE_SHIFT
            | (u16::from(self.can) & TYPE_CAN_BITS) << TYPE_CAN_SHIFT;
        b[12..14].copy_from_slice(&ty.to_be_bytes());
        b[14..LSD_BYTES].copy_from_slice(&self.meta);
        let c = crc(&CRC16_M17, &b[..LSD_BYTES]) as u16;
        b[LSD_BYTES..].copy_from_slice(&c.to_be_bytes());
        b
    }

    /// The LSF in `bytes`, or `None` when its CRC fails.
    pub fn parse(bytes: &[u8; LSF_BYTES]) -> Option<Lsf> {
        if crc(&CRC16_M17, bytes) != 0 {
            return None;
        }
        let addr = |r: std::ops::Range<usize>| {
            decode_callsign(bytes[r].try_into().expect("six address bytes"))
        };
        let ty = u16::from_be_bytes([bytes[12], bytes[13]]);
        let two = |shift: u16| (ty >> shift & TYPE_TWO_BITS) as u8;
        Some(Lsf {
            dst: addr(0..6),
            src: addr(6..12),
            stream: ty & 1 == 1,
            data_type: two(TYPE_DATA_SHIFT),
            encryption: two(TYPE_ENCRYPTION_SHIFT),
            encryption_subtype: two(TYPE_SUBTYPE_SHIFT),
            can: (ty >> TYPE_CAN_SHIFT & TYPE_CAN_BITS) as u8,
            meta: bytes[14..LSD_BYTES]
                .try_into()
                .expect("fourteen META bytes"),
        })
    }
}

/// The `type` an event names: `packet` for packet mode, else `voice`, `voice_data` or
/// `data` (the reserved data type `00` included).
pub fn type_name(lsf: &Lsf) -> &'static str {
    match (lsf.stream, lsf.data_type) {
        (false, _) => "packet",
        (true, DATA_TYPE_VOICE) => "voice",
        (true, DATA_TYPE_VOICE_DATA) => "voice_data",
        (true, _) => "data",
    }
}

/// The `encryption` an event names: `None` for none (and in packet mode, where the bits
/// are reserved), else `scrambler`, `aes` or `other`.
pub fn encryption_name(lsf: &Lsf) -> Option<&'static str> {
    match (lsf.stream, lsf.encryption) {
        (true, ENCRYPTION_SCRAMBLER) => Some("scrambler"),
        (true, ENCRYPTION_AES) => Some("aes"),
        (true, ENCRYPTION_OTHER) => Some("other"),
        _ => None,
    }
}

/// The LSF frame's 192 symbols, the sync burst first: the LSF and its tail
/// convolutionally coded, P1 punctured, interleaved and randomized.
pub fn lsf_frame_symbols(lsf: &Lsf) -> Vec<u8> {
    let coded = puncture(&conv_encode(&bytes_to_bits(&lsf.to_bytes())), P1);
    frame_symbols(SYNC_LSF, &coded)
}

/// A received LSF frame's 368 payload bits (the symbols after the sync burst, as bits)
/// decoded to the LSF bytes; `None` when the CRC fails.
pub fn decode_lsf_frame(bits: &[u8; FRAME_BITS]) -> Option<[u8; LSF_BYTES]> {
    let decoded = viterbi_decode(&frame_type3(bits), P1);
    let bytes: [u8; LSF_BYTES] = bits_to_bytes(&decoded).try_into().ok()?;
    (crc(&CRC16_M17, &bytes) == 0).then_some(bytes)
}

// ---- stream frames ----

/// A stream frame's 192 symbols, the sync burst first: the LICH (chunk
/// `frame_number % 6` of the LSF and its counter, four Golay words), then the frame
/// number (its top bit set when `last`) and `payload`, convolutionally coded and P2
/// punctured; interleaved and randomized.
pub fn stream_frame_symbols(
    lsf: &Lsf,
    frame_number: u16,
    payload: &[u8; STREAM_PAYLOAD_BYTES],
    last: bool,
) -> Vec<u8> {
    let count = usize::from(frame_number) % LICH_CHUNKS;
    let lsf_bytes = lsf.to_bytes();
    let mut lich = [0u8; LICH_CHUNK_BYTES + 1];
    lich[..LICH_CHUNK_BYTES]
        .copy_from_slice(&lsf_bytes[count * LICH_CHUNK_BYTES..(count + 1) * LICH_CHUNK_BYTES]);
    lich[LICH_CHUNK_BYTES] = (count as u8) << LICH_COUNT_SHIFT;
    let mut type3: Vec<u8> = bytes_to_bits(&lich)
        .chunks(GOLAY_DATA_BITS)
        .flat_map(|c| {
            let word = golay24_encode(c.iter().fold(0u16, |a, &b| a << 1 | u16::from(b)));
            (0..GOLAY_WORD_BITS)
                .rev()
                .map(move |k| (word >> k & 1) as u8)
        })
        .collect();
    let fn_word = (frame_number & !FN_LAST) | if last { FN_LAST } else { 0 };
    let mut contents = fn_word.to_be_bytes().to_vec();
    contents.extend_from_slice(payload);
    type3.extend(puncture(&conv_encode(&bytes_to_bits(&contents)), P2));
    frame_symbols(SYNC_STREAM, &type3)
}

/// A decoded stream frame.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamFrame {
    /// The LICH's LSF chunk and its counter (0..=5); `None` when a Golay word had more
    /// than 3 errors or the counter is out of range.
    pub lich: Option<([u8; LICH_CHUNK_BYTES], u8)>,
    /// The frame number without its end-of-transmission bit.
    pub frame_number: u16,
    /// The end-of-transmission bit: the stream's last frame.
    pub last: bool,
    pub payload: [u8; STREAM_PAYLOAD_BYTES],
}

/// A received stream frame's 368 payload bits (the symbols after the sync burst, as
/// bits) decoded: the LICH by Golay, the contents by Viterbi (hard decisions; the
/// contents carry no CRC, so nothing checks them).
pub fn decode_stream_frame(bits: &[u8; FRAME_BITS]) -> StreamFrame {
    let type3 = frame_type3(bits);
    let words: Option<Vec<u16>> = type3[..LICH_CODED_BITS]
        .chunks(GOLAY_WORD_BITS)
        .map(|c| golay24_decode(c.iter().fold(0u32, |a, &b| a << 1 | u32::from(b))))
        .collect();
    let lich = words.and_then(|words| {
        let bits: Vec<u8> = words
            .iter()
            .flat_map(|&w| (0..GOLAY_DATA_BITS).rev().map(move |k| (w >> k & 1) as u8))
            .collect();
        debug_assert_eq!(bits.len(), LICH_BITS);
        let bytes = bits_to_bytes(&bits);
        let count = bytes[LICH_CHUNK_BYTES] >> LICH_COUNT_SHIFT;
        (usize::from(count) < LICH_CHUNKS).then(|| {
            (
                bytes[..LICH_CHUNK_BYTES].try_into().expect("five bytes"),
                count,
            )
        })
    });
    let contents = bits_to_bytes(&viterbi_decode(&type3[LICH_CODED_BITS..], P2));
    debug_assert_eq!(contents.len() * 8, STREAM_CONTENT_BITS);
    let fn_word = u16::from_be_bytes([contents[0], contents[1]]);
    StreamFrame {
        lich,
        frame_number: fn_word & !FN_LAST,
        last: fn_word & FN_LAST != 0,
        payload: contents[2..].try_into().expect("sixteen payload bytes"),
    }
}

/// The LSF rebuilt from its six LICH chunks (by counter); `None` while one is missing
/// or when the rebuilt LSF fails its CRC.
pub fn lich_assemble(
    chunks: &[Option<[u8; LICH_CHUNK_BYTES]>; LICH_CHUNKS],
) -> Option<[u8; LSF_BYTES]> {
    let mut out = [0u8; LSF_BYTES];
    for (i, c) in chunks.iter().enumerate() {
        out[i * LICH_CHUNK_BYTES..(i + 1) * LICH_CHUNK_BYTES].copy_from_slice(c.as_ref()?);
    }
    (crc(&CRC16_M17, &out) == 0).then_some(out)
}

// ---- GNSS META ----

/// A signed 24-bit big-endian value.
fn i24(b: &[u8]) -> i32 {
    (i32::from(b[0]) << 24 | i32::from(b[1]) << 16 | i32::from(b[2]) << 8) >> 8
}

/// The latitude and longitude of a META field in the specification's GNSS form, when
/// its validity nibble marks the position valid; `None` otherwise. The caller checks
/// the LSF says the META is GNSS data (no encryption, subtype [`META_SUBTYPE_GNSS`]).
pub fn gnss_position(meta: &[u8; META_BYTES]) -> Option<(f64, f64)> {
    if (meta[1] >> GNSS_VALIDITY_SHIFT) & GNSS_POSITION_VALID == 0 {
        return None;
    }
    // -2^23 is never sent.
    let never = -(GNSS_FULL_SCALE as i32) - 1;
    let lat = i24(&meta[GNSS_LAT_AT..GNSS_LAT_AT + 3]);
    let lon = i24(&meta[GNSS_LON_AT..GNSS_LON_AT + 3]);
    if lat == never || lon == never {
        return None;
    }
    Some((
        f64::from(lat) * GNSS_LAT_SCALE_DEG / GNSS_FULL_SCALE,
        f64::from(lon) * GNSS_LON_SCALE_DEG / GNSS_FULL_SCALE,
    ))
}

/// A GNSS META field carrying only a position (data source M17 client, fixed station,
/// validity: position), for tests and generators.
pub fn gnss_meta(lat: f64, lon: f64) -> [u8; META_BYTES] {
    let mut m = [0u8; META_BYTES];
    m[1] = GNSS_POSITION_VALID << GNSS_VALIDITY_SHIFT;
    let put = |m: &mut [u8; META_BYTES], at: usize, v: f64, scale: f64| {
        let raw = (v / scale * GNSS_FULL_SCALE)
            .round()
            .clamp(-GNSS_FULL_SCALE, GNSS_FULL_SCALE) as i32;
        m[at..at + 3].copy_from_slice(&raw.to_be_bytes()[1..]);
    };
    put(&mut m, GNSS_LAT_AT, lat, GNSS_LAT_SCALE_DEG);
    put(&mut m, GNSS_LON_AT, lon, GNSS_LON_SCALE_DEG);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repeatable pseudo-random bit source (an LCG), so the tests need no crate.
    fn bits(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (s >> 63) as u8
            })
            .collect()
    }

    fn lsf() -> Lsf {
        Lsf {
            dst: "ALL".into(),
            src: "VK4ZET".into(),
            stream: true,
            data_type: DATA_TYPE_VOICE,
            encryption: ENCRYPTION_NONE,
            encryption_subtype: 0,
            can: 5,
            meta: [0; META_BYTES],
        }
    }

    #[test]
    fn golay_round_trips_and_corrects_three_errors() {
        for data in [0u16, 0xFFF, 0xABC, 0x123] {
            let word = golay24_encode(data);
            assert_eq!(word >> 12, u32::from(data), "systematic");
            assert_eq!(golay24_decode(word), Some(data));
            for a in 0..24 {
                for b in a + 1..24 {
                    for c in b + 1..24 {
                        let bad = word ^ (1 << a) ^ (1 << b) ^ (1 << c);
                        assert_eq!(golay24_decode(bad), Some(data), "{data:03x} {a} {b} {c}");
                    }
                }
            }
        }
    }

    #[test]
    fn golay_detects_four_errors() {
        let word = golay24_encode(0x5A5);
        assert_eq!(golay24_decode(word ^ 0b1111), None);
        assert_eq!(golay24_decode(word ^ (0b11 << 22) ^ 0b11), None);
    }

    #[test]
    fn the_specifications_golay_vector() {
        // The rows of the specification's generator matrix G = [I12 | P]: data 0x800
        // (the first row) and 0x001 (the last), each data word followed by its row of P.
        assert_eq!(golay24_encode(0x800), 0x800_C75);
        assert_eq!(golay24_encode(0x001), 0x001_8EB);
        // And the code those rows make is the extended Golay code: distance 8.
        let min = (1u16..4096)
            .map(|d| golay24_encode(d).count_ones())
            .min()
            .unwrap();
        assert_eq!(min, 8);
    }

    #[test]
    fn the_convolutional_code_has_the_specifications_polynomials() {
        // G1 = 1 + D^3 + D^4, G2 = 1 + D + D^2 + D^4: a lone 1 gives the impulse
        // response, G1 and G2 interleaved, then the four flush bits' zeros.
        let out = conv_encode(&[1]);
        assert_eq!(out.len(), 2 * (1 + FLUSH_BITS));
        let g1: Vec<u8> = out.iter().step_by(2).copied().collect();
        let g2: Vec<u8> = out.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(g1, [1, 0, 0, 1, 1]);
        assert_eq!(g2, [1, 1, 1, 0, 1]);
    }

    #[test]
    fn the_puncture_patterns_keep_the_specifications_counts() {
        assert_eq!(P1.len(), 61);
        assert_eq!(P1.iter().filter(|&&b| b == 1).count(), 46);
        assert_eq!(P2, [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0]);
        assert_eq!(P3, [1, 1, 1, 1, 1, 1, 1, 0]);
        // The LSF: 240 bits + 4 flush, 488 coded, 368 after P1.
        assert_eq!(puncture(&conv_encode(&[0; 240]), P1).len(), FRAME_BITS);
        // The stream contents: 144 + 4, 296 coded, 272 after P2.
        assert_eq!(puncture(&conv_encode(&[0; 144]), P2).len(), 272);
        // A packet frame: 206 + 4, 420 coded, 368 after P3.
        assert_eq!(puncture(&conv_encode(&[0; 206]), P3).len(), FRAME_BITS);
    }

    #[test]
    fn viterbi_recovers_a_punctured_codeword_with_two_errors() {
        let input = bits(240, 7);
        let mut coded = puncture(&conv_encode(&input), P1);
        coded[40] ^= 1;
        coded[200] ^= 1;
        assert_eq!(viterbi_decode(&coded, P1), input);
        let input = bits(144, 9);
        let mut coded = puncture(&conv_encode(&input), P2);
        coded[10] ^= 1;
        coded[150] ^= 1;
        assert_eq!(viterbi_decode(&coded, P2), input);
    }

    #[test]
    fn the_interleaver_is_the_specifications_table() {
        // Rows of the specification's table (input index, output index).
        let mut one = [0u8; FRAME_BITS];
        for (i, want) in [(0, 0), (1, 137), (2, 90), (47, 367), (91, 139), (367, 47)] {
            one.fill(0);
            one[i] = 1;
            let out = interleave(&one);
            assert_eq!(out.iter().position(|&b| b == 1), Some(want), "{i}");
        }
        let x: [u8; FRAME_BITS] = bits(FRAME_BITS, 3).try_into().unwrap();
        assert_eq!(deinterleave(&interleave(&x)), x);
    }

    #[test]
    fn the_decorrelator_is_the_specifications_sequence_and_its_own_inverse() {
        let mut zeros = [0u8; FRAME_BITS];
        decorrelate(&mut zeros);
        // 0xD6, 0xB5 first, 0xC3 last.
        assert_eq!(
            zeros[..16],
            [1, 1, 0, 1, 0, 1, 1, 0, 1, 0, 1, 1, 0, 1, 0, 1]
        );
        assert_eq!(zeros[360..], [1, 1, 0, 0, 0, 0, 1, 1]);
        let x: [u8; FRAME_BITS] = bits(FRAME_BITS, 5).try_into().unwrap();
        let mut y = x;
        decorrelate(&mut y);
        assert_ne!(y, x);
        decorrelate(&mut y);
        assert_eq!(y, x);
    }

    #[test]
    fn symbols_follow_the_dibit_table() {
        // The specification's example: byte 0xB4 is sent -1, -3, +3, +1.
        assert_eq!(bits_to_symbols(&[1, 0, 1, 1, 0, 1, 0, 0]), [1, 0, 3, 2]);
        assert_eq!(symbols_to_bits(&[1, 0, 3, 2]), [1, 0, 1, 1, 0, 1, 0, 0]);
        // The sync bursts as the specification lists them.
        assert_eq!(word_symbols(SYNC_LSF), [3, 3, 3, 3, 0, 0, 3, 0]);
        assert_eq!(word_symbols(SYNC_BERT), [0, 3, 0, 0, 3, 3, 3, 3]);
        assert_eq!(word_symbols(SYNC_STREAM), [0, 0, 0, 0, 3, 3, 0, 3]);
        assert_eq!(word_symbols(SYNC_PACKET), [3, 0, 3, 3, 0, 0, 0, 0]);
        assert_eq!(word_symbols(EOT), [3, 3, 3, 3, 3, 3, 0, 3]);
        // The LSF preamble alternates +3, -3 and ends opposite the sync's first +3.
        let pre = preamble_symbols();
        assert_eq!(pre.len(), FRAME_SYMBOLS);
        assert_eq!(pre[..4], [3, 0, 3, 0]);
        assert_eq!(pre[FRAME_SYMBOLS - 1], 0);
        let eot = eot_symbols();
        assert_eq!(eot.len(), FRAME_SYMBOLS);
        assert_eq!(eot[8..16], [3, 3, 3, 3, 3, 3, 0, 3]);
    }

    #[test]
    fn callsigns_round_trip_in_base_40() {
        // The specification's worked example: AB1CD is 0x9FDD51.
        assert_eq!(encode_callsign("AB1CD"), Some([0, 0, 0, 0x9F, 0xDD, 0x51]));
        assert_eq!(encode_callsign("ALL"), Some([0xFF; 6]));
        for c in [
            "VK4ZET",
            "ALL",
            "N0CALL/P",
            "M17-M17 C",
            ".........",
            "#BOOT",
        ] {
            let bytes = encode_callsign(c).unwrap();
            assert_eq!(decode_callsign(&bytes), c);
        }
        assert_eq!(encode_callsign("VK4ZETABCD"), None, "ten characters");
        assert_eq!(encode_callsign(""), None);
        assert_eq!(encode_callsign("   "), None, "the reserved zero address");
        assert_eq!(encode_callsign("vk4zet"), None, "outside the alphabet");
        assert_eq!(decode_callsign(&[0; 6]), "");
    }

    #[test]
    fn an_lsf_round_trips_with_its_crc() {
        let lsf = lsf();
        let bytes = lsf.to_bytes();
        assert_eq!(bytes[..6], [0xFF; 6]);
        // TYPE: stream, voice, no encryption, CAN 5 (bits 7..10).
        assert_eq!(
            u16::from_be_bytes([bytes[12], bytes[13]]),
            1 | 2 << 1 | 5 << 7
        );
        assert_eq!(crate::crc::crc(&crate::crc::CRC16_M17, &bytes), 0);
        assert_eq!(Lsf::parse(&bytes), Some(lsf.clone()));
        for i in [0, 13, 29] {
            let mut bad = bytes;
            bad[i] ^= 0x10;
            assert_eq!(Lsf::parse(&bad), None, "{i}");
        }
        assert_eq!(type_name(&lsf), "voice");
        assert_eq!(encryption_name(&lsf), None);
        let aes = Lsf {
            encryption: ENCRYPTION_AES,
            data_type: DATA_TYPE_VOICE_DATA,
            ..lsf.clone()
        };
        assert_eq!(type_name(&aes), "voice_data");
        assert_eq!(encryption_name(&aes), Some("aes"));
        let packet = Lsf {
            stream: false,
            ..lsf
        };
        assert_eq!(type_name(&packet), "packet");
    }

    #[test]
    fn an_lsf_frame_decodes_to_its_lsf() {
        let lsf = lsf();
        let syms = lsf_frame_symbols(&lsf);
        assert_eq!(syms.len(), FRAME_SYMBOLS);
        assert_eq!(syms[..8], word_symbols(SYNC_LSF));
        let mut payload: [u8; FRAME_BITS] = symbols_to_bits(&syms[8..]).try_into().unwrap();
        payload[17] ^= 1;
        payload[250] ^= 1;
        assert_eq!(decode_lsf_frame(&payload), Some(lsf.to_bytes()));
    }

    #[test]
    fn a_stream_frame_decodes_to_its_lich_chunk_and_payload() {
        let lsf = lsf();
        let lsf_bytes = lsf.to_bytes();
        let payload: [u8; STREAM_PAYLOAD_BYTES] = core::array::from_fn(|i| i as u8 * 7);
        for (fn_, last) in [(0u16, false), (4, false), (13, true)] {
            let syms = stream_frame_symbols(&lsf, fn_, &payload, last);
            assert_eq!(syms.len(), FRAME_SYMBOLS);
            assert_eq!(syms[..8], word_symbols(SYNC_STREAM));
            let mut bits: [u8; FRAME_BITS] = symbols_to_bits(&syms[8..]).try_into().unwrap();
            // A couple of channel errors: they land somewhere after deinterleaving.
            bits[5] ^= 1;
            bits[300] ^= 1;
            let frame = decode_stream_frame(&bits);
            let chunk = usize::from(fn_) % LICH_CHUNKS;
            let want: [u8; LICH_CHUNK_BYTES] = lsf_bytes
                [chunk * LICH_CHUNK_BYTES..(chunk + 1) * LICH_CHUNK_BYTES]
                .try_into()
                .unwrap();
            assert_eq!(frame.lich, Some((want, chunk as u8)));
            assert_eq!(frame.frame_number, fn_);
            assert_eq!(frame.last, last);
            assert_eq!(frame.payload, payload);
        }
    }

    #[test]
    fn six_chunks_rebuild_the_lsf() {
        let lsf = lsf();
        let payload = [0u8; STREAM_PAYLOAD_BYTES];
        let mut chunks: [Option<[u8; LICH_CHUNK_BYTES]>; LICH_CHUNKS] = [None; LICH_CHUNKS];
        // A receiver joining at frame 3: frames 3..9 carry chunks 3, 4, 5, 0, 1, 2.
        for fn_ in 3..9u16 {
            assert_eq!(lich_assemble(&chunks), None);
            let syms = stream_frame_symbols(&lsf, fn_, &payload, false);
            let bits: [u8; FRAME_BITS] = symbols_to_bits(&syms[8..]).try_into().unwrap();
            let (chunk, n) = decode_stream_frame(&bits).lich.unwrap();
            chunks[usize::from(n)] = Some(chunk);
        }
        assert_eq!(lich_assemble(&chunks), Some(lsf.to_bytes()));
        // A corrupted chunk fails the CRC.
        let mut bad = chunks;
        bad[2].as_mut().unwrap()[0] ^= 1;
        assert_eq!(lich_assemble(&bad), None);
    }

    #[test]
    fn gnss_meta_gives_a_position_and_zero_meta_none() {
        let meta = gnss_meta(-27.3842, 153.1175);
        // Validity: the position bit only.
        assert_eq!(meta[1] >> 4, 0b1000);
        let (lat, lon) = gnss_position(&meta).unwrap();
        assert!((lat - -27.3842).abs() < 0.001, "{lat}");
        assert!((lon - 153.1175).abs() < 0.001, "{lon}");
        assert_eq!(gnss_position(&[0; META_BYTES]), None);
        // The pole and the antimeridian at full scale.
        let (lat, lon) = gnss_position(&gnss_meta(90.0, -180.0)).unwrap();
        assert!((lat - 90.0).abs() < 1e-9 && (lon + 180.0).abs() < 1e-9);
    }
}
