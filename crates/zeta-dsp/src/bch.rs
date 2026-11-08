//! The BCH(31,21) code POCSAG and FLEX share: 21 data bits, 10 check bits from the
//! generator `x^10+x^9+x^8+x^6+x^5+x^3+1` ([`GENERATOR`]), then an even-parity bit.
//! The encoder lives here so the POCSAG and FLEX synthesisers use one; the check is the
//! POCSAG decoder's [`bch_ok`](crate::pocsag::bch_ok), computed separately, so the two
//! check each other.

/// The BCH(31,21) generator polynomial, `x^10+x^9+x^8+x^6+x^5+x^3+1`.
pub const GENERATOR: u32 = 0x769;
/// Data bits a word carries.
pub const DATA_BITS: u32 = 21;
/// Check bits the generator adds.
pub const CHECK_BITS: u32 = 10;
const DATA_MASK: u32 = (1 << DATA_BITS) - 1;

/// The 32-bit word for 21 data bits, POCSAG's layout: the data in bits 31..11 (bit 31
/// first on the air), the 10 check bits (the remainder of `data * x^10` modulo
/// [`GENERATOR`]) in bits 10..1, and bit 0 making the word's parity even. Bits of
/// `data21` above the 21st are ignored.
pub fn bch3121_word(data21: u32) -> u32 {
    let data = data21 & DATA_MASK;
    let mut r = data << CHECK_BITS;
    for i in (CHECK_BITS..CHECK_BITS + DATA_BITS).rev() {
        if r & (1 << i) != 0 {
            r ^= GENERATOR << (i - CHECK_BITS);
        }
    }
    let w = (data << (CHECK_BITS + 1)) | (r << 1);
    w | (w.count_ones() & 1)
}

/// True when `word` is a BCH(31,21) codeword with even parity (no correction): the
/// POCSAG decoder's [`bch_ok`](crate::pocsag::bch_ok).
pub fn bch3121_ok(word: u32) -> bool {
    crate::pocsag::bch_ok(word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pocsag::{IDLE, SYNC};

    #[test]
    fn bch3121_words_pass_the_pocsag_check() {
        for data in [0, 1, 0x1F_FFFF, 0x12345] {
            assert!(bch3121_ok(bch3121_word(data)), "{data:#x}");
        }
        // POCSAG's sync and idle codewords are the words of their own data bits.
        for w in [SYNC, IDLE] {
            assert_eq!(bch3121_word(w >> (CHECK_BITS + 1)), w, "{w:#010x}");
        }
        // A single flipped bit fails the check.
        assert!(!bch3121_ok(bch3121_word(0x12345) ^ (1 << 7)));
    }
}
