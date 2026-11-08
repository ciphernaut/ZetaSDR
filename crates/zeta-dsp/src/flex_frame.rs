//! FLEX paging frame generator: builds one 1.875 s frame at 1600 bit/s, 2-level, holding
//! one alphanumeric page, for the `flex` synthesiser family (`zeta modes check flex_decode`).
//!
//! Written without any decoder's source. The frame outline (1.875 s frames, 128 to a
//! 4-minute cycle; a frame is sync 1, the frame information word and sync 2, then blocks
//! 0 to 10; sync 1 opens with a 32-bit bit sync pattern) is as search summaries of the
//! FLEX patents US 6,038,549, US 6,378,073 and US 6,396,411 describe it. The patents
//! themselves were not read; they are credited with nothing more. The rates and the ±4800 Hz
//! deviation are general knowledge (the design spec points to the Signal Identification
//! Wiki's FLEX page, <https://www.sigidwiki.com/wiki/FLEX>, which was not fetched for this
//! module). The field
//! layouts below (the sync words and the 64-bit sync code `AAAA:A6C6AAAA:~AAAA` with
//! `0x870C` for 1600 bit/s 2-level, the word bit order, the frame and block information
//! words, the interleave, the short address as the capcode plus 32768, the vector and
//! the message words) are from general knowledge of the protocol, each confirmed against
//! the oracle where marked. The oracle is multimon-ng 1.5.0, whose sources (GPL) were not
//! read: it was run only as a black box, fed audio through `-a FLEX_NEXT -v 3`, whose
//! debug lines name the stage it reached (sync, frame information word, block information
//! word, address, vector) and what it read there; `strings` was run on its binary only to
//! see its output formats.
//!
//! - **Line**: NRZ 2-FSK at [`BAUD`]; [`FRAME_BITS`] bits a frame.
//! - **Words**: 21 data bits `i0..i20` (`i0` the least significant bit of the values
//!   below), sent `i0` first, then the 10 check bits and the parity bit. On the line a
//!   word is the BCH(31,21) word of POCSAG's layout ([`crate::bch::bch3121_word`]) of the
//!   bit-reversed data, sent most significant bit first (oracle: it reads every word
//!   this way with no corrections).
//! - **Sync 1**: [`BIT_SYNC_BITS`] alternating bits, then [`SYNC_A_1600_2`], the marker
//!   [`SYNC_MARKER`], [`SYNC_B`], the inverted A and the inverted marker, 16 bits each,
//!   most significant bit first: 112 bits. They are sent complemented relative to the
//!   words (oracle: it locks on either polarity, reporting `sync_code=0x870c`, but reads
//!   the frame information word only when the sync is the complement of what the words'
//!   polarity would make it; sent complemented, it reports `polarity=POS`).
//! - **Frame information word**: the checksum in `i0..i3` (the 4-bit sum of the six
//!   groups `i0..i3`, `i4..i7`, `i8..i11`, `i12..i15`, `i16..i19` and `i20` is
//!   [`CHECKSUM_SUM`]), the cycle in `i4..i7`, the frame in `i8..i14`, the rest zero
//!   (oracle: it prints the cycle and frame and the time they stand for).
//! - **Sync 2**: [`SYNC2_BITS`] bits at 1600 bit/s: 4 bits of comma, [`SYNC_C`], the
//!   inverted comma and the inverted C, complemented like sync 1. The oracle times it
//!   rather than reading it.
//! - **Blocks**: [`BLOCKS`] blocks of [`BLOCK_WORDS`] words, [`FRAME_WORDS`] in all (the one
//!   phase 1600 bit/s carries). Each block goes out by column: the first bit of each of
//!   its 8 words, then the second, and so on (oracle).
//! - **Block information word** (word 0): the checksum, no priority addresses, no further
//!   block information words, and the vector field's first word in `i10..i15`. The
//!   address field is the words between it and the vector field (oracle: `AW 01 VW 02`).
//! - **Address** (word 1): the short address, the capcode plus [`SHORT_ADDRESS_OFFSET`];
//!   capcodes [`CAPCODE_MIN`]..=[`CAPCODE_MAX`] (oracle: 0 and 1 933 313 are refused as
//!   out of range, 1 and 1 933 312 decode).
//! - **Vector** (word 2): the checksum, the type in `i4..i6` ([`VECTOR_ALPHA`]), the
//!   message's first word in `i7..i13` and its length in words in `i14..i20` (oracle: it
//!   prints the first word after the header and the length less the header).
//! - **Message**: a header word (the 10-bit fragment check `i0..i9`, zero here, which the
//!   oracle does not check; continued `i10` 0; the fragment number `i11..i12`
//!   [`FRAGMENT_ONLY`], which the oracle prints as `3.0.K`, complete), then 7-bit
//!   characters three to a word from `i0`: the first slot the message signature (zero;
//!   the oracle does not print it), then `text`, then zero fill (oracle: the text comes
//!   back exact, [`TEXT_MAX`] characters at most, the most a frame holds).
//! - **Idle**: the remaining words alternate the all-zero and all-one data words, so the
//!   line keeps changing (oracle: a block of identical bits loses it, `Timeout`, before it
//!   reads the block information word).

use crate::bch::bch3121_word;

/// Bits a second.
pub const BAUD: u32 = 1600;
/// The frame's length, ms.
pub const FRAME_MS: u32 = 1875;
/// Bits a frame at [`BAUD`].
pub const FRAME_BITS: usize = (BAUD * FRAME_MS / 1000) as usize;
/// Alternating bits that open sync 1.
pub const BIT_SYNC_BITS: usize = 32;
/// The A word for 1600 bit/s 2-level.
pub const SYNC_A_1600_2: u16 = 0x870C;
/// The fixed middle of the 64-bit sync code, after A.
pub const SYNC_MARKER: u16 = 0xA6C6;
/// The B pattern.
pub const SYNC_B: u16 = 0xAAAA;
/// The C pattern of sync 2.
pub const SYNC_C: u16 = 0xED84;
/// Sync 2's length at 1600 bit/s (25 ms).
pub const SYNC2_BITS: usize = 40;
/// Comma bits either side of C in sync 2.
const COMMA_BITS: usize = 4;
/// Blocks a frame.
pub const BLOCKS: usize = 11;
/// Words a block.
pub const BLOCK_WORDS: usize = 8;
/// Words a frame at 1600 bit/s.
pub const FRAME_WORDS: usize = BLOCKS * BLOCK_WORDS;
/// Bits a word on the line.
pub const WORD_BITS: usize = 32;
/// The 4-bit sum a word's checksum makes.
pub const CHECKSUM_SUM: u32 = 0xF;
/// The short address is the capcode plus this.
pub const SHORT_ADDRESS_OFFSET: u32 = 32_768;
/// The lowest short-address capcode.
pub const CAPCODE_MIN: u32 = 1;
/// The highest short-address capcode.
pub const CAPCODE_MAX: u32 = 1_933_312;
/// The vector type of an alphanumeric message.
pub const VECTOR_ALPHA: u32 = 5;
/// The fragment number of a message sent whole.
pub const FRAGMENT_ONLY: u32 = 3;
/// 7-bit characters a message word holds.
const CHARS_PER_WORD: usize = 3;
/// Bits a character.
const CHAR_BITS: u32 = 7;
/// Words before the message: the block information word, the address, the vector and
/// the message header.
const OVERHEAD_WORDS: usize = 4;
/// The most text characters one frame holds: the character slots after the overhead,
/// less the signature.
pub const TEXT_MAX: usize = (FRAME_WORDS - OVERHEAD_WORDS) * CHARS_PER_WORD - 1;
/// The frames in a cycle, so the frame number's range.
pub const FRAMES_PER_CYCLE: u8 = 128;
/// The cycles in an hour, so the cycle number's range.
pub const CYCLES: u8 = 15;

/// One frame's page.
#[derive(Clone, Debug, PartialEq)]
pub struct FlexParams {
    /// The pager's capcode, a short address ([`CAPCODE_MIN`]..=[`CAPCODE_MAX`]).
    pub capcode: u32,
    /// Printable ASCII, at most [`TEXT_MAX`] characters.
    pub text: String,
    /// The cycle, 0..[`CYCLES`].
    pub cycle: u8,
    /// The frame in the cycle, 0..[`FRAMES_PER_CYCLE`].
    pub frame: u8,
}

/// The data word `v` (`i0` its least significant bit) as the 32 bits sent, first first.
pub fn word_line_bits(v: u32) -> [u8; WORD_BITS] {
    let w = bch3121_word(reverse21(v));
    std::array::from_fn(|k| ((w >> (WORD_BITS - 1 - k)) & 1) as u8)
}

/// The 21 data bits of `v` in reverse order.
fn reverse21(v: u32) -> u32 {
    (v << (32 - crate::bch::DATA_BITS)).reverse_bits()
}

/// `v` with its checksum in `i0..i3`: the low four bits set so the 4-bit sum of the
/// word's six groups is [`CHECKSUM_SUM`].
fn with_checksum(v: u32) -> u32 {
    let v = v & !0xF;
    let sum: u32 = (1..6).map(|k| (v >> (4 * k)) & 0xF).sum();
    v | (CHECKSUM_SUM.wrapping_sub(sum) & 0xF)
}

/// 16 bits, most significant first.
fn push16(out: &mut Vec<u8>, x: u16) {
    out.extend((0..16).rev().map(|k| ((x >> k) & 1) as u8));
}

/// Sync 1, the frame information word and sync 2: everything before the blocks.
fn header_bits(cycle: u8, frame: u8) -> Vec<u8> {
    let mut sync1: Vec<u8> = (0..BIT_SYNC_BITS).map(|k| u8::from(k % 2 == 0)).collect();
    for x in [
        SYNC_A_1600_2,
        SYNC_MARKER,
        SYNC_B,
        !SYNC_A_1600_2,
        !SYNC_MARKER,
    ] {
        push16(&mut sync1, x);
    }
    let mut sync2: Vec<u8> = (0..COMMA_BITS).map(|k| u8::from(k % 2 == 0)).collect();
    push16(&mut sync2, SYNC_C);
    sync2.extend((0..COMMA_BITS).map(|k| u8::from(k % 2 == 1)));
    push16(&mut sync2, !SYNC_C);
    debug_assert_eq!(sync2.len(), SYNC2_BITS);
    let fiw = with_checksum((u32::from(cycle) & 0xF) << 4 | (u32::from(frame) & 0x7F) << 8);
    // The syncs go out complemented relative to the words (see the module comment).
    let mut out: Vec<u8> = sync1.iter().map(|b| b ^ 1).collect();
    out.extend(word_line_bits(fiw));
    out.extend(sync2.iter().map(|b| b ^ 1));
    out
}

/// The frame's [`FRAME_WORDS`] data words: block information, address, vector, the
/// message, then idle words.
fn frame_words(p: &FlexParams) -> Vec<u32> {
    const ADDRESS_AT: usize = 1;
    const VECTOR_AT: usize = 2;
    const MESSAGE_AT: usize = 3;
    let mut chars: Vec<u32> = std::iter::once(0)
        .chain(p.text.bytes().map(|b| u32::from(b & 0x7F)))
        .collect();
    chars.resize(chars.len().div_ceil(CHARS_PER_WORD) * CHARS_PER_WORD, 0);
    let mut message = vec![FRAGMENT_ONLY << 11];
    message.extend(chars.chunks(CHARS_PER_WORD).map(|c| {
        c.iter()
            .enumerate()
            .fold(0, |w, (k, &ch)| w | ch << (CHAR_BITS * k as u32))
    }));
    let mut words = vec![0u32; FRAME_WORDS];
    words[0] = with_checksum((VECTOR_AT as u32) << 10);
    words[ADDRESS_AT] = p.capcode + SHORT_ADDRESS_OFFSET;
    words[VECTOR_AT] =
        with_checksum(VECTOR_ALPHA << 4 | (MESSAGE_AT as u32) << 7 | (message.len() as u32) << 14);
    let end = MESSAGE_AT + message.len();
    words[MESSAGE_AT..end].copy_from_slice(&message);
    for (n, w) in words.iter_mut().enumerate().skip(end) {
        *w = if n % 2 == 0 { 0 } else { IDLE_ONES };
    }
    words
}

/// The all-one data word, which alternates with the all-zero one as idle.
const IDLE_ONES: u32 = (1 << crate::bch::DATA_BITS) - 1;

/// One frame's bits, one `u8` (0 or 1) a bit, first first: [`FRAME_BITS`] of them.
/// `p` is assumed checked (the capcode a short address, at most [`TEXT_MAX`] ASCII
/// characters); the cycle and frame are taken modulo their field widths.
pub fn flex_frame_bits(p: &FlexParams) -> Vec<u8> {
    let mut out = header_bits(p.cycle, p.frame);
    for block in frame_words(p).chunks(BLOCK_WORDS) {
        let lines: Vec<[u8; WORD_BITS]> = block.iter().map(|&w| word_line_bits(w)).collect();
        for k in 0..WORD_BITS {
            out.extend(lines.iter().map(|l| l[k]));
        }
    }
    debug_assert_eq!(out.len(), FRAME_BITS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bch::bch3121_ok;

    fn params() -> FlexParams {
        FlexParams {
            capcode: 1_234_567,
            text: "HELLO ZETA".into(),
            cycle: 1,
            frame: 7,
        }
    }

    /// The offset of the first block bit.
    const BLOCKS_AT: usize = BIT_SYNC_BITS + 5 * 16 + WORD_BITS + SYNC2_BITS;

    /// The frame's 88 words as received, de-interleaved: each as the 32 line bits read
    /// most significant first (so a POCSAG-layout BCH word).
    fn line_words(bits: &[u8]) -> Vec<u32> {
        let mut words = Vec::new();
        for block in bits[BLOCKS_AT..].chunks(BLOCK_WORDS * WORD_BITS) {
            for j in 0..BLOCK_WORDS {
                words.push(
                    (0..WORD_BITS).fold(0u32, |a, k| (a << 1) | block[k * BLOCK_WORDS + j] as u32),
                );
            }
        }
        words
    }

    /// A line word's 21 data bits, `i0` least significant.
    fn data(w: u32) -> u32 {
        (w >> 11).reverse_bits() >> 11
    }

    #[test]
    fn a_frame_is_1875_ms_of_bits_at_1600() {
        assert_eq!(FRAME_BITS, 3000);
        assert_eq!(flex_frame_bits(&params()).len(), FRAME_BITS);
        assert_eq!(BLOCKS_AT + FRAME_WORDS * WORD_BITS, FRAME_BITS);
    }

    #[test]
    fn every_block_word_passes_bch() {
        let bits = flex_frame_bits(&params());
        let words = line_words(&bits);
        assert_eq!(words.len(), FRAME_WORDS);
        for (n, w) in words.iter().enumerate() {
            assert!(bch3121_ok(*w), "word {n}: {w:#010x}");
        }
        // The frame information word too, and it names the cycle and frame.
        let at = BIT_SYNC_BITS + 5 * 16;
        let fiw = bits[at..at + WORD_BITS]
            .iter()
            .fold(0u32, |a, &b| (a << 1) | b as u32);
        assert!(bch3121_ok(fiw));
        let d = data(fiw);
        assert_eq!((d >> 4) & 0xF, 1);
        assert_eq!((d >> 8) & 0x7F, 7);
    }

    #[test]
    fn the_address_and_vector_point_at_the_message() {
        let words: Vec<u32> = line_words(&flex_frame_bits(&params()))
            .into_iter()
            .map(data)
            .collect();
        let vector_at = ((words[0] >> 10) & 0x3F) as usize;
        assert_eq!(vector_at, 2, "one address word, then the vector field");
        assert_eq!(words[1], 1_234_567 + SHORT_ADDRESS_OFFSET);
        let v = words[vector_at];
        assert_eq!((v >> 4) & 0x7, VECTOR_ALPHA);
        let start = ((v >> 7) & 0x7F) as usize;
        let len = ((v >> 14) & 0x7F) as usize;
        assert_eq!(start, 3);
        assert_eq!((words[start] >> 11) & 0x3, FRAGMENT_ONLY);
        // The characters after the header: the signature, then the text, then zero fill.
        let chars: Vec<u8> = words[start + 1..start + len]
            .iter()
            .flat_map(|w| (0..3).map(move |k| ((w >> (7 * k)) & 0x7F) as u8))
            .collect();
        assert_eq!(chars[0], 0);
        assert_eq!(&chars[1..11], b"HELLO ZETA");
        assert!(chars[11..].iter().all(|&c| c == 0));
        assert_eq!(len, 1 + 11usize.div_ceil(3));
        // Every word that carries a checksum makes the sum.
        let sum = |w: u32| (0..6).map(|k| (w >> (4 * k)) & 0xF).sum::<u32>() & 0xF;
        for (n, &w) in [words[0], v].iter().enumerate() {
            assert_eq!(sum(w), CHECKSUM_SUM, "word {n}");
        }
    }

    #[test]
    fn the_longest_text_fills_the_frame() {
        let mut p = params();
        p.text = "A".repeat(TEXT_MAX);
        let words: Vec<u32> = line_words(&flex_frame_bits(&p))
            .into_iter()
            .map(data)
            .collect();
        let v = words[2];
        assert_eq!(((v >> 7) & 0x7F) + ((v >> 14) & 0x7F), FRAME_WORDS as u32);
    }
}
