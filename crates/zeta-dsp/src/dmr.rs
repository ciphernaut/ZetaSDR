//! DMR (Digital Mobile Radio, ETSI TS 102 361): its codes, its bursts, a framer that turns
//! the 4-level FSK front end's soft dibits ([`crate::fsk4`]) into bursts, and encoders for a
//! repeater's downlink and a handheld's bursts for the `dmr` synthesiser family.
//!
//! **Sources.** ETSI TS 102 361-1 V2.5.1 (2017-10), the air interface ("-1" below), and
//! TS 102 361-2 V2.4.1 (2017-10), the voice and generic services ("-2"), both freely
//! published by ETSI and read for this module. Everything below comes from them except the
//! inside of the AMBE+2 vocoder frame, which they do not define (its own paragraph says
//! where that comes from). Where the standard prints a table, a matrix or a worked example,
//! a test holds the code to it; the rest is round-tripped only (the report of plan 38's
//! task 7 lists which is which).
//! - **Modulation** (-1 10.2.1, 10.2.2): 4800 symbols/s, deviation index 0.27 (±1944 and
//!   ±648 Hz), root raised cosine of roll-off 0.2, the dibit mapping of table 10.3; see
//!   [`crate::fsk4`].
//! - **Bursts** (-1 4.2.2, 6.1, 6.2, annex E): 264 bits in 27.5 ms, the first symbol sent L66:
//!   108 payload bits, a 48-bit centre field, 108 payload bits. A voice burst's payload is
//!   the vocoder socket; its centre a sync (burst A) or the 16-bit EMB around 32 bits of
//!   embedded signalling (B to F). A data burst's payload is a BPTC (196,96) word in 98 and
//!   98 bits around the 20-bit slot type, split 10 and 10 either side of the sync. Tests
//!   compare every bit of a data burst with table E.1 and the centre of a voice burst with
//!   table E.6.
//! - **Syncs** (-1 9.1.1, table 9.2): BS (repeater) voice [`DMR_SYNC_BS_VOICE`] and data
//!   [`DMR_SYNC_BS_DATA`], MS (handheld) voice [`DMR_SYNC_MS_VOICE`] and data
//!   [`DMR_SYNC_MS_DATA`]; each data sync is its voice sync with every symbol negated. The
//!   standalone reverse channel sync and the four TDMA direct mode syncs are not searched.
//! - **TDMA** (-1 4.2.2, 4.5, 4.6.1, 5.1.4): a repeater sends continuously, a 24-bit CACH
//!   before each burst, so a slot's bursts are 60 ms apart; a slot with nothing to send
//!   carries Idle bursts (7.3). A handheld sends one burst in each 60 ms, 27.5 ms on and the
//!   rest off.
//! - **CACH** (-1 6.3, 9.1.4, 9.3.8, 9.3.9, B.3.5, B.4.1): the TACT (AT, TC, LCSS) is a
//!   Hamming (7,4,3) word spread over the 24 bits as figure B.9 places them; TC names the
//!   burst that follows (table 9.24: 0 slot 1, 1 slot 2). The 17 payload bits carry short
//!   link control (7.1.4) that the framer does not read. The encoder sends the Null message
//!   (-2 7.1.3.1: SLCO 0, data 0) in four CACHs, LCSS first, continuation, continuation,
//!   last; its payload bits are all zero, since its CRC-8 (B.3.7, initial remainder 0) of
//!   zero is zero and the BPTC of B.2.3 is linear.
//! - **EMB** (-1 9.1.2, 9.3.1 to 9.3.4, B.3.2): colour code, PI and LCSS in a quadratic
//!   residue (16,7,6) word (generator `x^8 + x^5 + x^4 + x^3 + 1`), two errors corrected.
//! - **Slot type** (-1 9.1.3, 9.3.6, 9.3.7, B.3.1): colour code and data type (table 9.22)
//!   in a Golay (20,8,7) word (generator `x^11 + x^10 + x^6 + x^5 + x^4 + x^2 + 1`), three
//!   errors corrected.
//! - **BPTC (196,96)** (-1 B.1.1): R(2) to R(0), zero, then the 96 bits in a 9 × 11 matrix,
//!   each row a Hamming (15,11,3) word, each of the 15 columns a Hamming (13,9,3) word
//!   (B.3.4, generator `x^4 + x + 1`, shortened for the columns); R(3), zero, then the
//!   matrix row by row numbered 0 to 195, bit `i` sent `i × 181 mod 196`-th (B.1). Decoding
//!   alternates column and row corrections. Tests hold the encoder to figure D.1 (the Idle
//!   message encoded), the interleave to a sample of table B.2, and every bit's place in
//!   the burst to table E.1.
//! - **Full link control** (-1 7.1, 9.1.6, 9.3.10, 9.3.11; -2 7.1.1.1, 7.1.1.2, 7.2.1): PF,
//!   a reserved bit, FLCO (0 group voice, 3 unit-to-unit voice), FID, service options
//!   (emergency, privacy), the 24-bit group or target address, the 24-bit source. In a
//!   header or terminator it is followed by a Reed-Solomon (12,9) parity over GF(2^8)
//!   (B.3.6: `x^8 + x^4 + x^3 + x^2 + 1`, generator roots `α` to `α^3`, one symbol
//!   corrected), masked by the data type's CRC mask (B.3.12, table B.21: `969696` for the
//!   voice LC header, `999999` for the terminator), then BPTC (196,96). A test holds the
//!   code to the generator (B.9, B.10), every row of table B.18 and entries of tables B.19
//!   and B.20. Any FID is accepted with the two opcodes; a word with PF set, which -1
//!   reserves, is not read.
//! - **The binary codes' generator matrices** (-1 tables B.11, B.12 and B.14 to B.17): a
//!   test compares every row with the encoders, which are written from the polynomials.
//! - **Embedded link control** (-1 7.1.3, B.2.1, B.3.11): the 72 bits and a 5-bit checksum
//!   (the octets' sum mod 31) in 7 rows of a Hamming (16,11,4) code (B.3.4) and a row of
//!   column parity, read out column by column as four 32-bit fragments, carried by bursts
//!   B to E with LCSS first, continuation, continuation, last; burst F carries the Null
//!   embedded message (D.1: all zero, LCSS single fragment).
//! - **Other embedded messages** (-2 5.2.1.2, 5.2.2.2, tables 5.4 and 5.6; 7.1.1.3 to
//!   7.1.1.5): in some superframes a radio sends a talker alias (FLCO `000100` to `000111`)
//!   or its position (GPS, `001000`) in bursts B to E in place of the link control, coded
//!   the same way. The framer reports such a message by its FLCO ([`DmrContent::Voice`]'s
//!   `other`) and keeps the call's link control. The encoder can send a talker alias
//!   header (7.1.1.4, table 7.4: the 7-bit format of 7.2.18, an invented alias) in every
//!   second superframe ([`encode_repeater_aliased`]).
//! - **Voice superframe** (-1 5.1.2): an optional voice LC header, superframes of bursts A to
//!   F (360 ms), then a terminator with link control. Idle bursts carry table D.2's 96 fill
//!   bits.
//! - **Vocoder socket** (-1 6.1): 216 bits, VS(215) first; three 72-bit frames, VF(1)'s
//!   most significant bit first.
//!
//! - **Privacy header and CSBK** (-1 6.2, 7.2, 9.1.8, B.3.8, table B.21): 80 bits and a
//!   CRC-CCITT (`x^16 + x^12 + x^5 + 1`, initial remainder 0, inverted) under the data
//!   type's mask (`6969`, `A5A5`), then BPTC (196,96). The framer reports them by their data
//!   type and does not read them. The encoder's privacy header carries 80 pseudo-random
//!   bits, since -1 does not define its contents; its control blocks are Preamble CSBKs (-2 7.1.2.5).
//!
//! **Encryption.** [`FullLc::encrypted`] is the privacy bit of the service options (-2
//! table 7.11, which does not define privacy further); an encrypted call also sends a
//! privacy header after its voice LC header (-1 5.1.2.2, figure 5.6). The EMB's PI bit is
//! not an encryption mark: -1 V2.5.1 names it the pre-emption and power control indicator
//! (9.3.2, table 9.19: 1 means the embedded signalling carries reverse channel information
//! for the other logical channel; the table's caption and earlier editions call it the
//! privacy indicator). [`DmrContent::Voice`]'s `pi` is that bit as received; the encoder
//! sends 0. Nothing here decrypts.
//!
//! **The AMBE+2 frame: decoders' values, not the standard's.** -1 places whole 72-bit
//! frames (6.1) and says nothing of their inside. [`AmbeSoft`] holds a frame as
//! `zeta-mbe ambe3600x2450` (and mbelib-neo 2.6.11, the library it links) takes it: 4 rows
//! of 24 cells, one soft byte per bit (0 a certain zero, 255 a certain one), the unused
//! cells 0, the mask still on. Taken from where the named code agrees, value by value:
//! - The interleave of the 72 bits into the rows is DSD's (`dmr_const.h`, ISC licence; the
//!   same values, credited to DSD, are in mbelib-neo's tools): a test compares it entry
//!   by entry. Here it is a derivation: bit `n` of the rows in word order (row 0's 24 from
//!   cell 23 down, row 1's 23 from cell 22, row 2's 11 from cell 10, row 3's 14 from
//!   cell 13) is sent `4 (n mod 18) + n / 18`-th.
//! - The rows are mbelib-neo's (`ambe_common.c`, read for values only, nothing copied):
//!   row 0 the Golay (24,12) word of the first 12 parameter bits (cells 23 to 1 the
//!   (23,12) word top bit first, cell 0 its even parity), row 1 the Golay (23,12) word of
//!   the next 12 under a pseudo-random mask seeded from row 0's data (P25's IMBE mask:
//!   `p(0) = 16 u0`, `p(n) = (173 p(n−1) + 13849) mod 65536`, the mask bit
//!   `p(n) ≥ 32768`, over cells 22 down to 0), rows 2 and 3 the last 11 and 14 bits bare.
//!   The Golay code is P25's, generator `0xC75`: the polynomial -1 gives for the slot
//!   type's Golay (20,8) (B.4), though -1 does not say the vocoder uses it. A test
//!   compares it with [`crate::m17`]'s Golay (24,12) encoder.
//! - **The voice payload** of an encoded call is coded here from 49 parameter bits that
//!   differ from frame to frame and from call to call (`voice_payload`, a SplitMix64 step
//!   of the calling radio's ID and the frame's number): not speech, and no codec made it,
//!   but varied as speech's frames are, so no burst sits on one level, which the receiver
//!   would refuse as a carrier, and two calls at once never carry the same frames (a
//!   receiver may take a burst that repeats the last one for a stuck carrier). A DMR silence
//!   frame is widely quoted, from hotspot software, but no public source that states it as
//!   the standard's was found, so it is not used.
//!
//! **The framer.** It searches the four syncs and takes the rest of a burst by position:
//! each dibit's [`SoftDibit::at`], never a count of dibits, since the front end gives held
//! symbols out late and a handheld's bursts each start a fresh acquisition. The symbol
//! period is measured from the spacing of the dibits' positions. A handheld's burst has no
//! slot (`slot` is `None`).
//! - **A repeater's slots.** The first sync takes its slot from the CACH's TC. Once both
//!   slots are timed (below), a sync's slot is the grid's: the slot whose bursts fall within
//!   a few symbols of it. A TACT is a Hamming (7,4) word, and that code is perfect, so two
//!   wrong bits decode to a wrong word that can name the other slot; a misread TC must not
//!   move a slot's timing or end its call. Only when the TC names the other slot on three
//!   syncs running (`GRID_OVERRIDE_SYNCS`) is it trusted and the grid rebuilt from it, so a
//!   repeater that was re-timed is followed.
//! - **The flywheel.** Any sync sets its slot's timing, and on a repeater the other slot's
//!   too (half a period off, its slot the other one). From then on each slot's bursts are
//!   read every 60 ms by position, with or without a burst A. A burst there with no sync
//!   is reported as voice only when its EMB decodes, names the colour code (on a repeater,
//!   either slot's: a repeater has one) and, once the superframe's phase is known, carries
//!   the LCSS of its letter (B first, C and D continuation, E last, F single). In a
//!   superframe whose burst A was not heard it must also, with a link control in hand,
//!   carry that link control's fragment for its letter or, the phase unknown, for the place
//!   its LCSS names (burst F's: the null message), within 4 of 32 bits; with no link control
//!   in hand, its EMB must decode with no bit corrected. So noise after a call is not taken
//!   for voice. A slot's timing is kept alive only by a sync or an EMB as sent (not by one
//!   the code corrected) and dropped after two superframes without either; after a data
//!   burst other than a header no voice is read without a new sync.
//! - **Colour code.** Learnt from a slot type, or from an EMB that decoded with no bit
//!   corrected, never from one the code corrected, since the code turns about a quarter of
//!   all words into a valid one.
//! - **The embedded link control** is assembled from the four latest fragments when they
//!   are the four places B to E in order. Within a superframe that is always so. Across a
//!   superframe's boundary it is so only while the call's link control is not yet in hand
//!   (at a join), with no row needing a correction, and only for two of the three orders:
//!   C, D, E then B, and E then B, C, D. The third, D, E then B, C, is never assembled.
//!   The reason is the row code. A fragment holds four of the 16 columns (B 0 to 3, C 4 to
//!   7, D 8 to 11, E 12 to 15), and a mixed row is the old row with the new one's columns,
//!   so it is a code word only when the two rows' difference on those columns is one. No
//!   non-zero code word lies inside columns 0 to 3 or inside 12 to 15 (a test enumerates
//!   them), so in the two orders kept a mix that passes is the old or the new link control
//!   exactly. Code words do lie inside columns 0 to 7, so in the third order a mix of two
//!   link controls could pass every row and be held back by the 5-bit checksum alone; a
//!   test builds such a pair, whose mix passes the checksum too and names neither call.
//!   Once the link control is in hand only a whole superframe's B to E are taken, so a call
//!   that changes at a boundary gives the new link control at its own burst E. Dropping
//!   the third order costs a receiver that joins just past a burst C's fragment: it has
//!   the link control at the next superframe's D instead of its C.
//!
//! Polarity: a data burst whose slot type and BPTC both check tells it, and
//! [`DmrFramer::inverted`] says it, as P25's framer does.

use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::fsk4::{SoftDibit, sync_dibits};
use crate::util::splitmix64;

/// Whether a group or a unit-to-unit call (shared with the other voice stages).
pub use crate::voice_call::Call;

/// DMR's symbol rate, baud (-1 10.2.1).
pub const DMR_BAUD: u32 = 4800;
/// The outer symbols' deviation, Hz (-1 10.2.2.1); the inner ones a third of it.
pub const DMR_DEVIATION_HZ: f64 = 1944.0;
/// The root-raised-cosine roll-off (-1 10.2.2.2).
pub const DMR_RRC_ALPHA: f64 = 0.2;
/// The most searches a DMR receiver's 4-level FSK lock coasts through stretches that sit
/// on one level ([`crate::fsk4::Fsk4Params::coast_searches`]). A voice burst's 48-bit
/// centre (the EMB and the embedded field) can be 24 dibits of 00 at colour code 0, where
/// the EMB's QR code of zero data is zero. How long a lock coasts also depends on the
/// voice payload around those centres, which speech varies, so it is measured over many
/// payloads: at colour code 0, handheld bursts at 0.8 of the deviation and 10 dB, 100
/// noise seeds for each of 20 calls from different sources (each seeding its payload,
/// `voice_payload`), with the receiver's own coast at 12, 14, 15 and 16 and with one that
/// never runs out (the figures the same); and the repeater call at 15 and 10 dB:
/// - the most searches a proven lock coasted in a row: 5 (4 for most payloads);
/// - the most a lock on probation coasted in all: 10 (4 or 5 for most payloads, 9 and 10
///   for two; 11 for one payload sent under another source's link control, which a call
///   never is but a payload could be; none on the repeater past 2, whose lock is proven
///   early).
///
/// The bound is the larger of the proven worst and a stretch's 4 searches more at each
/// end, where noise tips a stretch of 18 or 19 on the level over (5 + 8 = 13), and the
/// probation worst and one stretch more (11 + 4 = 15): 15. (It was 12, sized on one
/// payload, whose probation worst was 5.)
pub const DMR_COAST_SEARCHES: u32 = 15;
/// Bytes of one AMBE+2 frame as the vocoder takes it: 4 rows of 24, one per bit.
pub const AMBE_FEED_BYTES: usize = 96;
/// A voice superframe, ms: six bursts of a slot (-1 5.1.2.1).
pub const DMR_SUPERFRAME_MS: u64 = 360;
/// A repeater's voice sync (-1 table 9.2), first bit first from bit 47.
pub const DMR_SYNC_BS_VOICE: u64 = 0x755F_D7DF_75F7;
/// A repeater's data sync.
pub const DMR_SYNC_BS_DATA: u64 = 0xDFF5_7D75_DF5D;
/// A handheld's voice sync.
pub const DMR_SYNC_MS_VOICE: u64 = 0x7F7D_5DD5_7DFD;
/// A handheld's data sync.
pub const DMR_SYNC_MS_DATA: u64 = 0xD5D7_F77F_D757;

const BURST_BITS: usize = 264;
const BURST_DIBITS: usize = 132;
const CACH_BITS: usize = 24;
const CACH_DIBITS: usize = 12;
/// From a burst's first dibit (L66) to its centre field's first (L12).
const LEFT_DIBITS: usize = 54;
const SYNC_DIBITS: usize = 24;
const SYNC_BITS: u32 = 48;
/// From one burst of a slot to its next: two 30 ms slots (a repeater's CACH and burst, or
/// a handheld's burst and guard time, -1 4.2.2).
const SLOT_PERIOD_DIBITS: usize = 2 * (BURST_DIBITS + CACH_DIBITS);
/// Payload bits either side of the centre field.
const HALF_BITS: usize = 108;
const CENTRE_BITS: usize = 48;
/// A data burst's BPTC bits either side of its slot type.
const DATA_HALF_BITS: usize = 98;
const SLOT_TYPE_HALF_BITS: usize = 10;
const SLOT_TYPE_BITS: u32 = 20;
const EMB_HALF_BITS: usize = 8;
const EMB_BITS: u32 = 16;
const EMBEDDED_BITS: usize = 32;
const VOICE_BURSTS: usize = 6;
/// Dibits of a sync that may be wrong.
const SYNC_MAX_ERRORS: usize = 4;
/// How near a burst's expected place a sync stands in for it, symbols.
const SAME_BURST_SYMBOLS: f64 = 4.0;
/// Symbol spacings measured before the framer places bursts by their mean.
const SPACING_COUNT: usize = 32;
/// A symbol that is not there: both bits at the midpoint.
const ERASURE: [u8; 2] = [128, 128];

/// Data types (-1 table 9.22).
const DT_PI_HEADER: u8 = 0b0000;
const DT_VOICE_LC_HEADER: u8 = 0b0001;
const DT_TERMINATOR_LC: u8 = 0b0010;
const DT_CSBK: u8 = 0b0011;
const DT_IDLE: u8 = 0b1001;
/// The CRC masks of the voice LC header and the terminator (-1 table B.21).
const MASK_VOICE_LC_HEADER: u32 = 0x96_9696;
const MASK_TERMINATOR_LC: u32 = 0x99_9999;
/// LC start/stop (-1 table 9.20).
const LCSS_SINGLE: u8 = 0b00;
const LCSS_FIRST: u8 = 0b01;
const LCSS_LAST: u8 = 0b10;
const LCSS_CONTINUATION: u8 = 0b11;
/// A short LC's fragments over four CACHs.
const SHORT_LC_LCSS: [u8; 4] = [LCSS_FIRST, LCSS_CONTINUATION, LCSS_CONTINUATION, LCSS_LAST];
/// The embedded link control's fragments over bursts B to E.
const EMBEDDED_LCSS: [u8; 4] = [LCSS_FIRST, LCSS_CONTINUATION, LCSS_CONTINUATION, LCSS_LAST];

/// Full link control (-1 figure 7.1; -2 tables 7.1, 7.2, 7.11).
const LC_BYTES: usize = 9;
const LC_PF: u8 = 0x80;
const FLCO_MASK: u8 = 0x3F;
const FLCO_GROUP: u8 = 0x00;
const FLCO_PRIVATE: u8 = 0x03;
/// The talker alias header's FLCO (-2 table 5.4, 7.1.1.4) and its 7-bit format (7.2.18).
const FLCO_TALKER_ALIAS_HEADER: u8 = 0b00_0100;
const TA_FORMAT_7BIT: u8 = 0b00;
/// The GPS Info message's FLCO (-2 table 5.4, 7.1.1.3).
#[cfg(test)]
const FLCO_GPS_INFO: u8 = 0b00_1000;
/// The alias the encoder sends: invented, at most 7 characters (one header, -2 5.4.3).
const TALKER_ALIAS: &str = "ZETA";
const SVC_EMERGENCY: u8 = 0x80;
const SVC_PRIVACY: u8 = 0x40;
const ID_MASK: u32 = 0xFF_FFFF;

/// BPTC (196,96) (-1 B.1.1).
const BPTC_BITS: usize = 196;
const BPTC_INFO_BITS: usize = 96;
const BPTC_ROWS: usize = 13;
const BPTC_COLS: usize = 15;
const BPTC_DATA_ROWS: usize = 9;
const BPTC_DATA_COLS: usize = 11;
/// R(2) to R(0) before the information bits.
const BPTC_RESERVED: usize = 3;
const BPTC_INTERLEAVE: usize = 181;
/// Column-then-row correction passes.
const BPTC_PASSES: usize = 4;
/// The Idle message's fill bits (-1 table D.2), I(95) first.
const IDLE_FILL: [u8; 12] = [
    0xFF, 0x83, 0xDF, 0x17, 0x32, 0x09, 0x4E, 0xD1, 0xE7, 0xCD, 0x8A, 0x91,
];

/// Embedded link control (-1 B.2.1): 8 rows of 16.
const EMB_LC_ROWS: usize = 8;
const EMB_LC_COLS: usize = 16;
const EMB_LC_FRAGMENTS: usize = 4;
const CS_MODULUS: u32 = 31;

/// The TACT's seven bits among the CACH's 24 (-1 figure B.9), AT first.
const TACT_PLACES: [usize; 7] = [0, 4, 8, 12, 14, 18, 22];

/// The AMBE+2 frame: bits per row.
const AMBE_FRAME_BITS: usize = 72;
const AMBE_COLS: usize = 24;
const AMBE_ROW_BITS: [usize; 4] = [24, 23, 11, 14];
/// The interleave's streams: four of 18 bits.
const AMBE_STREAMS: usize = 4;
const AMBE_MASK_SEED: u32 = 16;
const AMBE_MASK_MUL: u32 = 173;
const AMBE_MASK_ADD: u32 = 13849;
const AMBE_MASK_TOP: u32 = 0x8000;

/// The flywheel (see [`DmrFramer`]): periods without a sync or a valid EMB before a slot's
/// timing is dropped (two superframes).
const FLYWHEEL_PERIODS: i64 = 2 * VOICE_BURSTS as i64;
/// Embedded bits that may differ from the known link control's in a superframe whose burst
/// A was not heard.
const UNANCHORED_MAX_DIFFER: usize = 4;
/// Embedded fragments a track keeps: two superframes' worth.
const KEPT_FRAGMENTS: usize = 8;
/// The letters a cross-boundary assembly may end on: B (C, D, E then B) and D (E then B, C,
/// D). Not C: D, E then B, C can mix two link controls (module doc).
const CROSS_BOUNDARY_ENDS: [usize; 2] = [1, 3];
/// Repeater syncs running whose CACH names the other slot than the timing grid before the
/// grid is rebuilt from the CACH: three, so one or two misread TACTs (both slots' syncs
/// count) never move a slot, and a repeater that was re-timed is followed within about
/// 90 ms of its idle bursts.
const GRID_OVERRIDE_SYNCS: usize = 3;
/// Each burst's LCSS by its letter, A to F (A has a sync, not an EMB).
const EXPECTED_LCSS: [Option<u8>; VOICE_BURSTS] = [
    None,
    Some(LCSS_FIRST),
    Some(LCSS_CONTINUATION),
    Some(LCSS_CONTINUATION),
    Some(LCSS_LAST),
    Some(LCSS_SINGLE),
];

/// A CSBK or a privacy header: 80 bits, then a 16-bit CRC-CCITT (-1 table 9.9, B.3.8).
const BLOCK_DATA_BYTES: usize = 10;
/// `x^16 + x^12 + x^5 + 1` (B.18).
const CRC_CCITT_POLY: u16 = 0x1021;
/// The CRC masks of the privacy header and the CSBK (-1 table B.21).
const MASK_PI_HEADER: u16 = 0x6969;
const MASK_CSBK: u16 = 0xA5A5;
/// A CSBK's last block bit, the Preamble CSBK's opcode, its group target bit (-2 table 7.7).
const CSBK_LAST_BLOCK: u8 = 0x80;
const CSBKO_PREAMBLE: u8 = 0b11_1101;
const CSBK_GROUP: u8 = 0x40;
/// The SplitMix64 input of the encoder's privacy header contents.
const PI_HEADER_SEED: u64 = 0x5049;

/// Who sent a burst: a repeater (BS-sourced syncs, a CACH between bursts) or a handheld
/// (MS-sourced syncs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Repeater,
    Handheld,
}

/// A voice call's full link control. `dst` is the talkgroup of a group call or the called
/// radio of a private one, `src` the calling radio (24 bits each); `encrypted` and
/// `emergency` are the service options' privacy and emergency bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FullLc {
    pub call: Call,
    pub dst: u32,
    pub src: u32,
    pub fid: u8,
    pub encrypted: bool,
    pub emergency: bool,
}

/// One AMBE+2 frame as `zeta-mbe ambe3600x2450` reads it: mbelib's 4 rows of 24, one soft
/// byte per bit, the mask still on, the unused cells 0.
pub type AmbeSoft = [u8; AMBE_FEED_BYTES];

/// What a burst carries. A link control that does not decode is `None`.
#[derive(Clone, Debug, PartialEq)]
pub enum DmrContent {
    VoiceHeader(Option<FullLc>),
    /// A voice burst: `first` for burst A (the sync); `letter` its place in its superframe,
    /// 0 for A to 5 for F, once the superframe's phase is known (burst A's is 0); `lc` the
    /// embedded link control, on the burst that completes it (E, or, while the call's is
    /// not yet in hand, B or D across a superframe's boundary); `other` the FLCO of an
    /// embedded message that is not a voice channel user's link control (a talker alias or
    /// GPS, -2 table 5.4), on the burst that completes it, the call's link control standing
    /// as it was; `pi` the EMB's PI bit (`false` on burst A, which has no EMB), the
    /// pre-emption and power control indicator, not an encryption mark; `errors` the bits
    /// corrected by the EMB's code, the embedded link control's and the three frames' Golay
    /// words.
    Voice {
        first: bool,
        letter: Option<u8>,
        lc: Option<FullLc>,
        other: Option<u8>,
        pi: bool,
        voice: Box<[AmbeSoft; 3]>,
        errors: u32,
    },
    Terminator(Option<FullLc>),
    PrivacyHeader,
    /// A control signalling block (not parsed).
    Control,
    Idle,
    /// Another data type (-1 table 9.22).
    Other(u8),
}

/// A burst: `at` is the sample position of its first symbol (L66), placed from its centre
/// field's by the measured symbol period; `slot` 1 or 2 from a repeater's CACH, `None`
/// from a handheld; `color_code` from the slot type or the EMB, `None` on burst A.
#[derive(Clone, Debug, PartialEq)]
pub struct DmrBurst {
    pub at: f64,
    pub slot: Option<u8>,
    pub origin: Origin,
    pub color_code: Option<u8>,
    pub content: DmrContent,
}

/// What a sync says of its burst.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Voice,
    Data,
}

fn sync_word(origin: Origin, kind: Kind) -> u64 {
    match (origin, kind) {
        (Origin::Repeater, Kind::Voice) => DMR_SYNC_BS_VOICE,
        (Origin::Repeater, Kind::Data) => DMR_SYNC_BS_DATA,
        (Origin::Handheld, Kind::Voice) => DMR_SYNC_MS_VOICE,
        (Origin::Handheld, Kind::Data) => DMR_SYNC_MS_DATA,
    }
}

/// What is known of a slot's call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackState {
    /// Timing only, from the other slot's sync: a call may be in progress.
    Unknown,
    /// A call is in progress: a voice sync, a voice LC header or a valid EMB was heard.
    Voice,
    /// The slot's last sync was a data burst's other than a header's: no call.
    Data,
}

/// A slot's timing between syncs (the flywheel), and what is known of the call on it.
/// Bursts are numbered by period (60 ms) from the first heard.
struct Track {
    slot: Option<u8>,
    origin: Origin,
    /// The centre field of one of the slot's bursts, heard or placed, and its period.
    anchor: f64,
    anchor_period: i64,
    /// The period of the next burst to read.
    next: i64,
    /// The last period with a sync or a valid EMB.
    last_good: i64,
    state: TrackState,
    /// The period of a burst A, which sets every burst's letter, when known.
    phase: Option<i64>,
    /// The period of the last burst A whose sync was heard.
    heard_a: Option<i64>,
    /// The call's link control, from its header or its embedded signalling.
    lc: Option<FullLc>,
    /// Embedded fragments (from bursts B to E), each with its period, oldest first.
    fragments: Vec<(i64, Vec<u8>)>,
}

impl Track {
    fn new(slot: Option<u8>, origin: Origin, anchor: f64, state: TrackState) -> Self {
        Track {
            slot,
            origin,
            anchor,
            anchor_period: 0,
            next: 1,
            last_good: 0,
            state,
            phase: None,
            heard_a: None,
            lc: None,
            fragments: Vec::new(),
        }
    }

    /// Where `period`'s centre field is.
    fn place(&self, period: i64, sps: f64) -> f64 {
        self.anchor + ((period - self.anchor_period) * SLOT_PERIOD_DIBITS as i64) as f64 * sps
    }

    /// A sync heard on the slot at `centre`: its period, the anchor moved to it.
    fn heard(&mut self, centre: f64, sps: f64) -> i64 {
        let periods = (centre - self.anchor) / (SLOT_PERIOD_DIBITS as f64 * sps);
        let period = self.anchor_period + periods.round() as i64;
        self.anchor = centre;
        self.anchor_period = period;
        self.next = period + 1;
        self.last_good = period;
        period
    }

    /// `period`'s burst in its superframe: 0 for A to 5 for F, when the phase is known.
    fn letter(&self, period: i64) -> Option<usize> {
        self.phase
            .map(|p| (period - p).rem_euclid(VOICE_BURSTS as i64) as usize)
    }

    /// Whether `period`'s superframe began with a burst A whose sync was heard.
    fn anchored(&self, period: i64) -> bool {
        let (Some(phase), Some(a)) = (self.phase, self.heard_a) else {
            return false;
        };
        let six = VOICE_BURSTS as i64;
        a == phase + (period - phase).div_euclid(six) * six
    }

    /// A data burst heard: no call follows unless it is a voice LC header.
    fn data(&mut self, state: TrackState, lc: Option<FullLc>) {
        self.state = state;
        self.lc = lc;
        self.phase = None;
        self.heard_a = None;
        self.fragments.clear();
    }

    /// The embedded message of the four latest fragments when they are the four places in
    /// order, B to E: of one superframe, or, while the call's link control is not yet in
    /// hand, across a superframe's boundary in the two orders the row code makes safe
    /// (C, D, E then B; E then B, C, D). The embedded code's row and column parity and its
    /// checksum decide.
    fn assemble(&self) -> Option<Embedded> {
        let phase = self.phase?;
        let six = VOICE_BURSTS as i64;
        let placed: Vec<(i64, usize, &Vec<u8>)> = self
            .fragments
            .iter()
            .filter_map(|(period, f)| {
                let rel = period - phase;
                let letter = rel.rem_euclid(six) as usize;
                (1..=EMB_LC_FRAGMENTS)
                    .contains(&letter)
                    .then(|| (rel.div_euclid(six) * 4 + letter as i64 - 1, letter, f))
            })
            .collect();
        let &(last, last_letter, _) = placed.last()?;
        // Across a boundary only where the newer superframe gives B alone or B, C and D:
        // there the row code leaves no mix but the old or the new link control (module doc).
        let across = last_letter != EMB_LC_FRAGMENTS;
        if across && (self.lc.is_some() || !CROSS_BOUNDARY_ENDS.contains(&last_letter)) {
            return None;
        }
        let mut by_letter: Vec<Vec<u8>> = vec![Vec::new(); EMB_LC_FRAGMENTS];
        for place in last - 3..=last {
            let (_, letter, f) = placed.iter().rev().find(|(p, ..)| *p == place)?;
            by_letter[letter - 1] = f.to_vec();
        }
        let (bytes, errors) = embedded_decode(&by_letter)?;
        // Across a boundary every row must be a code word as received (see the module doc).
        if across && errors != 0 {
            return None;
        }
        match parse_lc(&bytes) {
            Some(lc) => Some(Embedded::Lc(lc, errors)),
            None => other_flco(&bytes).map(|flco| Embedded::Other(flco, errors)),
        }
    }

    /// The burst at `period`, which has no sync, read by the flywheel: its EMB's data and
    /// the bits it corrected, and the link control it completes with the bits that took.
    /// `None` when the EMB does not decode, names another colour code than `known`, or does
    /// not fit the burst's letter; in a superframe whose burst A was not heard, when the
    /// embedded bits are not the known link control's fragment (or burst F's null message)
    /// or, with no link control in hand, when the EMB needed a correction.
    fn flywheel(
        &mut self,
        bits: &[u8; BURST_BITS],
        known: Option<u8>,
        period: i64,
    ) -> Option<FlywheelRead> {
        let (data, emb_errors) = QR16.decode(emb_word_of(bits))?;
        if known.is_some_and(|cc| u32::from(cc) != data >> 3) {
            return None;
        }
        let lcss = (data & 3) as u8;
        let embedded = &bits[HALF_BITS + EMB_HALF_BITS..][..EMBEDDED_BITS];
        let letter = self.letter(period);
        if letter.is_some_and(|l| EXPECTED_LCSS[l] != Some(lcss)) {
            return None;
        }
        if !self.anchored(period) {
            match self.lc {
                // The embedded bits must be the link control's fragment for the letter or, the
                // phase unknown, for the place the LCSS names (burst F's: the null message).
                Some(lc) => {
                    let fragments = embedded_encode(&lc_bytes(&lc));
                    let places: &[usize] = match (letter, lcss) {
                        (Some(l @ 1..=4), _) => &[l - 1][..],
                        (Some(_), _) | (None, LCSS_SINGLE) => &[EMB_LC_FRAGMENTS][..],
                        (None, LCSS_FIRST) => &[0][..],
                        (None, LCSS_LAST) => &[3][..],
                        (None, _) => &[1, 2][..],
                    };
                    let matches = |place: &usize| {
                        let want = fragments.get(*place).copied().unwrap_or([0; EMBEDDED_BITS]);
                        let differ = embedded
                            .iter()
                            .zip(want)
                            .filter(|&(s, w)| u8::from(*s >= 128) != w)
                            .count();
                        differ <= UNANCHORED_MAX_DIFFER
                    };
                    if !places.iter().any(matches) {
                        return None;
                    }
                }
                // With nothing to compare, only an EMB as sent.
                None if emb_errors != 0 => return None,
                None => {}
            }
        }
        if letter.is_none() {
            match lcss {
                LCSS_FIRST => self.phase = Some(period - 1),
                LCSS_LAST => self.phase = Some(period - 4),
                _ => {}
            }
        }
        self.state = TrackState::Voice;
        // Only an EMB as sent keeps the timing alive: the code turns about a quarter of all
        // words into one it corrects.
        if emb_errors == 0 {
            self.last_good = period;
        }
        let mut message = None;
        if lcss != LCSS_SINGLE {
            self.fragments.push((period, embedded.to_vec()));
            if self.fragments.len() > KEPT_FRAGMENTS {
                self.fragments.remove(0);
            }
            message = self.assemble();
            // Another message (a talker alias, GPS) leaves the call's link control as it was.
            if let Some(Embedded::Lc(found, _)) = message {
                self.lc = Some(found);
            }
        }
        Some(FlywheelRead {
            emb: data,
            emb_errors,
            letter: self.letter(period),
            message,
        })
    }
}

/// An embedded message assembled from bursts B to E.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Embedded {
    /// A voice channel user's link control, and the bits that took.
    Lc(FullLc, u32),
    /// Another message, by its FLCO (-2 table 5.4: a talker alias, GPS), and the bits that
    /// took.
    Other(u8, u32),
}

/// What the flywheel took from a burst: its EMB's data and the bits it corrected, its
/// letter once the phase is known, and the embedded message the burst completed.
struct FlywheelRead {
    emb: u32,
    emb_errors: u32,
    letter: Option<usize>,
    message: Option<Embedded>,
}

/// The symbol period, measured from the dibits' positions.
#[derive(Default)]
struct Spacing {
    last_at: Option<f64>,
    /// The first steps, whose median sets which steps count.
    first: Vec<f64>,
    nominal: Option<f64>,
    sum: f64,
    count: usize,
}

impl Spacing {
    fn learn(&mut self, dibits: &[SoftDibit]) {
        for d in dibits {
            if let Some(last) = self.last_at {
                let step = d.at - last;
                match self.nominal {
                    None if step > 0.0 => {
                        self.first.push(step);
                        if self.first.len() == SPACING_COUNT {
                            self.first.sort_by(f64::total_cmp);
                            self.nominal = Some(self.first[SPACING_COUNT / 2]);
                        }
                    }
                    // Steps across a gap or a resliced stretch do not count.
                    Some(n) if (0.5 * n..1.5 * n).contains(&step) => {
                        self.sum += step;
                        self.count += 1;
                    }
                    _ => {}
                }
            }
            self.last_at = Some(d.at);
        }
    }

    /// Samples per symbol: the mean step once enough are in, else the first steps' median.
    fn symbol(&self) -> Option<f64> {
        if self.count >= SPACING_COUNT {
            Some(self.sum / self.count as f64)
        } else {
            self.nominal
        }
    }
}

/// Turns soft dibits into bursts. State carries across [`push`](Self::push) calls.
///
/// Polarity: once a data burst tells it, [`inverted`](Self::inverted) says whether the
/// discriminator is upside down. The caller passes a change of it to
/// [`crate::fsk4::Fsk4Demod::invert`] after a push, so the framer takes every later push's
/// dibits as already turned, and turns those it holds itself. Until then a voice sync is
/// read as it stands.
#[derive(Default)]
pub struct DmrFramer {
    /// Dibits not yet consumed, in the signal's polarity as far as known.
    buf: Vec<SoftDibit>,
    /// Where the sync search goes on from, in `buf`.
    searched: usize,
    inverted: Option<bool>,
    spacing: Spacing,
    tracks: Vec<Track>,
    /// The colour code learnt on each slot (no slot, 1, 2): from a slot type, or an EMB
    /// with no bit corrected.
    color_codes: [Option<u8>; 3],
    /// Repeater syncs running whose CACH named the other slot than the grid's.
    disagreements: usize,
}

impl DmrFramer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The bursts `dibits` complete, in the order sent.
    pub fn push(&mut self, dibits: &[SoftDibit]) -> Vec<DmrBurst> {
        // The caller turned the demodulator after the last push, so these arrive turned.
        let applied = self.inverted.unwrap_or(false);
        self.spacing.learn(dibits);
        self.buf.extend_from_slice(dibits);
        let mut out = Vec::new();
        let Some(sps) = self.spacing.symbol() else {
            return out;
        };
        loop {
            let sync = self.find_sync();
            let resume = match sync {
                Some((i, ..)) => i,
                None => self
                    .buf
                    .len()
                    .saturating_sub(SYNC_DIBITS - 1)
                    .max(self.searched),
            };
            let sync_at = sync.map(|(i, ..)| self.buf[i].at);
            let next = self
                .tracks
                .iter()
                .enumerate()
                .map(|(k, t)| (k, t.place(t.next, sps)))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((k, at)) = next
                && sync_at.is_none_or(|s| at < s - SAME_BURST_SYMBOLS * sps)
            {
                if !self.covers(at, sps) {
                    self.searched = resume;
                    break;
                }
                out.extend(self.flywheel_burst(k, at, sps));
                continue;
            }
            let Some((i, origin, kind)) = sync else {
                self.searched = resume;
                break;
            };
            if !self.covers(self.buf[i].at, sps) {
                self.searched = resume;
                break;
            }
            out.extend(self.sync_burst(i, origin, kind, applied, sps));
            self.searched = i + SYNC_DIBITS;
        }
        self.trim(sps);
        out
    }

    /// Whether the discriminator is inverted; known after the first data burst.
    pub fn inverted(&self) -> Option<bool> {
        self.inverted
    }

    /// The first sync from `searched` with at most [`SYNC_MAX_ERRORS`] dibits wrong: its
    /// index and what it says.
    fn find_sync(&self) -> Option<(usize, Origin, Kind)> {
        let syncs: Vec<(Vec<u8>, Origin, Kind)> = [
            (Origin::Repeater, Kind::Voice),
            (Origin::Repeater, Kind::Data),
            (Origin::Handheld, Kind::Voice),
            (Origin::Handheld, Kind::Data),
        ]
        .into_iter()
        .map(|(o, k)| (sync_dibits(sync_word(o, k), SYNC_BITS), o, k))
        .collect();
        let last = self.buf.len().checked_sub(SYNC_DIBITS)?;
        (self.searched..=last).find_map(|i| {
            let window = &self.buf[i..i + SYNC_DIBITS];
            syncs.iter().find_map(|(pattern, o, k)| {
                let errors = window
                    .iter()
                    .zip(pattern)
                    .filter(|(d, p)| d.dibit != **p)
                    .count();
                (errors <= SYNC_MAX_ERRORS).then_some((i, *o, *k))
            })
        })
    }

    /// Whether the dibits reach the end of a burst whose centre field starts at `centre`.
    fn covers(&self, centre: f64, sps: f64) -> bool {
        let end = centre + (BURST_DIBITS - LEFT_DIBITS - 1) as f64 * sps;
        self.buf.last().is_some_and(|d| d.at >= end - 0.5 * sps)
    }

    /// The first dibit at or after sample position `at`.
    fn index_at(&self, at: f64) -> usize {
        self.buf.partition_point(|d| d.at < at)
    }

    /// The soft bits of the dibit nearest `at`, or an erasure when none is within half a
    /// symbol.
    fn pick(&self, at: f64, sps: f64) -> [u8; 2] {
        let i = self.index_at(at);
        [i.checked_sub(1), Some(i)]
            .into_iter()
            .flatten()
            .filter_map(|j| self.buf.get(j))
            .min_by(|a, b| (a.at - at).abs().total_cmp(&(b.at - at).abs()))
            .filter(|d| (d.at - at).abs() <= 0.5 * sps)
            .map_or(ERASURE, |d| d.soft)
    }

    /// The burst whose centre field starts at `centre` and the CACH before it, as soft
    /// bits, each symbol taken by its position.
    fn read(&self, centre: f64, sps: f64) -> ([u8; BURST_BITS], [u8; CACH_BITS]) {
        let mut bits = [0u8; BURST_BITS];
        for b in 0..BURST_DIBITS {
            let soft = self.pick(centre + (b as f64 - LEFT_DIBITS as f64) * sps, sps);
            bits[2 * b..2 * b + 2].copy_from_slice(&soft);
        }
        let mut cach = [0u8; CACH_BITS];
        for b in 0..CACH_DIBITS {
            let back = (LEFT_DIBITS + CACH_DIBITS - b) as f64;
            let soft = self.pick(centre - back * sps, sps);
            cach[2 * b..2 * b + 2].copy_from_slice(&soft);
        }
        (bits, cach)
    }

    /// The track of `slot`, made when there is none, with its timing from a sync at
    /// `centre`; on a repeater the other slot's too, half a period off.
    fn track(&mut self, slot: Option<u8>, origin: Origin, centre: f64, sps: f64) -> usize {
        if origin == Origin::Repeater
            && let Some(other) = slot.map(|s| 3 - s)
            && !self.tracks.iter().any(|t| t.slot == Some(other))
        {
            let half = (SLOT_PERIOD_DIBITS / 2) as f64 * sps;
            self.tracks.push(Track::new(
                Some(other),
                origin,
                centre - half,
                TrackState::Unknown,
            ));
        }
        match self.tracks.iter().position(|t| t.slot == slot) {
            Some(k) => k,
            None => {
                let state = TrackState::Unknown;
                self.tracks.push(Track::new(slot, origin, centre, state));
                self.tracks.len() - 1
            }
        }
    }

    /// The slot of a repeater's sync at `centre` whose CACH names slot `tc`. Once both slots
    /// are timed the grid decides: the slot whose track has a burst within
    /// [`SAME_BURST_SYMBOLS`] of the sync, since a TACT with two bits wrong decodes, the
    /// Hamming (7,4) code being perfect, and can name the other slot. When the TC disagrees
    /// with the grid on [`GRID_OVERRIDE_SYNCS`] syncs running, the TC is trusted and the grid
    /// rebuilt from it: the repeater was re-timed.
    fn repeater_slot(&mut self, tc: u8, centre: f64, sps: f64) -> u8 {
        let period = SLOT_PERIOD_DIBITS as f64 * sps;
        let on_grid = |t: &&Track| {
            let periods = (centre - t.anchor) / period;
            (periods - periods.round()).abs() * period <= SAME_BURST_SYMBOLS * sps
        };
        let timed = |t: &&Track| t.origin == Origin::Repeater && t.slot.is_some();
        let tracks: Vec<&Track> = self.tracks.iter().filter(timed).collect();
        let grid = match tracks.as_slice() {
            [_, _] => tracks.iter().copied().find(on_grid).and_then(|t| t.slot),
            _ => None,
        };
        let Some(grid) = grid.filter(|&g| g != tc) else {
            self.disagreements = 0;
            return tc;
        };
        self.disagreements += 1;
        if self.disagreements < GRID_OVERRIDE_SYNCS {
            return grid;
        }
        self.disagreements = 0;
        self.tracks.retain(|t| t.origin != Origin::Repeater);
        tc
    }

    /// The burst at sync `i`.
    fn sync_burst(
        &mut self,
        i: usize,
        origin: Origin,
        kind: Kind,
        applied: bool,
        sps: f64,
    ) -> Option<DmrBurst> {
        let centre = self.buf[i].at;
        let (mut bits, mut cach) = self.read(centre, sps);
        let mut kind = kind;
        if self.inverted.is_none() {
            if kind == Kind::Data && data_checks(&bits) {
                self.inverted = Some(applied);
            } else {
                let (turned, turned_cach) = (flipped(&bits), flipped(&cach));
                if kind == Kind::Voice && data_checks(&turned) {
                    // A data burst upside down: its sync reads as the voice sync.
                    let back = (LEFT_DIBITS + CACH_DIBITS + 1) as f64 * sps;
                    let from = self.index_at(centre - back);
                    self.buf[from..].iter_mut().for_each(flip);
                    self.inverted = Some(!applied);
                    self.tracks.clear();
                    (bits, cach, kind) = (turned, turned_cach, Kind::Data);
                } else if kind == Kind::Data {
                    // Unknown polarity and codes that do not check: not read.
                    return None;
                }
            }
        }
        let slot = (origin == Origin::Repeater).then(|| {
            let tc = (cach_tact(&cach).0 >> 2 & 1) + 1;
            self.repeater_slot(tc, centre, sps)
        });
        let at = centre - LEFT_DIBITS as f64 * sps;
        let k = self.track(slot, origin, centre, sps);
        let track = &mut self.tracks[k];
        track.origin = origin;
        let period = track.heard(centre, sps);
        match kind {
            Kind::Voice => {
                let six = VOICE_BURSTS as i64;
                if track
                    .phase
                    .is_some_and(|p| (period - p).rem_euclid(six) != 0)
                {
                    // Another superframe grid: another call.
                    track.fragments.clear();
                    track.lc = None;
                }
                track.state = TrackState::Voice;
                track.phase = Some(period);
                track.heard_a = Some(period);
                let (voice, errors) = voice_frames(&bits);
                Some(DmrBurst {
                    at,
                    slot,
                    origin,
                    color_code: None,
                    content: DmrContent::Voice {
                        first: true,
                        letter: Some(0),
                        lc: None,
                        other: None,
                        pi: false,
                        voice,
                        errors,
                    },
                })
            }
            Kind::Data => {
                let (data, _) = GOLAY20.decode(slot_type_word(&bits))?;
                let (cc, dt) = ((data >> 4) as u8, (data & 0xF) as u8);
                let lc = |mask| {
                    bptc_decode(&info_bits(&bits))
                        .and_then(|(info, _)| lc_decode(&info, mask))
                        .map(|(lc, _)| lc)
                };
                let content = match dt {
                    DT_VOICE_LC_HEADER => {
                        let header = lc(MASK_VOICE_LC_HEADER);
                        track.data(TrackState::Voice, header);
                        DmrContent::VoiceHeader(header)
                    }
                    // A privacy header comes between a call's header and its voice.
                    DT_PI_HEADER => DmrContent::PrivacyHeader,
                    other => {
                        track.data(TrackState::Data, None);
                        match other {
                            DT_TERMINATOR_LC => DmrContent::Terminator(lc(MASK_TERMINATOR_LC)),
                            DT_CSBK => DmrContent::Control,
                            DT_IDLE => DmrContent::Idle,
                            d => DmrContent::Other(d),
                        }
                    }
                };
                self.color_codes[slot_key(slot)] = Some(cc);
                Some(DmrBurst {
                    at,
                    slot,
                    origin,
                    color_code: Some(cc),
                    content,
                })
            }
        }
    }

    /// Track `k`'s next burst, its centre field at `centre`, read by position: a voice burst
    /// when the flywheel takes it ([`Track::flywheel`]). A track that has had neither a sync
    /// nor a valid EMB for two superframes is dropped.
    fn flywheel_burst(&mut self, k: usize, centre: f64, sps: f64) -> Option<DmrBurst> {
        let (bits, _) = self.read(centre, sps);
        let key = slot_key(self.tracks[k].slot);
        // A repeater has one colour code: the other slot's stands in until this one's is heard.
        let known = match key {
            1 | 2 => self.color_codes[key].or(self.color_codes[3 - key]),
            _ => self.color_codes[key],
        };
        let track = &mut self.tracks[k];
        let period = track.next;
        track.next += 1;
        let (slot, origin) = (track.slot, track.origin);
        let read = match track.state {
            TrackState::Data => None,
            _ => track.flywheel(&bits, known, period),
        };
        if period - track.last_good > FLYWHEEL_PERIODS {
            self.tracks.remove(k);
        }
        let FlywheelRead {
            emb: data,
            emb_errors,
            letter,
            message,
        } = read?;
        let cc = (data >> 3) as u8;
        if emb_errors == 0 {
            self.color_codes[key] = Some(cc);
        }
        let (voice, ambe_errors) = voice_frames(&bits);
        let (lc, lc_errors, other) = match message {
            Some(Embedded::Lc(lc, e)) => (Some(lc), e, None),
            Some(Embedded::Other(flco, e)) => (None, e, Some(flco)),
            None => (None, 0, None),
        };
        Some(DmrBurst {
            at: centre - LEFT_DIBITS as f64 * sps,
            slot,
            origin,
            color_code: Some(cc),
            content: DmrContent::Voice {
                first: false,
                letter: letter.map(|l| l as u8),
                lc,
                other,
                pi: data >> 2 & 1 == 1,
                voice,
                errors: emb_errors + lc_errors + ambe_errors,
            },
        })
    }

    /// Drops the dibits nothing can reach any more: those before the CACH of a burst whose
    /// sync starts at `searched`, or of a track's next burst.
    fn trim(&mut self, sps: f64) {
        let back = (LEFT_DIBITS + CACH_DIBITS + 2) as f64 * sps;
        let search_at = self
            .buf
            .get(self.searched)
            .or(self.buf.last())
            .map_or(f64::INFINITY, |d| d.at);
        let needed = self
            .tracks
            .iter()
            .map(|t| t.place(t.next, sps))
            .fold(search_at, f64::min);
        let n = self.index_at(needed - back).min(self.searched);
        self.buf.drain(..n);
        self.searched -= n;
    }
}

fn slot_key(slot: Option<u8>) -> usize {
    match slot {
        Some(s @ 1..=2) => usize::from(s),
        _ => 0,
    }
}

/// `d` in the other polarity: each level negated, so the first bit flips.
fn flip(d: &mut SoftDibit) {
    d.dibit ^= 0b10;
    d.soft[0] = 255 - d.soft[0];
}

/// Soft bits in the other polarity: every dibit's first bit flipped.
fn flipped<const N: usize>(bits: &[u8; N]) -> [u8; N] {
    std::array::from_fn(|i| if i % 2 == 0 { 255 - bits[i] } else { bits[i] })
}

/// Soft bits gathered into a word, the first the top bit.
fn hard_word<'a>(bits: impl Iterator<Item = &'a u8>) -> u32 {
    bits.fold(0, |w, &b| w << 1 | u32::from(b >= 128))
}

fn slot_type_word(bits: &[u8; BURST_BITS]) -> u32 {
    let at = HALF_BITS - SLOT_TYPE_HALF_BITS;
    hard_word(
        bits[at..HALF_BITS]
            .iter()
            .chain(&bits[HALF_BITS + CENTRE_BITS..][..SLOT_TYPE_HALF_BITS]),
    )
}

fn emb_word_of(bits: &[u8; BURST_BITS]) -> u32 {
    let low = HALF_BITS + CENTRE_BITS - EMB_HALF_BITS;
    hard_word(
        bits[HALF_BITS..][..EMB_HALF_BITS]
            .iter()
            .chain(&bits[low..][..EMB_HALF_BITS]),
    )
}

/// A data burst's 196 BPTC bits in the order sent.
fn info_bits(bits: &[u8; BURST_BITS]) -> Vec<u8> {
    bits[..DATA_HALF_BITS]
        .iter()
        .chain(&bits[BURST_BITS - DATA_HALF_BITS..])
        .copied()
        .collect()
}

/// Whether a data burst's slot type and BPTC both check.
fn data_checks(bits: &[u8; BURST_BITS]) -> bool {
    GOLAY20.decode(slot_type_word(bits)).is_some() && bptc_decode(&info_bits(bits)).is_some()
}

/// A voice burst's three frames and the bits their Golay words corrected.
fn voice_frames(bits: &[u8; BURST_BITS]) -> (Box<[AmbeSoft; 3]>, u32) {
    let vs: Vec<u8> = bits[..HALF_BITS]
        .iter()
        .chain(&bits[HALF_BITS + CENTRE_BITS..])
        .copied()
        .collect();
    let frames: [AmbeSoft; 3] = std::array::from_fn(|f| {
        ambe_from_bits(&vs[f * AMBE_FRAME_BITS..(f + 1) * AMBE_FRAME_BITS])
    });
    let errors = frames.iter().map(|f| ambe_decode(f).1).sum();
    (Box::new(frames), errors)
}

// ---- encoding ----

/// One slot of [`encode_repeater`]: a call of `superframes` with link control `lc`; with
/// `control`, Preamble CSBKs (-2 7.1.2.5) from `lc.src` to `lc.dst` instead, for as long
/// as the call would last; with `lc: None`, Idle bursts for as long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotPlan {
    pub lc: Option<FullLc>,
    pub superframes: u32,
    pub control: bool,
}

/// A repeater's continuous downlink with CACH: both slots, each a call, control blocks or
/// idle. A call is a voice LC header, a privacy header when `lc.encrypted`, `superframes`
/// voice superframes of the voice payload (`voice_payload`, seeded with the call's
/// source, so two calls never carry the same frames), a terminator; a slot that
/// finishes first idles until the other does. The CACHs carry the Null short LC. Dibits.
pub fn encode_repeater(color_code: u8, slots: [SlotPlan; 2]) -> Vec<u8> {
    encode_repeater_aliased(color_code, slots, [false; 2])
}

/// [`encode_repeater`], each slot's call carrying a talker alias when `talker_alias` says
/// so: its second superframe, and every second one after it, sends a talker alias header
/// (-2 5.4.3, 7.1.1.4) in bursts B to E in place of the link control, as radios interleave
/// them.
pub fn encode_repeater_aliased(
    color_code: u8,
    slots: [SlotPlan; 2],
    talker_alias: [bool; 2],
) -> Vec<u8> {
    let bursts: [Vec<[u8; BURST_BITS]>; 2] = std::array::from_fn(|s| {
        let (plan, alias) = (slots[s], talker_alias[s]);
        match plan.lc {
            Some(lc) if plan.control => {
                let n = call_bursts_len(plan.superframes, false);
                (0..n)
                    .map(|i| csbk_burst(color_code, &preamble_csbk(&lc, n - 1 - i)))
                    .collect()
            }
            Some(lc) => call_bursts(
                color_code,
                &lc,
                plan.superframes,
                Origin::Repeater,
                alias.then(talker_alias_bytes),
            ),
            None => vec![idle_burst(color_code); call_bursts_len(plan.superframes, false)],
        }
    });
    let busy = slots.map(|plan| plan.lc.is_some());
    downlink(color_code, &bursts, busy)
}

/// A handheld's bursts: one slot in two, silence (no symbols) between. The call is as
/// [`encode_repeater`]'s, with the handheld's syncs and no CACH; each burst is followed by
/// the rest of its 60 ms as `None`.
pub fn encode_handheld(color_code: u8, lc: FullLc, superframes: u32) -> Vec<Option<u8>> {
    encode_handheld_aliased(color_code, lc, superframes, false)
}

/// [`encode_handheld`], the call carrying a talker alias as [`encode_repeater_aliased`]'s
/// do when `talker_alias`.
pub fn encode_handheld_aliased(
    color_code: u8,
    lc: FullLc,
    superframes: u32,
    talker_alias: bool,
) -> Vec<Option<u8>> {
    handheld_air(&call_bursts(
        color_code,
        &lc,
        superframes,
        Origin::Handheld,
        talker_alias.then(talker_alias_bytes),
    ))
}

/// A handheld's bursts on the air: each followed by the rest of its 60 ms as `None`.
fn handheld_air(bursts: &[[u8; BURST_BITS]]) -> Vec<Option<u8>> {
    let mut out = Vec::new();
    for burst in bursts {
        out.extend(dibits_of(burst).into_iter().map(Some));
        out.extend(std::iter::repeat_n(None, SLOT_PERIOD_DIBITS - BURST_DIBITS));
    }
    out
}

/// The downlink of two slots' bursts, a CACH before each (TC naming the slot, AT busy
/// while `busy` and the slot's bursts last); a slot that runs out idles.
fn downlink(cc: u8, bursts: &[Vec<[u8; BURST_BITS]>; 2], busy: [bool; 2]) -> Vec<u8> {
    let idle = idle_burst(cc);
    let frames = bursts.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * SLOT_PERIOD_DIBITS);
    for f in 0..frames {
        for (s, list) in bursts.iter().enumerate() {
            let at = busy[s] && f < list.len();
            let lcss = SHORT_LC_LCSS[(2 * f + s) % SHORT_LC_LCSS.len()];
            out.extend(dibits_of(&cach_bits(at, s as u8, lcss)));
            out.extend(dibits_of(list.get(f).unwrap_or(&idle)));
        }
    }
    out
}

fn call_bursts_len(superframes: u32, encrypted: bool) -> usize {
    2 + usize::from(encrypted) + VOICE_BURSTS * superframes as usize
}

/// A call's bursts on one slot: the voice LC header, a privacy header when encrypted,
/// `superframes` × bursts A to F, the terminator with link control. With `interleave`
/// (another embedded message's 9 octets: a talker alias, GPS) the second superframe and
/// every second one after it carry it in B to E instead of the link control. The EMB's PI
/// bit is 0 throughout: it is not an encryption mark (see the module doc). The voice
/// frames are `voice_payload(lc.src, 0, 1, 2 …)`.
fn call_bursts(
    cc: u8,
    lc: &FullLc,
    superframes: u32,
    origin: Origin,
    interleave: Option<[u8; LC_BYTES]>,
) -> Vec<[u8; BURST_BITS]> {
    let data_sync = sync_word(origin, Kind::Data);
    let data = |dt, mask| {
        data_burst(
            data_sync,
            slot_type(cc, dt),
            &bptc_encode(&lc_info(lc, mask)),
        )
    };
    let fragments = embedded_encode(&lc_bytes(lc));
    let other = interleave.map(|b| embedded_encode(&b));
    let emb = |lcss| emb_word(cc, false, lcss);
    let mut frame = 0;
    let mut socket = || {
        let frames = std::array::from_fn(|i| ambe_encode(&voice_payload(lc.src, frame + i as u32)));
        frame += 3;
        voice_socket(&frames)
    };
    let mut out = Vec::with_capacity(call_bursts_len(superframes, lc.encrypted));
    out.push(data(DT_VOICE_LC_HEADER, MASK_VOICE_LC_HEADER));
    if lc.encrypted {
        out.push(data_burst(
            data_sync,
            slot_type(cc, DT_PI_HEADER),
            &bptc_encode(&pi_header_info()),
        ));
    }
    for sf in 0..superframes {
        out.push(voice_burst(
            &socket(),
            &sync_centre(sync_word(origin, Kind::Voice)),
        ));
        let embedded = match &other {
            Some(other) if sf % 2 == 1 => other,
            _ => &fragments,
        };
        for (fragment, lcss) in embedded.iter().zip(EMBEDDED_LCSS) {
            out.push(voice_burst(&socket(), &emb_centre(emb(lcss), fragment)));
        }
        // Burst F: the Null embedded message.
        let null = emb_centre(emb(LCSS_SINGLE), &[0; EMBEDDED_BITS]);
        out.push(voice_burst(&socket(), &null));
    }
    out.push(data(DT_TERMINATOR_LC, MASK_TERMINATOR_LC));
    out
}

/// The 49 parameter bits of the `n`-th voice frame of a call seeded `seed` (its calling
/// radio's ID): one SplitMix64 step ([`splitmix64`]) of `seed` above `n`, its top 49 bits
/// split 12, 12, 11 and 14. Varied from frame to frame so that no burst sits on one level,
/// as speech's frames do not, and from call to call so that two calls at once never carry
/// the same frames, as no two talkers do; it is not speech, and no codec made it. The
/// receiver's coast is sized over many seeds ([`DMR_COAST_SEARCHES`]), as speech varies.
pub(crate) fn voice_payload(seed: u32, n: u32) -> [u16; 4] {
    let z = splitmix64(u64::from(seed) << 32 | u64::from(n));
    [
        (z >> 52) as u16 & 0xFFF,
        (z >> 40) as u16 & 0xFFF,
        (z >> 29) as u16 & 0x7FF,
        (z >> 15) as u16 & 0x3FFF,
    ]
}

/// A repeater's Idle burst: table D.2's fill bits.
fn idle_burst(cc: u8) -> [u8; BURST_BITS] {
    let info: [u8; BPTC_INFO_BITS] = std::array::from_fn(|i| IDLE_FILL[i / 8] >> (7 - i % 8) & 1);
    data_burst(
        DMR_SYNC_BS_DATA,
        slot_type(cc, DT_IDLE),
        &bptc_encode(&info),
    )
}

/// A privacy header's information bits: -1 does not define its 80 bits of contents
/// (table 6.1's note), filled here from SplitMix64 so the burst is not one level, then
/// their CRC-CCITT under the PI header's mask.
fn pi_header_info() -> [u8; BPTC_INFO_BITS] {
    let fill = splitmix64(PI_HEADER_SEED).to_be_bytes();
    let block: [u8; BLOCK_DATA_BYTES] = std::array::from_fn(|i| fill[i % fill.len()] ^ i as u8);
    crc_block(&block, MASK_PI_HEADER)
}

/// A Preamble CSBK (-2 table 7.7): LB, PF 0, CSBKO `111101`, FID 0, CSBK content to
/// follow, a group target, `follow` blocks to follow, the target and source addresses.
fn preamble_csbk(lc: &FullLc, follow: usize) -> [u8; BLOCK_DATA_BYTES] {
    let [_, d2, d1, d0] = (lc.dst & ID_MASK).to_be_bytes();
    let [_, s2, s1, s0] = (lc.src & ID_MASK).to_be_bytes();
    let gi = if lc.call == Call::Group {
        CSBK_GROUP
    } else {
        0
    };
    [
        CSBK_LAST_BLOCK | CSBKO_PREAMBLE,
        0,
        gi,
        follow.min(255) as u8,
        d2,
        d1,
        d0,
        s2,
        s1,
        s0,
    ]
}

/// A repeater's CSBK burst (-1 7.2, table 9.9): the block, its CRC-CCITT under the CSBK
/// mask, BPTC (196,96).
fn csbk_burst(cc: u8, block: &[u8; BLOCK_DATA_BYTES]) -> [u8; BURST_BITS] {
    let info = crc_block(block, MASK_CSBK);
    data_burst(
        DMR_SYNC_BS_DATA,
        slot_type(cc, DT_CSBK),
        &bptc_encode(&info),
    )
}

/// 80 bits of a block and their CRC-CCITT under `mask`, as 96 information bits.
fn crc_block(block: &[u8; BLOCK_DATA_BYTES], mask: u16) -> [u8; BPTC_INFO_BITS] {
    let crc = crc_ccitt(block) ^ mask;
    let bytes: Vec<u8> = block.iter().copied().chain(crc.to_be_bytes()).collect();
    std::array::from_fn(|i| bytes[i / 8] >> (7 - i % 8) & 1)
}

/// The CRC-CCITT of B.3.8: the remainder of `x^16 M(x)` by `x^16 + x^12 + x^5 + 1`
/// (initial remainder 0), inverted.
fn crc_ccitt(data: &[u8]) -> u16 {
    let rem = data.iter().fold(0u16, |crc, &byte| {
        (0..8).fold(crc ^ u16::from(byte) << 8, |c, _| {
            if c & 0x8000 != 0 {
                c << 1 ^ CRC_CCITT_POLY
            } else {
                c << 1
            }
        })
    });
    rem ^ 0xFFFF
}

fn slot_type(cc: u8, dt: u8) -> u32 {
    GOLAY20.encode(u32::from(cc & 0xF) << 4 | u32::from(dt & 0xF))
}

fn emb_word(cc: u8, pi: bool, lcss: u8) -> u32 {
    QR16.encode(u32::from(cc & 0xF) << 3 | u32::from(pi) << 2 | u32::from(lcss & 3))
}

/// Bits (0 or 1), two to a dibit, the first the dibit's top bit.
fn dibits_of(bits: &[u8]) -> Vec<u8> {
    bits.chunks(2).map(|p| p[0] << 1 | p[1]).collect()
}

/// A data burst: the BPTC bits `info` in the order sent, the 20-bit slot type, the sync.
fn data_burst(sync: u64, slot_type: u32, info: &[u8; BPTC_BITS]) -> [u8; BURST_BITS] {
    let mut b = [0u8; BURST_BITS];
    b[..DATA_HALF_BITS].copy_from_slice(&info[..DATA_HALF_BITS]);
    b[BURST_BITS - DATA_HALF_BITS..].copy_from_slice(&info[DATA_HALF_BITS..]);
    for i in 0..SLOT_TYPE_HALF_BITS {
        let high = SLOT_TYPE_BITS as usize - 1 - i;
        b[HALF_BITS - SLOT_TYPE_HALF_BITS + i] = (slot_type >> high & 1) as u8;
        b[HALF_BITS + CENTRE_BITS + i] = (slot_type >> (high - SLOT_TYPE_HALF_BITS) & 1) as u8;
    }
    b[HALF_BITS..HALF_BITS + CENTRE_BITS].copy_from_slice(&sync_centre(sync));
    b
}

/// A voice burst: the vocoder socket `vs` (VS(215) first) around the centre field.
fn voice_burst(vs: &[u8; 2 * HALF_BITS], centre: &[u8; CENTRE_BITS]) -> [u8; BURST_BITS] {
    let mut b = [0u8; BURST_BITS];
    b[..HALF_BITS].copy_from_slice(&vs[..HALF_BITS]);
    b[HALF_BITS..HALF_BITS + CENTRE_BITS].copy_from_slice(centre);
    b[HALF_BITS + CENTRE_BITS..].copy_from_slice(&vs[HALF_BITS..]);
    b
}

fn sync_centre(sync: u64) -> [u8; CENTRE_BITS] {
    std::array::from_fn(|i| (sync >> (CENTRE_BITS - 1 - i) & 1) as u8)
}

/// The centre of a voice burst with embedded signalling: the 16-bit EMB word's top half,
/// the 32 embedded bits, its low half.
fn emb_centre(emb: u32, embedded: &[u8; EMBEDDED_BITS]) -> [u8; CENTRE_BITS] {
    let mut c = [0u8; CENTRE_BITS];
    for i in 0..EMB_HALF_BITS {
        c[i] = (emb >> (EMB_BITS as usize - 1 - i) & 1) as u8;
        c[CENTRE_BITS - EMB_HALF_BITS + i] = (emb >> (EMB_HALF_BITS - 1 - i) & 1) as u8;
    }
    c[EMB_HALF_BITS..EMB_HALF_BITS + EMBEDDED_BITS].copy_from_slice(embedded);
    c
}

/// The vocoder socket of three frames: VF(1) first, each in the order sent.
fn voice_socket(frames: &[AmbeSoft; 3]) -> [u8; 2 * HALF_BITS] {
    let mut vs = [0u8; 2 * HALF_BITS];
    for (f, frame) in frames.iter().enumerate() {
        vs[f * AMBE_FRAME_BITS..][..AMBE_FRAME_BITS].copy_from_slice(&ambe_tx_bits(frame));
    }
    vs
}

// ---- the CACH ----

/// The CACH of a 7-bit TACT word (AT first) and 17 payload bits (P(16) first).
fn cach_place(tact: u32, payload: u32) -> [u8; CACH_BITS] {
    let mut bits = [0u8; CACH_BITS];
    for (i, &p) in TACT_PLACES.iter().enumerate() {
        bits[p] = (tact >> (TACT_PLACES.len() - 1 - i) & 1) as u8;
    }
    let gaps = (0..CACH_BITS).filter(|p| !TACT_PLACES.contains(p));
    let payload_bits = CACH_BITS - TACT_PLACES.len();
    for (j, p) in gaps.enumerate() {
        bits[p] = (payload >> (payload_bits - 1 - j) & 1) as u8;
    }
    bits
}

/// A CACH with the TACT of `at`, `tc` and `lcss` and an empty payload (the Null short LC's
/// bits are all zero).
fn cach_bits(at: bool, tc: u8, lcss: u8) -> [u8; CACH_BITS] {
    let data = u32::from(at) << 3 | u32::from(tc & 1) << 2 | u32::from(lcss & 3);
    cach_place(HAMMING7.encode(data), 0)
}

/// A CACH's TACT (AT, TC, LCSS as 4 bits) from its soft bits, and the bits corrected.
fn cach_tact(bits: &[u8]) -> (u8, u32) {
    let word = hard_word(TACT_PLACES.iter().map(|&p| &bits[p]));
    // The code is perfect: every word decodes.
    let (data, errors) = HAMMING7.decode(word).unwrap_or((word >> 3, 0));
    (data as u8, errors)
}

// ---- link control ----

/// The 72-bit full link control as 9 octets.
fn lc_bytes(lc: &FullLc) -> [u8; LC_BYTES] {
    let flco = match lc.call {
        Call::Group => FLCO_GROUP,
        Call::Private => FLCO_PRIVATE,
    };
    let mut svc = 0;
    if lc.emergency {
        svc |= SVC_EMERGENCY;
    }
    if lc.encrypted {
        svc |= SVC_PRIVACY;
    }
    let [_, d2, d1, d0] = (lc.dst & ID_MASK).to_be_bytes();
    let [_, s2, s1, s0] = (lc.src & ID_MASK).to_be_bytes();
    [flco, lc.fid, svc, d2, d1, d0, s2, s1, s0]
}

/// The link control in 9 octets, when it is a voice channel user's (either opcode, any
/// FID) and PF is clear.
fn parse_lc(b: &[u8; LC_BYTES]) -> Option<FullLc> {
    if b[0] & LC_PF != 0 {
        return None;
    }
    let call = match b[0] & FLCO_MASK {
        FLCO_GROUP => Call::Group,
        FLCO_PRIVATE => Call::Private,
        _ => return None,
    };
    let id = |b: &[u8]| b.iter().fold(0u32, |w, &x| w << 8 | u32::from(x));
    Some(FullLc {
        call,
        dst: id(&b[3..6]),
        src: id(&b[6..9]),
        fid: b[1],
        encrypted: b[2] & SVC_PRIVACY != 0,
        emergency: b[2] & SVC_EMERGENCY != 0,
    })
}

/// The FLCO of an embedded message in 9 octets that is not a voice channel user's link
/// control (a talker alias or GPS, -2 table 5.4), when PF is clear.
fn other_flco(b: &[u8; LC_BYTES]) -> Option<u8> {
    let flco = b[0] & FLCO_MASK;
    (b[0] & LC_PF == 0 && flco != FLCO_GROUP && flco != FLCO_PRIVATE).then_some(flco)
}

/// The encoder's talker alias header (-2 7.1.1.4, table 7.4): FLCO `000100`, FID 0, the
/// 7-bit format (7.2.18), the length in characters (7.2.19) and [`TALKER_ALIAS`] in 7 bits
/// a character, the rest of the 49 data bits zero.
fn talker_alias_bytes() -> [u8; LC_BYTES] {
    let chars = TALKER_ALIAS.as_bytes();
    // Format (2), length (5), then the 49 data bits: 56 bits from octet 2.
    let mut bits = u64::from(TA_FORMAT_7BIT) << 54 | (chars.len() as u64) << 49;
    for (i, &c) in chars.iter().enumerate() {
        bits |= u64::from(c & 0x7F) << (42 - 7 * i);
    }
    let mut b = [0u8; LC_BYTES];
    b[0] = FLCO_TALKER_ALIAS_HEADER;
    b[2..].copy_from_slice(&bits.to_be_bytes()[1..]);
    b
}

/// The 96 information bits of a header or terminator: the link control, then its
/// Reed-Solomon parity under `mask`, I(95) first.
fn lc_info(lc: &FullLc, mask: u32) -> [u8; BPTC_INFO_BITS] {
    let b = lc_bytes(lc);
    let parity = rs_parity(&b);
    let [_, m2, m1, m0] = mask.to_be_bytes();
    let word: Vec<u8> = b
        .iter()
        .copied()
        .chain([parity[0] ^ m2, parity[1] ^ m1, parity[2] ^ m0])
        .collect();
    std::array::from_fn(|i| word[i / 8] >> (7 - i % 8) & 1)
}

/// The link control from a header's or terminator's 96 bits (0 or 1) under `mask`, one
/// octet corrected, and the bits corrected.
fn lc_decode(info: &[u8; BPTC_INFO_BITS], mask: u32) -> Option<(FullLc, u32)> {
    let mut word = [0u8; LC_BYTES + 3];
    for (i, &b) in info.iter().enumerate() {
        word[i / 8] |= (b & 1) << (7 - i % 8);
    }
    for (w, m) in word[LC_BYTES..].iter_mut().zip(&mask.to_be_bytes()[1..]) {
        *w ^= m;
    }
    let errors = rs_decode(&mut word)?;
    let lc: [u8; LC_BYTES] = word[..LC_BYTES].try_into().ok()?;
    parse_lc(&lc).map(|lc| (lc, errors))
}

// ---- embedded link control ----

/// The 5-bit checksum (B.26): the octets' sum modulo 31.
fn checksum(lc: &[u8; LC_BYTES]) -> u8 {
    (lc.iter().map(|&b| u32::from(b)).sum::<u32>() % CS_MODULUS) as u8
}

/// The encode matrix of figure B.3: LC(71) to LC(61) and LC(60) to LC(50) in rows 0 and
/// 1, ten bits and one checksum bit in each of rows 2 to 6, each row's Hamming (16,11,4)
/// parity, then a row of even column parity.
fn embedded_matrix(lc: &[u8; LC_BYTES], cs: u8) -> [[u8; EMB_LC_COLS]; EMB_LC_ROWS] {
    let bit = |i: usize| u32::from(lc[i / 8] >> (7 - i % 8) & 1);
    let mut m = [[0u8; EMB_LC_COLS]; EMB_LC_ROWS];
    let mut i = 0;
    for (row, cells) in m.iter_mut().enumerate().take(EMB_LC_ROWS - 1) {
        let lc_bits = if row < 2 { 11 } else { 10 };
        let mut data = 0;
        for _ in 0..lc_bits {
            data = data << 1 | bit(i);
            i += 1;
        }
        if row >= 2 {
            data = data << 1 | u32::from(cs >> (6 - row) & 1);
        }
        let word = HAMMING16.encode(data);
        for (col, c) in cells.iter_mut().enumerate() {
            *c = (word >> (EMB_LC_COLS - 1 - col) & 1) as u8;
        }
    }
    let parity: [u8; EMB_LC_COLS] =
        std::array::from_fn(|col| m[..EMB_LC_ROWS - 1].iter().fold(0, |p, r| p ^ r[col]));
    m[EMB_LC_ROWS - 1] = parity;
    m
}

/// The matrix read out column by column, top to bottom, as four 32-bit fragments.
fn embedded_place(m: &[[u8; EMB_LC_COLS]; EMB_LC_ROWS]) -> [[u8; EMBEDDED_BITS]; 4] {
    let mut f = [[0u8; EMBEDDED_BITS]; EMB_LC_FRAGMENTS];
    for col in 0..EMB_LC_COLS {
        for (row, cells) in m.iter().enumerate() {
            let t = EMB_LC_ROWS * col + row;
            f[t / EMBEDDED_BITS][t % EMBEDDED_BITS] = cells[col];
        }
    }
    f
}

fn embedded_encode(lc: &[u8; LC_BYTES]) -> [[u8; EMBEDDED_BITS]; 4] {
    embedded_place(&embedded_matrix(lc, checksum(lc)))
}

/// The link control from four fragments' soft bits (bursts B to E), each row corrected
/// (one error), the column parity and the checksum checked; and the bits corrected.
fn embedded_decode(fragments: &[Vec<u8>]) -> Option<([u8; LC_BYTES], u32)> {
    if fragments.len() != EMB_LC_FRAGMENTS || fragments.iter().any(|f| f.len() != EMBEDDED_BITS) {
        return None;
    }
    let mut m = [[0u8; EMB_LC_COLS]; EMB_LC_ROWS];
    for col in 0..EMB_LC_COLS {
        for (row, cells) in m.iter_mut().enumerate() {
            let t = EMB_LC_ROWS * col + row;
            cells[col] = u8::from(fragments[t / EMBEDDED_BITS][t % EMBEDDED_BITS] >= 128);
        }
    }
    let mut errors = 0;
    let mut rows = [0u32; EMB_LC_ROWS];
    for (row, cells) in m.iter().enumerate() {
        let word = cells.iter().fold(0u32, |w, &b| w << 1 | u32::from(b));
        let (data, e) = HAMMING16.decode(word)?;
        errors += e;
        rows[row] = data;
    }
    // Row 7, the column parity, is a code word too; the rest must sum to it.
    let parity = rows[..EMB_LC_ROWS - 1]
        .iter()
        .fold(0, |p, &d| p ^ HAMMING16.encode(d));
    if parity != HAMMING16.encode(rows[EMB_LC_ROWS - 1]) {
        return None;
    }
    let mut lc = [0u8; LC_BYTES];
    let mut cs = 0u8;
    let mut i = 0;
    for (row, &data) in rows.iter().enumerate().take(EMB_LC_ROWS - 1) {
        let lc_bits = if row < 2 { 11 } else { 10 };
        let data_bits = 11;
        for j in 0..lc_bits {
            lc[i / 8] |= ((data >> (data_bits - 1 - j) & 1) as u8) << (7 - i % 8);
            i += 1;
        }
        if row >= 2 {
            cs = cs << 1 | (data & 1) as u8;
        }
    }
    (checksum(&lc) == cs).then_some((lc, errors))
}

// ---- BPTC (196,96) ----

/// Where bit `index` of the encoded matrix (table B.2's numbering) is sent: (B.1).
fn bptc_position(index: usize) -> usize {
    index * BPTC_INTERLEAVE % BPTC_BITS
}

/// The 13 × 15 encode matrix of figure B.1 for 96 information bits (I(95) first).
fn bptc_matrix(info: &[u8; BPTC_INFO_BITS]) -> [[u8; BPTC_COLS]; BPTC_ROWS] {
    let mut m = [[0u8; BPTC_COLS]; BPTC_ROWS];
    for (q, &b) in info.iter().enumerate() {
        let q = q + BPTC_RESERVED;
        m[q / BPTC_DATA_COLS][q % BPTC_DATA_COLS] = b & 1;
    }
    for row in m.iter_mut().take(BPTC_DATA_ROWS) {
        let word = HAMMING15.encode(bits_word(&row[..BPTC_DATA_COLS]));
        for (col, c) in row.iter_mut().enumerate() {
            *c = (word >> (BPTC_COLS - 1 - col) & 1) as u8;
        }
    }
    for col in 0..BPTC_COLS {
        let word = HAMMING13.encode(column_word(&m, col, BPTC_DATA_ROWS));
        for (row, cells) in m.iter_mut().enumerate() {
            cells[col] = (word >> (BPTC_ROWS - 1 - row) & 1) as u8;
        }
    }
    m
}

/// Bits (0 or 1) as a word, the first the top bit.
fn bits_word(bits: &[u8]) -> u32 {
    bits.iter().fold(0, |w, &b| w << 1 | u32::from(b))
}

/// The top `rows` cells of column `col` as a word.
fn column_word(m: &[[u8; BPTC_COLS]; BPTC_ROWS], col: usize, rows: usize) -> u32 {
    m[..rows].iter().fold(0, |w, r| w << 1 | u32::from(r[col]))
}

/// The encoded matrix in the order sent, R(3) first.
fn bptc_encode(info: &[u8; BPTC_INFO_BITS]) -> [u8; BPTC_BITS] {
    let m = bptc_matrix(info);
    let mut tx = [0u8; BPTC_BITS];
    for (row, cells) in m.iter().enumerate() {
        for (col, &c) in cells.iter().enumerate() {
            tx[bptc_position(1 + BPTC_COLS * row + col)] = c;
        }
    }
    tx
}

/// The 96 information bits from 196 soft bits in the order sent, and the bits corrected;
/// `None` unless every row and column ends a code word.
fn bptc_decode(tx: &[u8]) -> Option<([u8; BPTC_INFO_BITS], u32)> {
    if tx.len() != BPTC_BITS {
        return None;
    }
    let mut m = [[0u8; BPTC_COLS]; BPTC_ROWS];
    for (row, cells) in m.iter_mut().enumerate() {
        for (col, c) in cells.iter_mut().enumerate() {
            *c = u8::from(tx[bptc_position(1 + BPTC_COLS * row + col)] >= 128);
        }
    }
    let received = m;
    for _ in 0..BPTC_PASSES {
        let mut fixed = false;
        for col in 0..BPTC_COLS {
            if let Some((data, e @ 1..)) = HAMMING13.decode(column_word(&m, col, BPTC_ROWS)) {
                let word = HAMMING13.encode(data);
                for (row, cells) in m.iter_mut().enumerate() {
                    cells[col] = (word >> (BPTC_ROWS - 1 - row) & 1) as u8;
                }
                fixed |= e > 0;
            }
        }
        for cells in m.iter_mut().take(BPTC_DATA_ROWS) {
            if let Some((data, 1..)) = HAMMING15.decode(bits_word(cells)) {
                let word = HAMMING15.encode(data);
                for (col, c) in cells.iter_mut().enumerate() {
                    *c = (word >> (BPTC_COLS - 1 - col) & 1) as u8;
                }
                fixed = true;
            }
        }
        if !fixed {
            break;
        }
    }
    let rows_ok = m[..BPTC_DATA_ROWS]
        .iter()
        .all(|r| HAMMING15.decode(bits_word(r)).is_some_and(|(_, e)| e == 0));
    let cols_ok = (0..BPTC_COLS).all(|col| {
        HAMMING13
            .decode(column_word(&m, col, BPTC_ROWS))
            .is_some_and(|(_, e)| e == 0)
    });
    if !(rows_ok && cols_ok) {
        return None;
    }
    let errors = m
        .iter()
        .flatten()
        .zip(received.iter().flatten())
        .filter(|(a, b)| a != b)
        .count() as u32;
    let info = std::array::from_fn(|q| {
        let q = q + BPTC_RESERVED;
        m[q / BPTC_DATA_COLS][q % BPTC_DATA_COLS]
    });
    Some((info, errors))
}

// ---- cyclic block codes ----

/// No error pattern of the code's reach has this syndrome.
const NO_PATTERN: u32 = u32::MAX;
/// `x^11 + x^10 + x^6 + x^5 + x^4 + x^2 + 1`: the Golay codes' generator (B.4).
const GOLAY_POLY: u32 = 0xC75;
/// `x^8 + x^5 + x^4 + x^3 + 1`: the quadratic residue code's (B.5).
const QR_POLY: u32 = 0x139;
/// `x^4 + x + 1`: the Hamming (15,11) code's, shortened and extended (B.7).
const HAMMING4_POLY: u32 = 0x13;
/// `x^3 + x + 1`: the Hamming (7,4) code's (B.8).
const HAMMING3_POLY: u32 = 0xB;

/// A systematic code from a generator polynomial, shortened to `k` data bits and, when
/// `extended`, given an even-parity bit last: the data in the top bits, then the
/// remainder of the data times `x^deg` by the generator. Decoded by syndrome: every
/// pattern of up to `t` errors in a table.
struct Cyclic {
    k: u32,
    poly: u32,
    extended: bool,
    t: u32,
    table: &'static OnceLock<Vec<u32>>,
}

impl Cyclic {
    fn degree(&self) -> u32 {
        31 - self.poly.leading_zeros()
    }

    fn check_bits(&self) -> u32 {
        self.degree() + u32::from(self.extended)
    }

    fn n(&self) -> u32 {
        self.k + self.check_bits()
    }

    fn encode(&self, data: u32) -> u32 {
        let r = self.degree();
        let data = data & ((1 << self.k) - 1);
        let mut rem = data << r;
        for i in (r..r + self.k).rev() {
            if rem >> i & 1 == 1 {
                rem ^= self.poly << (i - r);
            }
        }
        let word = data << r | rem;
        if self.extended {
            word << 1 | (word.count_ones() & 1)
        } else {
            word
        }
    }

    fn syndrome(&self, word: u32) -> u32 {
        let c = self.check_bits();
        (self.encode(word >> c) ^ word) & ((1 << c) - 1)
    }

    /// The lightest error pattern of each syndrome, up to weight `t`.
    fn table(&self) -> &[u32] {
        self.table.get_or_init(|| {
            let mut table = vec![NO_PATTERN; 1 << self.check_bits()];
            let mut level = vec![0u32];
            for weight in 0..=self.t {
                for &e in &level {
                    let s = self.syndrome(e) as usize;
                    if table[s] == NO_PATTERN {
                        table[s] = e;
                    }
                }
                if weight < self.t {
                    // Every pattern one bit heavier, each once: a bit above the top one.
                    level = level
                        .iter()
                        .flat_map(|&e| (32 - e.leading_zeros()..self.n()).map(move |i| e | 1 << i))
                        .collect();
                }
            }
            table
        })
    }

    /// The data of `word` with up to `t` errors corrected, and the bits corrected; `None`
    /// when its syndrome is no such pattern's.
    fn decode(&self, word: u32) -> Option<(u32, u32)> {
        let word = word & ((1 << self.n()) - 1);
        let e = self.table()[self.syndrome(word) as usize];
        (e != NO_PATTERN).then(|| ((word ^ e) >> self.check_bits(), e.count_ones()))
    }
}

/// The slot type's Golay (20,8,7) (B.3.1).
static GOLAY20: Cyclic = Cyclic {
    k: 8,
    poly: GOLAY_POLY,
    extended: true,
    t: 3,
    table: &GOLAY20_TABLE,
};
static GOLAY20_TABLE: OnceLock<Vec<u32>> = OnceLock::new();
/// The EMB's quadratic residue (16,7,6) (B.3.2).
static QR16: Cyclic = Cyclic {
    k: 7,
    poly: QR_POLY,
    extended: true,
    t: 2,
    table: &QR16_TABLE,
};
static QR16_TABLE: OnceLock<Vec<u32>> = OnceLock::new();
/// The TACT's Hamming (7,4,3) (B.3.5).
static HAMMING7: Cyclic = Cyclic {
    k: 4,
    poly: HAMMING3_POLY,
    extended: false,
    t: 1,
    table: &HAMMING7_TABLE,
};
static HAMMING7_TABLE: OnceLock<Vec<u32>> = OnceLock::new();
/// The BPTC's column code, Hamming (13,9,3) (B.3.4).
static HAMMING13: Cyclic = Cyclic {
    k: 9,
    poly: HAMMING4_POLY,
    extended: false,
    t: 1,
    table: &HAMMING13_TABLE,
};
static HAMMING13_TABLE: OnceLock<Vec<u32>> = OnceLock::new();
/// The BPTC's row code, Hamming (15,11,3) (B.3.4).
static HAMMING15: Cyclic = Cyclic {
    k: 11,
    poly: HAMMING4_POLY,
    extended: false,
    t: 1,
    table: &HAMMING15_TABLE,
};
static HAMMING15_TABLE: OnceLock<Vec<u32>> = OnceLock::new();
/// The embedded link control's row code, Hamming (16,11,4) (B.3.4).
static HAMMING16: Cyclic = Cyclic {
    k: 11,
    poly: HAMMING4_POLY,
    extended: true,
    t: 1,
    table: &HAMMING16_TABLE,
};
static HAMMING16_TABLE: OnceLock<Vec<u32>> = OnceLock::new();
/// The AMBE+2 frame's row 1, Golay (23,12) (decoders' values, see the module doc).
static GOLAY23: Cyclic = Cyclic {
    k: 12,
    poly: GOLAY_POLY,
    extended: false,
    t: 3,
    table: &GOLAY23_TABLE,
};
static GOLAY23_TABLE: OnceLock<Vec<u32>> = OnceLock::new();
/// The AMBE+2 frame's row 0, Golay (24,12).
static GOLAY24: Cyclic = Cyclic {
    k: 12,
    poly: GOLAY_POLY,
    extended: true,
    t: 3,
    table: &GOLAY24_TABLE,
};
static GOLAY24_TABLE: OnceLock<Vec<u32>> = OnceLock::new();

// ---- Reed-Solomon (12,9) over GF(2^8) ----

/// `x^8 + x^4 + x^3 + x^2 + 1` (B.14).
const GF_POLY: u16 = 0x11D;
const GF_ORDER: usize = 255;
/// The generator below `x^3` (B.10): `x^3 + 0E x^2 + 38 x + 40`.
const RS_GENERATOR: [u8; 3] = [0x0E, 0x38, 0x40];
const RS_SYMBOLS: usize = 12;

const fn gf_tables() -> ([u8; GF_ORDER], [u8; 256]) {
    let mut exp = [0u8; GF_ORDER];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < GF_ORDER {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= GF_POLY;
        }
        i += 1;
    }
    (exp, log)
}

const GF: ([u8; GF_ORDER], [u8; 256]) = gf_tables();

/// `α^i`.
fn alpha(i: usize) -> u8 {
    GF.0[i % GF_ORDER]
}

/// The exponent of `x`, not zero.
fn gf_log(x: u8) -> u8 {
    GF.1[usize::from(x)]
}

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        alpha(usize::from(gf_log(a)) + usize::from(gf_log(b)))
    }
}

/// `a / b`, `b` not zero.
fn gf_div(a: u8, b: u8) -> u8 {
    if a == 0 {
        0
    } else {
        alpha(usize::from(gf_log(a)) + GF_ORDER - usize::from(gf_log(b)))
    }
}

/// The three parity octets of 9 message octets (the first the highest power): the
/// remainder of the message times `x^3` by the generator.
fn rs_parity(msg: &[u8; LC_BYTES]) -> [u8; 3] {
    let g = RS_GENERATOR;
    msg.iter().fold([0u8; 3], |r, &m| {
        let f = m ^ r[0];
        [
            r[1] ^ gf_mul(f, g[0]),
            r[2] ^ gf_mul(f, g[1]),
            gf_mul(f, g[2]),
        ]
    })
}

/// Corrects `word` (message then parity) in place, one octet at most; the bits corrected,
/// or `None` past the code's reach.
fn rs_decode(word: &mut [u8; RS_SYMBOLS]) -> Option<u32> {
    let s: [u8; 3] = std::array::from_fn(|j| {
        let x = alpha(j + 1);
        word.iter().fold(0, |acc, &c| gf_mul(acc, x) ^ c)
    });
    if s == [0; 3] {
        return Some(0);
    }
    if s[0] == 0 || s[1] == 0 {
        return None;
    }
    // One error of value e at degree d: S_j = e α^(jd).
    let x = gf_div(s[1], s[0]);
    if gf_mul(s[1], x) != s[2] {
        return None;
    }
    let degree = usize::from(gf_log(x));
    if degree >= RS_SYMBOLS {
        return None;
    }
    let e = gf_div(s[0], x);
    word[RS_SYMBOLS - 1 - degree] ^= e;
    Some(e.count_ones())
}

// ---- AMBE+2 frames ----

/// For each bit of a voice frame in the order sent, its cell in [`AmbeSoft`]: bit `n` of
/// the rows in word order (each row from its top cell down) is sent `4 (n mod 18) + n/18`-th.
const AMBE_CELLS: [u8; AMBE_FRAME_BITS] = {
    let mut out = [0u8; AMBE_FRAME_BITS];
    let per_stream = AMBE_FRAME_BITS / AMBE_STREAMS;
    let (mut row, mut n) = (0, 0);
    while row < AMBE_ROW_BITS.len() {
        let mut j = 0;
        while j < AMBE_ROW_BITS[row] {
            let col = AMBE_ROW_BITS[row] - 1 - j;
            out[AMBE_STREAMS * (n % per_stream) + n / per_stream] = (row * AMBE_COLS + col) as u8;
            n += 1;
            j += 1;
        }
        row += 1;
    }
    out
};

/// The mask over row 1, seeded from `u0`: bit `c` for cell `c`.
fn ambe_mask(u0: u16) -> u32 {
    let mut p = AMBE_MASK_SEED * u32::from(u0);
    let mut mask = 0;
    for col in (0..AMBE_ROW_BITS[1]).rev() {
        p = (AMBE_MASK_MUL * p + AMBE_MASK_ADD) & 0xFFFF;
        if p & AMBE_MASK_TOP != 0 {
            mask |= 1 << col;
        }
    }
    mask
}

/// Row `row` of a frame (hard), cell `c` as bit `c`.
fn ambe_row(f: &AmbeSoft, row: usize) -> u32 {
    (0..AMBE_ROW_BITS[row]).fold(0, |w, c| w | u32::from(f[AMBE_COLS * row + c] >= 128) << c)
}

/// The 49 parameter bits as rows (12, 12, 11 and 14 bits) coded into a frame.
fn ambe_encode(u: &[u16; 4]) -> AmbeSoft {
    let words = [
        GOLAY24.encode(u32::from(u[0])),
        GOLAY23.encode(u32::from(u[1])) ^ ambe_mask(u[0] & 0xFFF),
        u32::from(u[2]),
        u32::from(u[3]),
    ];
    let mut f = [0u8; AMBE_FEED_BYTES];
    for (row, &w) in words.iter().enumerate() {
        for c in 0..AMBE_ROW_BITS[row] {
            f[AMBE_COLS * row + c] = 255 * (w >> c & 1) as u8;
        }
    }
    f
}

/// The parameter bits of a frame (hard decisions), and the bits its two Golay words
/// corrected.
fn ambe_decode(f: &AmbeSoft) -> ([u16; 4], u32) {
    let row0 = ambe_row(f, 0);
    let (u0, e0) = GOLAY24.decode(row0).unwrap_or((row0 >> 12, 0));
    let row1 = ambe_row(f, 1) ^ ambe_mask(u0 as u16);
    let (u1, e1) = GOLAY23.decode(row1).unwrap_or((row1 >> 11, 0));
    (
        [
            u0 as u16,
            u1 as u16,
            ambe_row(f, 2) as u16,
            ambe_row(f, 3) as u16,
        ],
        e0 + e1,
    )
}

/// A frame's 72 bits in the order sent (hard).
fn ambe_tx_bits(f: &AmbeSoft) -> [u8; AMBE_FRAME_BITS] {
    AMBE_CELLS.map(|c| u8::from(f[usize::from(c)] >= 128))
}

/// A frame from its 72 soft bits in the order sent.
fn ambe_from_bits(bits: &[u8]) -> AmbeSoft {
    let mut f = [0u8; AMBE_FEED_BYTES];
    for (&c, &b) in AMBE_CELLS.iter().zip(bits) {
        f[usize::from(c)] = b;
    }
    f
}

/// The encoder's pieces, for the `dmr_decode` stage's tests to put calls together burst by
/// burst: calls on each slot starting and ending when a test says, a header or a privacy
/// header left out or added, a burst lost.
#[cfg(test)]
pub(crate) mod build {
    use super::*;

    /// One burst's 264 bits (0 or 1).
    pub(crate) type Burst = [u8; BURST_BITS];

    /// A call's bursts as [`encode_repeater_aliased`] sends them on slot 1: the voice LC
    /// header, a privacy header when `lc.encrypted`, the superframes, the terminator.
    pub(crate) fn call(
        cc: u8,
        lc: &FullLc,
        superframes: u32,
        origin: Origin,
        talker_alias: bool,
    ) -> Vec<Burst> {
        call_bursts(
            cc,
            lc,
            superframes,
            origin,
            talker_alias.then(talker_alias_bytes),
        )
    }

    /// As [`call`], every second superframe carrying a GPS Info message (-2 7.1.1.3,
    /// table 7.3: FLCO `001000`, FID 0) in place of the link control, its position fields
    /// all zero.
    pub(crate) fn call_with_gps(cc: u8, lc: &FullLc, superframes: u32) -> Vec<Burst> {
        let mut gps = [0u8; LC_BYTES];
        gps[0] = FLCO_GPS_INFO;
        call_bursts(cc, lc, superframes, Origin::Repeater, Some(gps))
    }

    pub(crate) fn idle(cc: u8) -> Burst {
        idle_burst(cc)
    }

    /// The privacy header a call encrypted sends after its voice header.
    pub(crate) fn privacy_header(cc: u8, origin: Origin) -> Burst {
        data_burst(
            sync_word(origin, Kind::Data),
            slot_type(cc, DT_PI_HEADER),
            &bptc_encode(&pi_header_info()),
        )
    }

    /// `burst` lost: its centre field (the sync, or the EMB and the embedded bits) made a
    /// word no code decodes and no sync, so the framer reads nothing there.
    pub(crate) fn lose(burst: &mut Burst) {
        let bad = (0..1u32 << 16).find(|&w| QR16.decode(w).is_none()).unwrap();
        let centre = emb_centre(bad, &[1; EMBEDDED_BITS]);
        burst[HALF_BITS..HALF_BITS + CENTRE_BITS].copy_from_slice(&centre);
    }

    /// A repeater's downlink of the two slots' bursts, a CACH before each; a slot that runs
    /// out idles. Dibits.
    pub(crate) fn downlink(cc: u8, slots: [Vec<Burst>; 2]) -> Vec<u8> {
        super::downlink(cc, &slots, [true; 2])
    }

    /// A handheld's bursts on the air, the rest of each 60 ms empty.
    pub(crate) fn handheld(bursts: &[Burst]) -> Vec<Option<u8>> {
        handheld_air(bursts)
    }
}

#[cfg(test)]
mod tests {
    use num_complex::Complex;

    use super::*;
    use crate::fsk4::{Fsk4Demod, Fsk4Params, Shape, dibit_symbol};
    use crate::synth::{rrc_modulate_4fsk, rrc_modulate_4fsk_gated};

    /// The test identities: invented, outside registered ranges.
    const CC: u8 = 5;
    const SRC: u32 = 9_990_001;
    const DST: u32 = 9_990;
    /// The other slot's call.
    const SRC_2: u32 = 9_990_002;
    const DST_2: u32 = 9_991;

    fn group() -> FullLc {
        FullLc {
            call: Call::Group,
            dst: DST,
            src: SRC,
            fid: 0,
            encrypted: false,
            emergency: false,
        }
    }

    fn private() -> FullLc {
        FullLc {
            call: Call::Private,
            dst: SRC_2,
            src: SRC,
            fid: 0x20,
            encrypted: true,
            emergency: true,
        }
    }

    /// Bits (one per byte, 0 or 1) as the decoders take them: soft, certain.
    fn certain(bits: &[u8]) -> Vec<u8> {
        bits.iter().map(|&b| 255 * b).collect()
    }

    /// Every error pattern of `weight` bits among the low `n`.
    fn patterns(n: u32, weight: u32) -> Vec<u32> {
        (0..1u32 << n)
            .filter(|e| e.count_ones() == weight)
            .collect()
    }

    /// Each of `data` encoded, then every pattern of up to the code's `t` errors: each
    /// corrected, with its weight reported.
    fn corrects_every_pattern(code: &Cyclic, data: impl Iterator<Item = u32>) {
        let all: Vec<Vec<u32>> = (1..=code.t).map(|w| patterns(code.n(), w)).collect();
        for d in data {
            let word = code.encode(d);
            assert_eq!(code.decode(word), Some((d, 0)), "data {d:x}");
            for (w, errors) in all.iter().enumerate() {
                for &e in errors {
                    assert_eq!(
                        code.decode(word ^ e),
                        Some((d, w as u32 + 1)),
                        "data {d:x} error {e:x}"
                    );
                }
            }
        }
    }

    /// `code`'s word for each data bit against the standard's generator matrix, given as
    /// the parity columns of each row (the rest of each row is the identity).
    fn matches_generator(code: &Cyclic, parity: &[&str]) {
        assert_eq!(parity.len() as u32, code.k);
        let n = code.n();
        for (i, p) in parity.iter().enumerate() {
            let p = p.chars().fold(0u32, |w, c| w << 1 | u32::from(c == '1'));
            let want = 1 << (n - 1 - i as u32) | p;
            assert_eq!(code.encode(1 << (code.k - 1 - i as u32)), want, "row {i}");
        }
    }

    #[test]
    fn the_codes_are_the_standards_generator_matrices() {
        // TS 102 361-1 tables B.11, B.12, B.14, B.15, B.16 and B.17.
        let golay20 = [
            "001111011010",
            "110110011001",
            "011011001101",
            "001101100111",
            "110111000110",
            "101010010111",
            "100100111110",
            "100011101011",
        ];
        matches_generator(&GOLAY20, &golay20);
        let qr16 = [
            "001001111",
            "100011110",
            "110110111",
            "111100010",
            "111001001",
            "011100101",
            "001110011",
        ];
        matches_generator(&QR16, &qr16);
        let h13 = [
            "1111", "1110", "0111", "1010", "0101", "1011", "1100", "0110", "0011",
        ];
        matches_generator(&HAMMING13, &h13);
        let h15 = [
            "1001", "1101", "1111", "1110", "0111", "1010", "0101", "1011", "1100", "0110", "0011",
        ];
        matches_generator(&HAMMING15, &h15);
        let h16 = [
            "10011", "11010", "11111", "11100", "01110", "10101", "01011", "10110", "11001",
            "01101", "00111",
        ];
        matches_generator(&HAMMING16, &h16);
        matches_generator(&HAMMING7, &["101", "111", "110", "011"]);
        // The AMBE+2 frame's Golay words are P25's, which M17's specification prints as a
        // generator matrix.
        for data in 0..1u32 << 12 {
            assert_eq!(
                GOLAY24.encode(data),
                crate::m17::golay24_encode(data as u16)
            );
            assert_eq!(GOLAY23.encode(data), GOLAY24.encode(data) >> 1);
        }
    }

    #[test]
    fn the_hamming_and_golay_codes_correct_what_they_are_designed_to() {
        corrects_every_pattern(&HAMMING7, 0..1 << 4);
        corrects_every_pattern(&HAMMING13, 0..1 << 9);
        corrects_every_pattern(&HAMMING15, 0..1 << 11);
        corrects_every_pattern(&HAMMING16, 0..1 << 11);
        let samples = [0u32, 0xFFF, 0xA5C, 0x3F1, 0x812];
        corrects_every_pattern(&GOLAY23, samples.into_iter());
        corrects_every_pattern(&GOLAY24, samples.into_iter());
        // The extended codes detect one error more than they correct.
        for d in [0u32, 0x5A3, 0x7FF] {
            let word = HAMMING16.encode(d);
            for e in patterns(16, 2) {
                assert_eq!(HAMMING16.decode(word ^ e), None, "error {e:x}");
            }
        }
        for d in samples {
            let word = GOLAY24.encode(d);
            for e in patterns(24, 4) {
                assert_eq!(GOLAY24.decode(word ^ e), None, "error {e:06x}");
            }
        }
    }

    #[test]
    fn the_emb_and_slot_type_round_trip() {
        // Slot type: colour code and data type, Golay (20,8,7), three errors corrected.
        corrects_every_pattern(&GOLAY20, 0..1 << 8);
        // EMB: colour code, PI and LCSS, QR (16,7,6), two corrected and three detected.
        corrects_every_pattern(&QR16, 0..1 << 7);
        for d in 0..1u32 << 7 {
            let word = QR16.encode(d);
            for e in patterns(16, 3) {
                assert_eq!(QR16.decode(word ^ e), None, "data {d:x} error {e:x}");
            }
        }
        // Each colour code through a burst and back.
        for cc in 0..16u8 {
            let info = bptc_encode(&lc_info(&group(), MASK_VOICE_LC_HEADER));
            let slot_type = GOLAY20.encode(u32::from(cc) << 4 | 1);
            let burst = data_burst(DMR_SYNC_BS_DATA, slot_type, &info);
            let word = burst[98..108]
                .iter()
                .chain(&burst[156..166])
                .fold(0u32, |w, &b| w << 1 | u32::from(b));
            assert_eq!(GOLAY20.decode(word), Some((u32::from(cc) << 4 | 1, 0)));
            let emb = QR16.encode(u32::from(cc) << 3 | 0b101);
            let centre = emb_centre(emb, &[0; 32]);
            let word = centre[..8]
                .iter()
                .chain(&centre[40..])
                .fold(0u32, |w, &b| w << 1 | u32::from(b));
            assert_eq!(QR16.decode(word), Some((u32::from(cc) << 3 | 0b101, 0)));
        }
    }

    #[test]
    fn reed_solomon_is_the_standards_code() {
        // Tables B.19 and B.20: GF(2^8) over x^8 + x^4 + x^3 + x^2 + 1.
        for (e, b) in [(0, 0x01), (8, 0x1D), (25, 0x03), (100, 0x11), (254, 0x8E)] {
            assert_eq!(alpha(e), b, "α^{e}");
        }
        assert_eq!((gf_log(0x03), gf_log(0xFF), gf_log(0x1D)), (25, 175, 8));
        // (B.9) is (B.10): (x + α)(x + α²)(x + α³) = x³ + 0E x² + 38 x + 40.
        let g = (1..=3).fold(vec![1u8], |g, i| {
            let mut next = vec![0u8; g.len() + 1];
            for (j, &c) in g.iter().enumerate() {
                next[j] ^= c;
                next[j + 1] ^= gf_mul(c, alpha(i));
            }
            next
        });
        assert_eq!(g, [0x01, 0x0E, 0x38, 0x40]);
        // Table B.18: the parity of each message octet alone.
        let rows: [[u8; 3]; 9] = [
            [0x1C, 0xBC, 0xFD],
            [0x89, 0x31, 0x08],
            [0xAD, 0x41, 0x36],
            [0x7D, 0x71, 0x16],
            [0xF3, 0xA6, 0x3A],
            [0x08, 0x83, 0x7B],
            [0x3F, 0x6F, 0x02],
            [0x6C, 0x0D, 0xA7],
            [0x0E, 0x38, 0x40],
        ];
        for (i, want) in rows.iter().enumerate() {
            let mut m = [0u8; 9];
            m[i] = 1;
            assert_eq!(rs_parity(&m), *want, "row {i}");
        }
        // Every single-symbol error, in every place, is corrected (the code's reach).
        let msg = lc_bytes(&group());
        let mut word = [0u8; 12];
        word[..9].copy_from_slice(&msg);
        word[9..].copy_from_slice(&rs_parity(&msg));
        for p in 0..12 {
            for e in 1..=255u8 {
                let mut w = word;
                w[p] ^= e;
                assert_eq!(
                    rs_decode(&mut w),
                    Some(e.count_ones()),
                    "place {p} error {e:x}"
                );
                assert_eq!(w, word);
            }
        }
    }

    /// The name figure B.1 gives the BPTC (196,96) matrix's bit at `index` (table B.2's
    /// numbering: R(3) is 0, then the 13 × 15 matrix row by row from 1).
    fn bptc_name(index: usize) -> String {
        if index == 0 {
            return "R(3)".into();
        }
        let (row, col) = ((index - 1) / 15, (index - 1) % 15);
        match (row, col) {
            (0..9, 0..11) => match row * 11 + col {
                q @ 0..3 => format!("R({})", 2 - q),
                q => format!("I({})", 95 - (q - 3)),
            },
            (0..9, _) => format!("H_R{}({})", row + 1, 14 - col),
            _ => format!("H_C{}({})", col + 1, 12 - row),
        }
    }

    /// A table of annex E as `(symbol, bit 1, bit 0)` lines, in the order sent.
    fn annex_e(table: &str) -> Vec<[String; 3]> {
        table
            .lines()
            .map(|l| {
                let t: Vec<&str> = l.split_whitespace().collect();
                [t[0].into(), t[1].into(), t[2].into()]
            })
            .collect()
    }

    /// The burst bit (0 the first sent) each name of `table` is at.
    fn annex_e_bits(table: &str) -> Vec<(String, usize)> {
        annex_e(table)
            .into_iter()
            .enumerate()
            .flat_map(|(i, [_, a, b])| [(a, 2 * i), (b, 2 * i + 1)])
            .collect()
    }

    /// The one bit set in `bits`.
    fn the_one(bits: &[u8]) -> usize {
        let set: Vec<usize> = (0..bits.len()).filter(|&i| bits[i] != 0).collect();
        assert_eq!(set.len(), 1, "{set:?}");
        set[0]
    }

    #[test]
    fn a_data_burst_places_its_fields_as_table_e1() {
        let mut got = vec![String::new(); 264];
        for index in 0..196 {
            let mut tx = [0u8; 196];
            tx[bptc_position(index)] = 1;
            got[the_one(&data_burst(0, 0, &tx))] = bptc_name(index);
        }
        let slot_names = [
            "CC(3)", "CC(2)", "CC(1)", "CC(0)", "DT(3)", "DT(2)", "DT(1)", "DT(0)",
        ];
        for bit in 0..20 {
            let name = match slot_names.get(bit) {
                Some(n) => n.to_string(),
                None => format!("Golay({})", 19 - bit),
            };
            got[the_one(&data_burst(0, 1 << (19 - bit), &[0; 196]))] = name;
        }
        for bit in 0..48 {
            got[the_one(&data_burst(1 << (47 - bit), 0, &[0; 196]))] =
                format!("D_Sync({})", 47 - bit);
        }
        for (name, bit) in annex_e_bits(TABLE_E1) {
            assert_eq!(got[bit], name, "bit {bit}");
        }
    }

    #[test]
    fn a_voice_burst_places_its_fields_as_table_e6() {
        // The vocoder socket: VS(215) sent first, VS(108) at L13's bit 0, VS(107) at R13's
        // bit 1, VS(0) last (table E.6, whose other lines are that run).
        for q in 0..216 {
            let mut vs = [0u8; 216];
            vs[q] = 1;
            let bit = the_one(&voice_burst(&vs, &[0; 48]));
            assert_eq!(bit, if q < 108 { q } else { q + 48 }, "VS({})", 215 - q);
        }
        // The centre: the EMB around the embedded fragment (burst B carries fragment 1).
        let emb_names = [
            "CC(3)", "CC(2)", "CC(1)", "CC(0)", "PI", "LCSS(1)", "LCSS(0)", "QR(8)", "QR(7)",
            "QR(6)", "QR(5)", "QR(4)", "QR(3)", "QR(2)", "QR(1)", "QR(0)",
        ];
        let mut got = vec![String::new(); 48];
        for (bit, name) in emb_names.iter().enumerate() {
            got[the_one(&emb_centre(1 << (15 - bit), &[0; 32]))] = name.to_string();
        }
        for row in 0..8 {
            for col in 0..16 {
                let mut m = [[0u8; 16]; 8];
                m[row][col] = 1;
                let frags = embedded_place(&m);
                if frags[0].contains(&1) {
                    let name = match (row, col) {
                        (7, c) => format!("PC({})", 15 - c),
                        (0..2, c @ 0..11) => format!("LC({})", 71 - (11 * row + c)),
                        (_, c @ 0..10) => format!("LC({})", 71 - (22 + 10 * (row - 2) + c)),
                        (_, 10) => format!("CS({})", 6 - row),
                        (_, c) => format!("H{}({})", row + 1, 15 - c),
                    };
                    got[the_one(&emb_centre(0, &frags[0]))] = name;
                }
            }
        }
        for (name, bit) in annex_e_bits(TABLE_E6_CENTRE) {
            assert_eq!(got[bit], name, "centre bit {bit}");
        }
    }

    #[test]
    fn bptc_round_trips_and_corrects() {
        let info = lc_info(&group(), MASK_TERMINATOR_LC);
        let tx = bptc_encode(&info);
        assert_eq!(bptc_decode(&certain(&tx)), Some((info, 0)));
        let cell = |row: usize, col: usize| bptc_position(1 + 15 * row + col);
        // One error in every row (and so at most one in each column).
        let mut hit = tx;
        for row in 0..13 {
            hit[cell(row, row)] ^= 1;
        }
        assert_eq!(bptc_decode(&certain(&hit)), Some((info, 13)));
        // One error in every column (two in the first two rows).
        let mut hit = tx;
        for col in 0..15 {
            hit[cell(col % 13, col)] ^= 1;
        }
        assert_eq!(bptc_decode(&certain(&hit)), Some((info, 15)));
        // Four in one column and four in one row: the column passes alone cannot take
        // them, the row passes can.
        let mut hit = tx;
        for i in 0..4 {
            hit[cell(2 * i, 3)] ^= 1;
            hit[cell(5, 2 * i + 5)] ^= 1;
        }
        assert_eq!(bptc_decode(&certain(&hit)), Some((info, 8)));
        // Every other bit of the first half sent wrong is past it.
        let mut hit = tx;
        for p in (0..98).step_by(2) {
            hit[p] ^= 1;
        }
        assert_eq!(bptc_decode(&certain(&hit)), None);
    }

    #[test]
    fn the_idle_message_is_the_standards_figure_d1() {
        // Table D.2's information bits, encoded: figure D.1.
        let d1 = [
            "000111111110100",
            "100000111101101",
            "111110001011101",
            "110011001000101",
            "000100101000111",
            "111011010001101",
            "111100111110010",
            "001101100011101",
            "010100100010101",
            "010011100100011",
            "101100111101100",
            "001000010000100",
            "010010101110110",
        ];
        let info: [u8; 96] = std::array::from_fn(|i| IDLE_FILL[i / 8] >> (7 - i % 8) & 1);
        let m = bptc_matrix(&info);
        for (row, want) in d1.iter().enumerate() {
            let got: String = m[row].iter().map(|&b| char::from(b'0' + b)).collect();
            assert_eq!(&got, want, "row {row}");
        }
        // Table B.2's interleave indices (a sample), equation (B.1).
        for (index, interleave) in [
            (0, 0),
            (1, 181),
            (2, 166),
            (4, 136),
            (12, 16),
            (13, 1),
            (26, 2),
            (66, 186),
            (98, 98),
            (131, 191),
            (135, 131),
            (144, 192),
            (195, 15),
        ] {
            assert_eq!(bptc_position(index), interleave, "index {index}");
        }
    }

    #[test]
    fn full_lc_round_trips_with_its_checksum_masks() {
        // TS 102 361-2 tables 7.1 and 7.2: PF, reserved, FLCO; FID; service options;
        // the group or target address; the source address.
        assert_eq!(
            lc_bytes(&group()),
            [0x00, 0x00, 0x00, 0x00, 0x27, 0x06, 0x98, 0x6F, 0x71]
        );
        assert_eq!(
            lc_bytes(&private()),
            [0x03, 0x20, 0xC0, 0x98, 0x6F, 0x72, 0x98, 0x6F, 0x71]
        );
        for lc in [group(), private()] {
            assert_eq!(parse_lc(&lc_bytes(&lc)), Some(lc));
            for (mask, other) in [
                (MASK_VOICE_LC_HEADER, MASK_TERMINATOR_LC),
                (MASK_TERMINATOR_LC, MASK_VOICE_LC_HEADER),
            ] {
                let info = lc_info(&lc, mask);
                assert_eq!(lc_decode(&info, mask), Some((lc, 0)));
                // A header's mask does not check as a terminator's, nor the reverse.
                assert_eq!(lc_decode(&info, other), None);
                // A whole octet wrong: Reed-Solomon (12,9) takes it.
                let mut bad = info;
                bad[8..16].iter_mut().for_each(|b| *b ^= 1);
                assert_eq!(lc_decode(&bad, mask), Some((lc, 8)));
                // Two octets are past it.
                bad[40..48].iter_mut().for_each(|b| *b ^= 1);
                assert_eq!(lc_decode(&bad, mask), None);
            }
        }
        // Another opcode is not a voice channel user's; the protect flag is not read.
        let mut b = lc_bytes(&group());
        b[0] = 0x08;
        assert_eq!(parse_lc(&b), None);
        b[0] = 0x80;
        assert_eq!(parse_lc(&b), None);
    }

    #[test]
    fn embedded_lc_reassembles_from_bursts_b_to_e() {
        for lc in [group(), private()] {
            let b = lc_bytes(&lc);
            let frags: Vec<Vec<u8>> = embedded_encode(&b).iter().map(|f| certain(f)).collect();
            assert_eq!(embedded_decode(&frags), Some((b, 0)));
            // One error in each of the eight rows of the encode matrix.
            let mut m = embedded_matrix(&b, checksum(&b));
            for (row, r) in m.iter_mut().enumerate() {
                r[2 * row] ^= 1;
            }
            let hit: Vec<Vec<u8>> = embedded_place(&m).iter().map(|f| certain(f)).collect();
            assert_eq!(embedded_decode(&hit), Some((b, 8)));
            // A checksum that does not match is refused.
            let m = embedded_matrix(&b, checksum(&b) ^ 1);
            let bad: Vec<Vec<u8>> = embedded_place(&m).iter().map(|f| certain(f)).collect();
            assert_eq!(embedded_decode(&bad), None);
        }
        // (B.26): the octets' sum modulo 31.
        assert_eq!(checksum(&[0xFF; 9]), (9 * 255 % 31) as u8);

        // Through the framer, from a handheld's clean dibits: burst E of each superframe
        // gives it, and no other.
        let call = encode_handheld(CC, private(), 2);
        let dibits: Vec<SoftDibit> = call
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                d.map(|d| SoftDibit {
                    dibit: d,
                    soft: [255 * (d >> 1), 255 * (d & 1)],
                    at: i as f64,
                })
            })
            .collect();
        let bursts = DmrFramer::new().push(&dibits);
        let lcs: Vec<Option<FullLc>> = bursts
            .iter()
            .filter_map(|b| match &b.content {
                DmrContent::Voice { lc, .. } => Some(*lc),
                _ => None,
            })
            .collect();
        let want: Vec<Option<FullLc>> =
            (0..12).map(|i| (i % 6 == 4).then_some(private())).collect();
        assert_eq!(lcs, want);
    }

    #[test]
    fn the_cach_names_the_slot() {
        // Figure B.9: the TACT bits spread over the 24, the payload in the gaps.
        let names = [
            "AT", "P(16)", "P(15)", "P(14)", "TC", "P(13)", "P(12)", "P(11)", "LS(1)", "P(10)",
            "P(9)", "P(8)", "LS(0)", "P(7)", "H(2)", "P(6)", "P(5)", "P(4)", "H(1)", "P(3)",
            "P(2)", "P(1)", "H(0)", "P(0)",
        ];
        let tact = ["AT", "TC", "LS(1)", "LS(0)", "H(2)", "H(1)", "H(0)"];
        for (bit, name) in tact.iter().enumerate() {
            assert_eq!(names[the_one(&cach_place(1 << (6 - bit), 0))], *name);
        }
        for bit in 0..17 {
            let at = the_one(&cach_place(0, 1 << (16 - bit)));
            assert_eq!(names[at], format!("P({})", 16 - bit));
        }
        // Every TACT through its Hamming (7,4,3), one error anywhere in the CACH.
        for at in [false, true] {
            for tc in 0..2u8 {
                for lcss in 0..4u8 {
                    let data = u8::from(at) << 3 | tc << 2 | lcss;
                    let bits = cach_bits(at, tc, lcss);
                    assert_eq!(cach_tact(&certain(&bits)), (data, 0));
                    for i in 0..24 {
                        let mut hit = bits;
                        hit[i] ^= 1;
                        let tact_bit = [0, 4, 8, 12, 14, 18, 22].contains(&i);
                        assert_eq!(cach_tact(&certain(&hit)), (data, u32::from(tact_bit)));
                    }
                }
            }
        }
    }

    /// The pseudo-random mask over row 1 as the AMBE+2 frame's coding has it (mbelib's
    /// demodulation): `p(0) = 16 u0`, `p(n) = (173 p(n-1) + 13849) mod 65536`, the mask
    /// bit `p(n) >= 32768`, over cells 22 down to 0.
    fn mask_cells(u0: u16) -> Vec<(usize, u8)> {
        let mut p = 16 * u32::from(u0);
        (0..23)
            .rev()
            .map(|col| {
                p = (173 * p + 13849) % 65536;
                (col, u8::from(p >= 32768))
            })
            .collect()
    }

    #[test]
    fn an_ambe_frame_lands_in_mbelib_rows() {
        // DSD's `dmr_const.h` (ISC licence), whose DMR interleave mbelib-neo's tools carry
        // too: dibit i of a frame puts its first bit at [rW][rX], its second at [rY][rZ].
        // TS 102 361-1 gives no interleave inside the 72 bits (clause 6.1 places whole
        // frames only), so these are a decoder's values.
        let rw = [
            0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 2, 0, 2, 0, 2, 0,
            2, 0, 2, 0, 2, 0, 2,
        ];
        let rx = [
            23, 10, 22, 9, 21, 8, 20, 7, 19, 6, 18, 5, 17, 4, 16, 3, 15, 2, 14, 1, 13, 0, 12, 10,
            11, 9, 10, 8, 9, 7, 8, 6, 7, 5, 6, 4,
        ];
        let ry = [
            0, 2, 0, 2, 0, 2, 0, 2, 0, 3, 0, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1,
            3, 1, 3, 1, 3, 1, 3,
        ];
        let rz = [
            5, 3, 4, 2, 3, 1, 2, 0, 1, 13, 0, 12, 22, 11, 21, 10, 20, 9, 19, 8, 18, 7, 17, 6, 16,
            5, 15, 4, 14, 3, 13, 2, 12, 1, 11, 0,
        ];
        for i in 0..36 {
            assert_eq!(
                usize::from(AMBE_CELLS[2 * i]),
                24 * rw[i] + rx[i],
                "dibit {i}"
            );
            assert_eq!(
                usize::from(AMBE_CELLS[2 * i + 1]),
                24 * ry[i] + rz[i],
                "dibit {i}"
            );
        }

        let u: [u16; 4] = [0xA5C, 0x3F1, 0x5AA, 0x2D3B];
        let f = ambe_encode(&u);
        let word = |row: usize, len: usize| {
            (0..len).fold(0u32, |w, c| w | u32::from(f[24 * row + c] >= 128) << c)
        };
        for &v in &f {
            assert!(v == 0 || v == 255, "a coded frame is certain: {v}");
        }
        // Row 0 is the Golay (24,12) word of u0, unmasked, its top bit in cell 23 and the
        // parity bit in cell 0.
        assert_eq!(word(0, 24), GOLAY24.encode(u32::from(u[0])));
        assert_eq!(GOLAY24.decode(word(0, 24)), Some((u32::from(u[0]), 0)));
        // Row 1 is the Golay (23,12) word of u1 under the mask seeded from u0.
        let mut plain = word(1, 23);
        for (col, m) in mask_cells(u[0]) {
            plain ^= u32::from(m) << col;
        }
        assert_eq!(plain, GOLAY23.encode(u32::from(u[1])));
        // Rows 2 and 3 are u2 (11 bits) and u3 (14) bare; the other cells are 0.
        assert_eq!(
            (word(2, 11), word(3, 14)),
            (u32::from(u[2]), u32::from(u[3]))
        );
        assert_eq!(f[24 + 23], 0);
        assert!(
            (24 * 2 + 11..24 * 3)
                .chain(24 * 3 + 14..96)
                .all(|i| f[i] == 0)
        );
        assert_eq!(ambe_decode(&f), (u, 0));
        // Three errors in row 0, three in row 1, corrected.
        let mut hit = f;
        for i in [0, 7, 23, 24, 30, 46] {
            hit[i] ^= 255;
        }
        assert_eq!(ambe_decode(&hit), (u, 6));
        // The 72 bits in the order sent, and back.
        assert_eq!(ambe_from_bits(&certain(&ambe_tx_bits(&f))), f);
    }

    /// `dibits` through the 4FSK modulator at 48 kHz, then 10 ms with the carrier off so
    /// the receive filters give out the last symbols, conjugated when `invert`.
    fn on_air(dibits: &[u8], invert: bool) -> Vec<Complex<f32>> {
        let symbols: Vec<u8> = dibits.iter().map(|&d| dibit_symbol(d)).collect();
        let mut iq = rrc_modulate_4fsk(&symbols, DMR_BAUD, DMR_DEVIATION_HZ, 48_000, DMR_RRC_ALPHA);
        iq.extend(std::iter::repeat_n(Complex::new(0.0, 0.0), 480));
        if invert {
            iq.iter_mut().for_each(|s| *s = s.conj());
        }
        iq
    }

    /// A handheld's bursts, nothing (zero amplitude) between them.
    fn on_air_gated(dibits: &[Option<u8>]) -> Vec<Complex<f32>> {
        let symbols: Vec<Option<u8>> = dibits.iter().map(|d| d.map(dibit_symbol)).collect();
        let mut iq =
            rrc_modulate_4fsk_gated(&symbols, DMR_BAUD, DMR_DEVIATION_HZ, 48_000, DMR_RRC_ALPHA);
        iq.extend(std::iter::repeat_n(Complex::new(0.0, 0.0), 480));
        iq
    }

    /// A stage's receive path: the front end, the framer fed in uneven pieces, the front
    /// end turned when the framer says the polarity changed.
    struct Receiver {
        demod: Fsk4Demod,
        turned: bool,
    }

    impl Receiver {
        fn new() -> Self {
            let demod = Fsk4Demod::new(
                48_000,
                Fsk4Params {
                    symbol_rate_baud: DMR_BAUD,
                    deviation_hz: DMR_DEVIATION_HZ,
                    shape: Shape::Rrc {
                        alpha: DMR_RRC_ALPHA,
                    },
                    coast_searches: DMR_COAST_SEARCHES,
                },
            )
            .expect("rate");
            Receiver {
                demod,
                turned: false,
            }
        }

        fn feed(&mut self, iq: &[Complex<f32>], framer: &mut DmrFramer) -> Vec<DmrBurst> {
            let mut bursts = Vec::new();
            for chunk in iq.chunks(777) {
                bursts.extend(framer.push(&self.demod.feed(chunk)));
                if let Some(inverted) = framer.inverted()
                    && inverted != self.turned
                {
                    self.demod.invert(inverted);
                    self.turned = inverted;
                }
            }
            bursts
        }

        fn flush(&mut self, framer: &mut DmrFramer) -> Vec<DmrBurst> {
            framer.push(&self.demod.flush())
        }
    }

    /// The bursts of `iq`, received and flushed at the end.
    fn receive(iq: &[Complex<f32>], framer: &mut DmrFramer) -> Vec<DmrBurst> {
        let mut rx = Receiver::new();
        let mut bursts = rx.feed(iq, framer);
        bursts.extend(rx.flush(framer));
        bursts
    }

    /// Samples from one burst of a slot to the next (60 ms at 48 kHz).
    const SLOT_PERIOD: f64 = 2880.0;
    /// A voice superframe, samples.
    const SUPERFRAME: f64 = DMR_SUPERFRAME_MS as f64 * 48.0;
    /// A burst, samples (27.5 ms).
    const BURST: f64 = 1320.0;
    /// How soon a repeater call joined anywhere has its link control, samples: the worst of
    /// the six offsets measured, 423 ms (a join just past burst C's fragment, which waits
    /// for the next superframe's D, since D, E then B, C is not assembled), and one burst
    /// (60 ms) of margin.
    const JOIN_LC_BOUND: f64 = (423.0 + 60.0) * 48.0;

    fn plan(lc: Option<FullLc>, superframes: u32) -> SlotPlan {
        SlotPlan {
            lc,
            superframes,
            control: false,
        }
    }

    /// The embedded link control a burst gives, if any.
    fn lc_of(b: &DmrBurst) -> Option<FullLc> {
        match b.content {
            DmrContent::Voice { lc, .. } => lc,
            _ => None,
        }
    }

    fn is_voice(b: &DmrBurst) -> bool {
        matches!(b.content, DmrContent::Voice { .. })
    }

    /// `bursts` are a call of `superframes` with link control `lc` on slot `slot`, whole:
    /// the header (and a privacy header when encrypted), the voice bursts (the sync on every
    /// sixth, the embedded link control on every burst E, PI clear, the payload's frames,
    /// no bit corrected), the terminator, each a slot period after the last.
    fn is_a_call(bursts: &[&DmrBurst], lc: FullLc, superframes: usize, slot: Option<u8>) {
        let headers = 1 + usize::from(lc.encrypted);
        assert_eq!(bursts.len(), headers + 1 + 6 * superframes, "{bursts:#?}");
        assert!(bursts.iter().all(|b| b.slot == slot));
        assert_eq!(bursts[0].content, DmrContent::VoiceHeader(Some(lc)));
        if lc.encrypted {
            assert_eq!(bursts[1].content, DmrContent::PrivacyHeader);
        }
        assert!(bursts[..headers].iter().all(|b| b.color_code == Some(CC)));
        for (i, b) in bursts[headers..headers + 6 * superframes]
            .iter()
            .enumerate()
        {
            let DmrContent::Voice {
                first,
                letter,
                lc: embedded,
                other,
                pi,
                voice,
                errors,
            } = &b.content
            else {
                panic!("burst {i}: {b:?}");
            };
            assert_eq!(*first, i % 6 == 0, "burst {i}");
            assert_eq!(*letter, Some((i % 6) as u8), "burst {i}");
            assert_eq!(*embedded, (i % 6 == 4).then_some(lc), "burst {i}");
            assert_eq!(*other, None, "burst {i}");
            assert!(!pi, "burst {i}");
            assert_eq!(*errors, 0, "burst {i}");
            assert_eq!(b.color_code, (i % 6 != 0).then_some(CC), "burst {i}");
            for (f, frame) in voice.iter().enumerate() {
                let n = (3 * i + f) as u32;
                assert_eq!(ambe_decode(frame), (voice_payload(lc.src, n), 0));
            }
        }
        assert_eq!(
            bursts.last().unwrap().content,
            DmrContent::Terminator(Some(lc))
        );
        for w in bursts.windows(2) {
            assert!(
                (w[1].at - w[0].at - SLOT_PERIOD).abs() < 10.0,
                "{} {}",
                w[0].at,
                w[1].at
            );
        }
    }

    #[test]
    fn a_repeater_call_decodes_to_its_bursts() {
        let call = encode_repeater(CC, [plan(Some(group()), 3), plan(None, 3)]);
        // Twenty TDMA frames of 60 ms: CACH, slot 1, CACH, slot 2.
        assert_eq!(call.len(), 20 * 288);
        for invert in [false, true] {
            let mut framer = DmrFramer::new();
            let bursts = receive(&on_air(&call, invert), &mut framer);
            assert_eq!(framer.inverted(), Some(invert));
            assert!(bursts.iter().all(|b| b.origin == Origin::Repeater));
            let one: Vec<&DmrBurst> = bursts.iter().filter(|b| b.slot == Some(1)).collect();
            let two: Vec<&DmrBurst> = bursts.iter().filter(|b| b.slot != Some(1)).collect();
            is_a_call(&one, group(), 3, Some(1));
            assert_eq!(two.len(), 20, "invert {invert}");
            assert!(two.iter().all(|b| b.slot == Some(2)
                && b.content == DmrContent::Idle
                && b.color_code == Some(CC)));
        }
    }

    #[test]
    fn a_talker_alias_superframe_names_its_message_and_keeps_the_link_control() {
        // Table 7.4: PF 0, reserved 0, FLCO 000100, FID 0, format 00 (7 bit), length 4,
        // then "ZETA" in 7-bit characters from the data's first bit.
        let b = talker_alias_bytes();
        assert_eq!(b[..2], [0b0000_0100, 0]);
        assert_eq!(b[2] >> 6, 0b00);
        assert_eq!(b[2] >> 1 & 0x1F, 4);
        let data =
            u64::from_be_bytes([0, b[2], b[3], b[4], b[5], b[6], b[7], b[8]]) & ((1 << 49) - 1);
        let chars: Vec<u8> = (0..4)
            .map(|i| (data >> (42 - 7 * i) & 0x7F) as u8)
            .collect();
        assert_eq!(chars, b"ZETA");
        assert_eq!(parse_lc(&b), None);
        assert_eq!(other_flco(&b), Some(FLCO_TALKER_ALIAS_HEADER));
        assert_eq!(other_flco(&lc_bytes(&group())), None);

        let call =
            encode_repeater_aliased(CC, [plan(Some(group()), 3), plan(None, 3)], [true, false]);
        let bursts = receive(&on_air(&call, false), &mut DmrFramer::new());
        let voice: Vec<&DmrBurst> = bursts
            .iter()
            .filter(|b| b.slot == Some(1) && is_voice(b))
            .collect();
        assert_eq!(voice.len(), 18);
        for (i, b) in voice.iter().enumerate() {
            let DmrContent::Voice { lc, other, .. } = &b.content else {
                unreachable!()
            };
            // The second superframe's burst E completes the alias, the others' the call's
            // link control.
            let (want_lc, want_other) = match i {
                4 | 16 => (Some(group()), None),
                10 => (None, Some(FLCO_TALKER_ALIAS_HEADER)),
                _ => (None, None),
            };
            assert_eq!((*lc, *other), (want_lc, want_other), "burst {i}");
        }
        assert_eq!(
            bursts
                .iter()
                .rfind(|b| b.slot == Some(1))
                .map(|b| &b.content),
            Some(&DmrContent::Terminator(Some(group())))
        );
        // Joined in the alias superframe: the alias, then the link control a superframe on.
        let join = (8.0 * SLOT_PERIOD + 1300.0) as usize;
        let bursts = receive(&on_air(&call, false)[join..], &mut DmrFramer::new());
        let first = bursts.iter().find(|b| lc_of(b).is_some()).unwrap();
        let third = 13.0 * SLOT_PERIOD - join as f64;
        assert!(first.at > third, "{} {third}", first.at);
        assert!(bursts.iter().filter_map(lc_of).all(|lc| lc == group()));
    }

    #[test]
    fn two_calls_are_told_apart_by_slot() {
        let other = FullLc {
            src: SRC_2,
            dst: DST_2,
            ..group()
        };
        let call = encode_repeater(CC, [plan(Some(group()), 2), plan(Some(other), 2)]);
        let bursts = receive(&on_air(&call, false), &mut DmrFramer::new());
        for (slot, lc) in [(1, group()), (2, other)] {
            let mine: Vec<&DmrBurst> = bursts.iter().filter(|b| b.slot == Some(slot)).collect();
            is_a_call(&mine, lc, 2, Some(slot));
        }
        assert_eq!(bursts.len(), 2 * 14);
    }

    #[test]
    fn a_handheld_call_decodes_without_a_cach() {
        let call = encode_handheld(CC, group(), 3);
        // A burst every 60 ms, 27.5 ms on.
        assert_eq!(call.len(), 20 * 288);
        assert_eq!(call.iter().filter(|d| d.is_some()).count(), 20 * 132);
        let bursts = receive(&on_air_gated(&call), &mut DmrFramer::new());
        assert!(bursts.iter().all(|b| b.origin == Origin::Handheld));
        is_a_call(&bursts.iter().collect::<Vec<_>>(), group(), 3, None);
        for (k, b) in bursts.iter().enumerate() {
            assert!(
                (b.at - k as f64 * SLOT_PERIOD).abs() < 10.0,
                "burst {k} at {}",
                b.at
            );
        }
        // An encrypted call says so in its link control and with a privacy header; its
        // EMBs' PI bits stay clear.
        let lc = FullLc {
            encrypted: true,
            ..group()
        };
        let bursts = receive(
            &on_air_gated(&encode_handheld(CC, lc, 1)),
            &mut DmrFramer::new(),
        );
        is_a_call(&bursts.iter().collect::<Vec<_>>(), lc, 1, None);
    }

    #[test]
    fn no_burst_sits_on_one_level() {
        let lc = FullLc {
            encrypted: true,
            ..group()
        };
        let repeater = encode_repeater(CC, [plan(Some(lc), 3), plan(None, 3)]);
        let handheld = encode_handheld(CC, lc, 3);
        let handheld: Vec<u8> = handheld.into_iter().flatten().collect();
        let bursts = repeater
            .chunks(144)
            .map(|c| &c[12..])
            .chain(handheld.chunks(132));
        for (i, burst) in bursts.enumerate() {
            let mut counts = [0usize; 4];
            burst.iter().for_each(|&d| counts[usize::from(d)] += 1);
            assert!(
                counts.iter().all(|&c| c <= 132 / 2),
                "burst {i}: {counts:?}"
            );
        }
        // The payload differs from frame to frame, and from call to call.
        let frames = |seed| (0..54).map(|n| voice_payload(seed, n)).collect::<Vec<_>>();
        assert!(frames(SRC).windows(2).all(|w| w[0] != w[1]));
        assert!(frames(SRC).iter().zip(frames(SRC_2)).all(|(a, b)| *a != b));
    }

    /// Two calls at once on a repeater never send a burst whose payload repeats the burst
    /// before it, which a receiver (an established decoder does) may take for a stuck
    /// carrier and drop.
    #[test]
    fn two_calls_at_once_never_repeat_a_burst() {
        let other = FullLc {
            src: SRC_2,
            dst: DST_2,
            ..group()
        };
        let downlink = encode_repeater(CC, [plan(Some(group()), 3), plan(Some(other), 3)]);
        let payloads: Vec<&[u8]> = downlink
            .chunks(144)
            .map(|slot| &slot[12..12 + 54])
            .collect();
        for (i, w) in payloads.windows(2).enumerate() {
            assert_ne!(w[0], w[1], "burst {} repeats burst {i}", i + 1);
        }
    }

    #[test]
    fn a_repeater_call_joined_anywhere_has_the_lc_within_483_ms() {
        let call = encode_repeater(CC, [plan(Some(group()), 4), plan(None, 4)]);
        let iq = on_air(&call, false);
        // The second superframe starts after the header's frame and the first six.
        let second = 7.0 * SLOT_PERIOD;
        for k in 0..6 {
            // Just past the centre field of slot 1's burst, so that burst's fragment is lost.
            let join = (second + k as f64 * SLOT_PERIOD + 1300.0) as usize;
            let bursts = receive(&iq[join..], &mut DmrFramer::new());
            let found = bursts
                .iter()
                .find(|b| lc_of(b).is_some())
                .expect("a link control");
            assert_eq!(lc_of(found), Some(group()), "join {k}");
            let in_hand = found.at + BURST;
            assert!(in_hand <= JOIN_LC_BOUND, "join {k}: {in_hand}");
            assert!(bursts.iter().filter_map(lc_of).all(|lc| lc == group()));
        }
    }

    #[test]
    fn a_handheld_call_joined_mid_superframe_has_the_lc_within_a_superframe_of_its_first_sync() {
        let call = encode_handheld(CC, group(), 3);
        let iq = on_air_gated(&call);
        // Inside the second superframe's burst C, past its centre field.
        let join = (9.0 * SLOT_PERIOD + 900.0) as usize;
        let bursts = receive(&iq[join..], &mut DmrFramer::new());
        let sync = bursts
            .iter()
            .find(|b| !matches!(b.content, DmrContent::Voice { first: false, .. }))
            .expect("a sync");
        let found = bursts
            .iter()
            .find(|b| lc_of(b).is_some())
            .expect("a link control");
        assert_eq!(lc_of(found), Some(group()));
        assert!(
            found.at + BURST - sync.at <= SUPERFRAME,
            "{} {}",
            sync.at,
            found.at
        );
    }

    /// `dibits` with slot 1's centre field in TDMA frame `frame` overwritten by `centre`
    /// (dibits) from its dibit `from`.
    fn overwrite(dibits: &mut [u8], frame: usize, from: usize, centre: &[u8]) {
        let at = frame * 288 + 12 + 54 + from;
        dibits[at..at + centre.len()].copy_from_slice(centre);
    }

    #[test]
    fn a_lost_burst_a_or_emb_loses_only_its_own_burst() {
        // A word the EMB's code cannot decode.
        let bad = (0..1u32 << 16).find(|&w| QR16.decode(w).is_none()).unwrap();
        let bad: Vec<u8> = (0..8).map(|i| (bad >> (14 - 2 * i) & 3) as u8).collect();
        // The second superframe's A is TDMA frame 7, its C frame 9.
        for lost in [7usize, 9] {
            let mut call = encode_repeater(CC, [plan(Some(group()), 3), plan(None, 3)]);
            if lost == 7 {
                // Inner levels only: no dibit of any sync.
                let noise: Vec<u8> = (0..24).map(|i| [0b00, 0b10][i % 2]).collect();
                overwrite(&mut call, lost, 0, &noise);
            } else {
                overwrite(&mut call, lost, 0, &bad[..4]);
                overwrite(&mut call, lost, 20, &bad[4..]);
            }
            let bursts = receive(&on_air(&call, false), &mut DmrFramer::new());
            let voice: Vec<&DmrBurst> = bursts
                .iter()
                .filter(|b| b.slot == Some(1) && is_voice(b))
                .collect();
            let frames: Vec<usize> = voice
                .iter()
                .map(|b| ((b.at - 120.0) / SLOT_PERIOD).round() as usize)
                .collect();
            let want: Vec<usize> = (1..19).filter(|&f| f != lost).collect();
            assert_eq!(frames, want, "lost {lost}");
            for b in &voice {
                let DmrContent::Voice { errors, .. } = b.content else {
                    unreachable!()
                };
                assert_eq!(errors, 0, "lost {lost}: {b:?}");
            }
        }
    }

    #[test]
    fn a_link_control_that_changes_at_a_boundary_is_never_mixed() {
        let other = FullLc {
            src: SRC_2,
            dst: DST_2,
            ..group()
        };
        // Two calls back to back on slot 1, no terminator and no header between them.
        let first = call_bursts(CC, &group(), 2, Origin::Repeater, None);
        let second = call_bursts(CC, &other, 2, Origin::Repeater, None);
        let slot1: Vec<[u8; BURST_BITS]> = first[..first.len() - 1]
            .iter()
            .chain(&second[1..])
            .copied()
            .collect();
        let idle = vec![idle_burst(CC); slot1.len()];
        let iq = on_air(&downlink(CC, &[slot1, idle], [true, false]), false);
        let lcs = |bursts: &[DmrBurst]| -> Vec<FullLc> {
            bursts
                .iter()
                .filter_map(|b| match b.content {
                    DmrContent::VoiceHeader(lc) | DmrContent::Terminator(lc) => lc,
                    _ => lc_of(b),
                })
                .collect()
        };
        // From the start: the first call's, then the second's.
        let seen = lcs(&receive(&iq, &mut DmrFramer::new()));
        let changes = seen.iter().position(|&lc| lc == other).expect("the second");
        assert!(seen[..changes].iter().all(|&lc| lc == group()), "{seen:?}");
        assert!(seen[changes..].iter().all(|&lc| lc == other), "{seen:?}");
        // Joined just past each burst's centre field across the change (TDMA frames 7 to
        // 18: the first call's second superframe, the second call's first).
        for frame in 7..19 {
            let join = (frame as f64 * SLOT_PERIOD + 1300.0) as usize;
            let seen = lcs(&receive(&iq[join..], &mut DmrFramer::new()));
            assert!(
                seen.iter().all(|&lc| lc == group() || lc == other),
                "frame {frame}: {seen:?}"
            );
            assert!(seen.contains(&other), "frame {frame}: {seen:?}");
        }
    }

    /// Two link controls whose mix in the order D, E then B, C passes every row as received
    /// and the checksum, and names neither call: the second's dst middle octet differs from
    /// the first's by a row code word inside columns 0 to 7 of row 3 (that octet's bits) and
    /// by a multiple of 31, and its src's last bit (row 6, column 9, D's) differs too.
    fn a_mixable_pair() -> (FullLc, FullLc, FullLc) {
        let (b, p) = (0..=255u32)
            .flat_map(|b| (1..=255u32).map(move |p| (b, p)))
            .find(|&(b, p)| {
                HAMMING16.encode(p << 3) & 0x1F == 0
                    && ((b ^ p) as i32 - b as i32).rem_euclid(31) == 0
            })
            .expect("a pair");
        let first = FullLc {
            dst: b << 8 | 0x06,
            ..group()
        };
        let second = FullLc {
            dst: (b ^ p) << 8 | 0x06,
            src: SRC ^ 1,
            ..group()
        };
        let mixed = FullLc {
            dst: second.dst,
            ..first
        };
        (first, second, mixed)
    }

    #[test]
    fn the_dropped_join_order_would_mix_two_link_controls() {
        // What D, E then B, C would give for the pair: a third link control, every row a
        // code word as received and the checksum passing.
        let (first, second, mixed) = a_mixable_pair();
        let old = embedded_encode(&lc_bytes(&first));
        let new = embedded_encode(&lc_bytes(&second));
        let fragments: Vec<Vec<u8>> = [new[0], new[1], old[2], old[3]]
            .iter()
            .map(|f| certain(f))
            .collect();
        assert_eq!(embedded_decode(&fragments), Some((lc_bytes(&mixed), 0)));
        assert!(mixed != first && mixed != second);
        // The two orders kept: a mix taken as received either fails or is exactly one of the
        // two link controls, for this pair and for many others.
        let mut pairs = vec![(first, second)];
        for n in 0..256u64 {
            let z = splitmix64(n);
            let lc = |bits: u64| FullLc {
                dst: (bits & 0xFF_FFFF) as u32,
                src: (bits >> 24 & 0xFF_FFFF) as u32,
                ..group()
            };
            // Half the pairs differ in a few low bits only, as neighbouring IDs do.
            let other = if n % 2 == 0 {
                z >> 13 ^ z << 5
            } else {
                z ^ (n >> 1 & 0xFF)
            };
            pairs.push((lc(z), lc(other)));
        }
        for (a, b) in pairs {
            let old = embedded_encode(&lc_bytes(&a));
            let new = embedded_encode(&lc_bytes(&b));
            for frags in [
                [new[0], old[1], old[2], old[3]],
                [new[0], new[1], new[2], old[3]],
            ] {
                let f: Vec<Vec<u8>> = frags.iter().map(|f| certain(f)).collect();
                if let Some((bytes, 0)) = embedded_decode(&f) {
                    let lc = parse_lc(&bytes);
                    assert!(lc == Some(a) || lc == Some(b), "{a:?} {b:?}: {lc:?}");
                }
            }
        }
    }

    #[test]
    fn a_call_that_changes_where_a_receiver_joins_never_gives_a_mixed_lc() {
        // Call pairs back to back on slot 1 (no terminator, no header between), joined just
        // past the first call's last burst C's fragment, so D, E of the first and B, C of the
        // second come first: the mixable pair and 64 others.
        let (first, second, _) = a_mixable_pair();
        let mut pairs = vec![(first, second)];
        for n in 0..64u64 {
            let z = splitmix64(n ^ 0xD3);
            let lc = |bits: u64| FullLc {
                dst: (bits & 0xFF_FFFF) as u32,
                src: (bits >> 24 & 0xFF_FFFF) as u32,
                ..group()
            };
            pairs.push((lc(z), lc(z >> 16 ^ z << 7)));
        }
        for (i, (a, b)) in pairs.into_iter().enumerate() {
            let one = call_bursts(CC, &a, 2, Origin::Repeater, None);
            let two = call_bursts(CC, &b, 2, Origin::Repeater, None);
            let slot1: Vec<[u8; BURST_BITS]> = one[..one.len() - 1]
                .iter()
                .chain(&two[1..])
                .copied()
                .collect();
            let idle = vec![idle_burst(CC); slot1.len()];
            let dibits = downlink(CC, &[slot1, idle], [true, false]);
            // The first call's second superframe's C is TDMA frame 9.
            let join = 9 * 288 + 12 + 54 + 24 + 10;
            let joined: Vec<Option<u8>> = dibits[join..].iter().map(|&d| Some(d)).collect();
            let bursts = DmrFramer::new().push(&clean(&joined));
            let seen: Vec<FullLc> = bursts
                .iter()
                .filter_map(|x| match x.content {
                    DmrContent::Terminator(lc) => lc,
                    _ => lc_of(x),
                })
                .collect();
            assert!(
                seen.iter().all(|&lc| lc == a || lc == b),
                "pair {i}: {seen:?}"
            );
            assert!(seen.contains(&b), "pair {i}: {seen:?}");
        }
    }

    /// `seconds` of noise (white, the carrier 40 dB under it) from `seed`.
    fn noise(seconds: f64, seed: u64) -> Vec<Complex<f32>> {
        let mut x = vec![Complex::new(1.0f32, 0.0); (seconds * 48_000.0) as usize];
        crate::synth::awgn(&mut x, -40.0, seed);
        x
    }

    /// A repeater call (slot 1) and a handheld call of `superframes`, each cut short with no
    /// terminator, on air.
    fn cut_calls(superframes: u32) -> [(&'static str, Vec<Complex<f32>>); 2] {
        let mut repeater = call_bursts(CC, &group(), superframes, Origin::Repeater, None);
        repeater.pop();
        let idle = vec![idle_burst(CC); repeater.len()];
        let repeater = on_air(&downlink(CC, &[repeater, idle], [true, false]), false);
        let mut handheld = encode_handheld(CC, group(), superframes);
        handheld.truncate(handheld.len() - 288);
        [
            ("repeater", repeater),
            ("handheld", on_air_gated(&handheld)),
        ]
    }

    /// How soon after a signal ends the flywheel lets go of its slots, samples: two
    /// superframes without a sync or an EMB as sent, counted from the last burst, plus the
    /// front end's latency on the noise that follows.
    const TRACKS_GONE_WITHIN: f64 = 2.0 * SUPERFRAME + 2.0 * SLOT_PERIOD;

    #[test]
    fn after_a_signal_ends_the_flywheel_stops_within_two_superframes() {
        for (name, iq) in cut_calls(3) {
            let mut framer = DmrFramer::new();
            let mut rx = Receiver::new();
            let mut bursts = rx.feed(&iq, &mut framer);
            assert!(!framer.tracks.is_empty(), "{name}");
            bursts.extend(rx.feed(&noise(TRACKS_GONE_WITHIN / 48_000.0, 11), &mut framer));
            assert!(
                framer.tracks.is_empty(),
                "{name}: {} left",
                framer.tracks.len()
            );
            bursts.extend(rx.feed(&noise(1.0, 12), &mut framer));
            bursts.extend(rx.flush(&mut framer));
            assert_eq!(bursts.iter().filter(|b| is_voice(b)).count(), 18, "{name}");
            assert!(bursts.iter().all(|b| b.at < iq.len() as f64), "{name}");
        }
    }

    #[test]
    fn a_minute_of_noise_after_a_call_is_never_voice() {
        for seed in 1..=3 {
            for (name, iq) in cut_calls(3) {
                let end = iq.len() as f64;
                let mut framer = DmrFramer::new();
                let mut rx = Receiver::new();
                let mut bursts = rx.feed(&iq, &mut framer);
                bursts.extend(rx.feed(&noise(60.0, seed), &mut framer));
                bursts.extend(rx.flush(&mut framer));
                let late: Vec<&DmrBurst> = bursts.iter().filter(|b| b.at >= end).collect();
                assert!(late.is_empty(), "{name} seed {seed}: {late:?}");
                assert_eq!(bursts.iter().filter(|b| is_voice(b)).count(), 18, "{name}");
            }
        }
    }

    #[test]
    fn a_voice_header_then_noise_is_not_voice() {
        // The header's link control is in hand, the phase never known.
        let header = call_bursts(CC, &group(), 1, Origin::Repeater, None)[..1].to_vec();
        let repeater = on_air(
            &downlink(CC, &[header, vec![idle_burst(CC)]], [true, false]),
            false,
        );
        let handheld: Vec<Option<u8>> = encode_handheld(CC, group(), 1)[..288].to_vec();
        for (name, iq) in [
            ("repeater", repeater),
            ("handheld", on_air_gated(&handheld)),
        ] {
            for seed in 1..=3 {
                let mut iq = iq.clone();
                let end = iq.len() as f64;
                iq.extend(noise(2.0, seed));
                let mut framer = DmrFramer::new();
                let bursts = receive(&iq, &mut framer);
                assert!(
                    bursts.iter().all(|b| !is_voice(b)),
                    "{name} {seed}: {bursts:?}"
                );
                assert!(bursts.iter().all(|b| b.at < end), "{name} {seed}");
                assert!(framer.tracks.is_empty(), "{name} {seed}");
            }
        }
    }

    #[test]
    fn a_call_joined_too_late_for_its_link_control_then_noise_is_not_voice() {
        // A repeater call cut short, joined just past its last superframe's burst B: its C
        // to F come with no link control in hand, then noise.
        let [(_, iq), _] = cut_calls(3);
        let join = (14.0 * SLOT_PERIOD + 1300.0) as usize;
        for seed in 1..=3 {
            let mut iq = iq[join..].to_vec();
            let end = iq.len() as f64;
            iq.extend(noise(2.0, seed));
            let mut framer = DmrFramer::new();
            let bursts = receive(&iq, &mut framer);
            assert!(bursts.iter().all(|b| lc_of(b).is_none()), "seed {seed}");
            let voice: Vec<&DmrBurst> = bursts.iter().filter(|b| is_voice(b)).collect();
            assert_eq!(voice.len(), 4, "seed {seed}: {voice:?}");
            assert!(bursts.iter().all(|b| b.at < end), "seed {seed}");
            assert!(framer.tracks.is_empty(), "seed {seed}");
        }
    }

    /// `dibits` with the CACH at `at` given two bit errors in its TACT that the Hamming
    /// (7,4) code takes for a word naming the other slot.
    fn misread_tc(dibits: &mut [u8], at: usize) {
        let bits: Vec<u8> = dibits[at..at + 12]
            .iter()
            .flat_map(|d| [d >> 1, d & 1])
            .collect();
        let tc = cach_tact(&certain(&bits)).0 >> 2 & 1;
        let (i, j) = (0..7)
            .flat_map(|i| (i + 1..7).map(move |j| (i, j)))
            .find(|&(i, j)| {
                let mut hit = bits.clone();
                hit[TACT_PLACES[i]] ^= 1;
                hit[TACT_PLACES[j]] ^= 1;
                cach_tact(&certain(&hit)).0 >> 2 & 1 != tc
            })
            .expect("two errors that flip TC");
        let mut hit = bits;
        hit[TACT_PLACES[i]] ^= 1;
        hit[TACT_PLACES[j]] ^= 1;
        dibits[at..at + 12].copy_from_slice(&dibits_of(&hit));
    }

    #[test]
    fn a_misread_tc_moves_no_slot() {
        // Slot 2's idle burst in TDMA frame 9 (the second superframe's C), and slot 1's
        // burst A in frame 7, each behind a CACH whose TC two bit errors flip.
        for (frame, slot) in [(9usize, 2usize), (7, 1)] {
            let mut call = encode_repeater(CC, [plan(Some(group()), 3), plan(None, 3)]);
            misread_tc(&mut call, frame * 288 + (slot - 1) * 144);
            let bursts = receive(&on_air(&call, false), &mut DmrFramer::new());
            let voice: Vec<&DmrBurst> = bursts.iter().filter(|b| is_voice(b)).collect();
            assert!(voice.iter().all(|b| b.slot == Some(1)), "frame {frame}");
            let frames: Vec<usize> = voice
                .iter()
                .map(|b| ((b.at - 120.0) / SLOT_PERIOD).round() as usize)
                .collect();
            assert_eq!(frames, (1..19).collect::<Vec<_>>(), "frame {frame}");
            for b in &voice {
                let DmrContent::Voice { errors, .. } = b.content else {
                    unreachable!()
                };
                assert_eq!(errors, 0, "frame {frame}: {b:?}");
            }
            // The link control at every burst E, the one after the misread included.
            let lcs: Vec<usize> = voice
                .iter()
                .zip(&frames)
                .filter(|(b, _)| lc_of(b) == Some(group()))
                .map(|(_, &f)| f)
                .collect();
            assert_eq!(lcs, [5, 11, 17], "frame {frame}");
            let idle = bursts.iter().filter(|b| b.content == DmrContent::Idle);
            assert!(idle.clone().all(|b| b.slot == Some(2)), "frame {frame}");
            assert_eq!(idle.count(), 20, "frame {frame}");
        }
    }

    #[test]
    fn a_re_timed_repeater_is_followed_after_three_syncs() {
        // A repeater cut half a TDMA frame into frame 10, then another downlink: its slot 1
        // falls on the first's slot 2 grid, and its CACHs say so on every sync.
        let other = FullLc {
            src: SRC_2,
            dst: DST_2,
            ..group()
        };
        let first = encode_repeater(CC, [plan(Some(group()), 2), plan(None, 2)]);
        let second = encode_repeater(CC, [plan(Some(other), 2), plan(None, 2)]);
        let cut = 10 * 288 + 144;
        let dibits: Vec<u8> = first[..cut].iter().chain(&second).copied().collect();
        let bursts = receive(&on_air(&dibits, false), &mut DmrFramer::new());
        // From the second downlink's burst A (its TDMA frame 1) on: its slots as it names
        // them, its call whole.
        let from = (cut + 288) as f64 * 10.0;
        let after: Vec<&DmrBurst> = bursts.iter().filter(|b| b.at >= from).collect();
        let one: Vec<&DmrBurst> = after
            .iter()
            .copied()
            .filter(|b| b.slot == Some(1))
            .collect();
        assert!(
            after
                .iter()
                .all(|b| b.slot == Some(1) || b.content == DmrContent::Idle)
        );
        assert_eq!(one.len(), 13, "{one:?}");
        assert!(one[..12].iter().all(|b| is_voice(b)));
        assert_eq!(one[12].content, DmrContent::Terminator(Some(other)));
        let lcs: Vec<FullLc> = one.iter().filter_map(|b| lc_of(b)).collect();
        assert_eq!(lcs, [other, other]);
    }

    #[test]
    fn a_control_channel_gives_control_bursts() {
        let control = SlotPlan {
            lc: Some(group()),
            superframes: 1,
            control: true,
        };
        let channel = encode_repeater(CC, [control, plan(None, 1)]);
        let bursts = receive(&on_air(&channel, false), &mut DmrFramer::new());
        let one: Vec<&DmrBurst> = bursts.iter().filter(|b| b.slot == Some(1)).collect();
        assert_eq!(one.len(), 8);
        assert!(one.iter().all(|b| b.content == DmrContent::Control));
        // Each a Preamble CSBK whose CRC-CCITT checks under the CSBK mask.
        for (i, burst) in channel.chunks(288).enumerate() {
            let bits: Vec<u8> = burst[12..144]
                .iter()
                .flat_map(|d| [255 * (d >> 1), 255 * (d & 1)])
                .collect();
            let bits: [u8; BURST_BITS] = bits.try_into().unwrap();
            let (info, _) = bptc_decode(&info_bits(&bits)).expect("BPTC");
            let mut bytes = [0u8; 12];
            for (j, &b) in info.iter().enumerate() {
                bytes[j / 8] |= b << (7 - j % 8);
            }
            assert_eq!(&bytes[..4], &[0xBD, 0x00, 0x40, (7 - i) as u8]);
            let crc = u16::from_be_bytes([bytes[10], bytes[11]]);
            assert_eq!(crc ^ MASK_CSBK, crc_ccitt(&bytes[..10]));
        }
    }

    #[test]
    fn the_crc_ccitt_is_b_3_8s() {
        // x^16 + x^12 + x^5 + 1, initial remainder 0, inverted: the CRC catalogue's
        // CRC-16/GSM, whose check value over "123456789" is CE3C.
        assert_eq!(crc_ccitt(b"123456789"), 0xCE3C);
        let spec = crate::crc::CrcSpec {
            name: "b.3.8",
            width: 16,
            poly: 0x1021,
            init: 0,
            refin: false,
            refout: false,
            xorout: 0xFFFF,
            check: 0xCE3C,
        };
        for data in [&b""[..], b"\x00", b"DMR", &[0xA5; 10]] {
            assert_eq!(u64::from(crc_ccitt(data)), crate::crc::crc(&spec, data));
        }
    }

    /// Clean soft dibits, one sample apart, the gaps left out.
    fn clean(dibits: &[Option<u8>]) -> Vec<SoftDibit> {
        dibits
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                d.map(|d| SoftDibit {
                    dibit: d,
                    soft: [255 * (d >> 1), 255 * (d & 1)],
                    at: i as f64,
                })
            })
            .collect()
    }

    #[test]
    fn another_data_type_is_reported_as_other() {
        // A handheld's data header (data type 0110), between a call's bursts.
        let mut call = encode_handheld(CC, group(), 1);
        let header = data_burst(
            DMR_SYNC_MS_DATA,
            slot_type(CC, 0b0110),
            &bptc_encode(&[1; 96]),
        );
        for (d, x) in call[..132].iter_mut().zip(dibits_of(&header)) {
            *d = Some(x);
        }
        let bursts = DmrFramer::new().push(&clean(&call));
        assert_eq!(bursts[0].content, DmrContent::Other(0b0110));
        assert_eq!(bursts[0].color_code, Some(CC));
    }

    #[test]
    fn a_colour_code_is_learnt_only_from_a_slot_type_or_a_clean_emb() {
        // A handheld's call from its burst A (no header, so no slot type), burst B's EMB
        // colour code 9's with two bits wrong, which the code takes for colour code 9's.
        let mut call = encode_handheld(CC, group(), 1)[288..].to_vec();
        let wrong = QR16.encode(9 << 3 | u32::from(LCSS_FIRST)) ^ 0b101;
        assert_eq!(
            QR16.decode(wrong),
            Some((9 << 3 | u32::from(LCSS_FIRST), 2))
        );
        let emb: Vec<Option<u8>> = (0..8)
            .map(|i| Some((wrong >> (14 - 2 * i) & 3) as u8))
            .collect();
        let centre = 288 + 54;
        call[centre..centre + 4].copy_from_slice(&emb[..4]);
        call[centre + 20..centre + 24].copy_from_slice(&emb[4..]);
        let bursts = DmrFramer::new().push(&clean(&call));
        let codes: Vec<Option<u8>> = bursts
            .iter()
            .filter(|b| is_voice(b))
            .map(|b| b.color_code)
            .collect();
        assert_eq!(
            codes,
            [None, Some(9), Some(CC), Some(CC), Some(CC), Some(CC)]
        );
    }

    #[test]
    fn no_row_code_word_lies_inside_one_fragments_columns() {
        // A fragment holds four columns of the embedded matrix (B 0 to 3, ..., E 12 to 15);
        // column c is bit 15 - c of a row's word.
        let words: Vec<u32> = (1..1u32 << 11).map(|d| HAMMING16.encode(d)).collect();
        let inside = |w: u32, cols: std::ops::Range<u32>| {
            (0..16).all(|c| w >> (15 - c) & 1 == 0 || cols.contains(&c))
        };
        // B new, C to E old; B to D new, E old: no word, so no mix passes.
        assert!(!words.iter().any(|&w| inside(w, 0..4)));
        assert!(!words.iter().any(|&w| inside(w, 12..16)));
        // B and C new, D and E old: there are words, so that order is not assembled.
        assert!(words.iter().any(|&w| inside(w, 0..8)));
    }

    /// Table E.1 of TS 102 361-1 V2.5.1 (the general data burst with SYNC), symbol by
    /// symbol in the order sent: each symbol's bit 1, then its bit 0.
    const TABLE_E1: &str = "\
        L66 R(3) H_R1(2)\n\
        L65 I(77) I(68)\n\
        L64 I(59) I(50)\n\
        L63 I(41) I(32)\n\
        L62 H_R7(1) H_R8(3)\n\
        L61 I(1) H_C8(3)\n\
        L60 H_C6(2) H_C4(1)\n\
        L59 H_C2(0) H_C15(0)\n\
        L58 H_R1(3) I(78)\n\
        L57 I(69) I(60)\n\
        L56 I(51) I(42)\n\
        L55 H_R6(0) H_R7(2)\n\
        L54 I(11) I(2)\n\
        L53 H_C7(3) H_C5(2)\n\
        L52 H_C3(1) H_C1(0)\n\
        L51 H_C14(0) I(88)\n\
        L50 I(79) I(70)\n\
        L49 I(61) I(52)\n\
        L48 I(43) H_R6(1)\n\
        L47 H_R7(3) I(12)\n\
        L46 I(3) H_C6(3)\n\
        L45 H_C4(2) H_C2(1)\n\
        L44 H_C15(1) H_C13(0)\n\
        L43 I(89) I(80)\n\
        L42 I(71) I(62)\n\
        L41 I(53) H_R5(0)\n\
        L40 H_R6(2) I(22)\n\
        L39 I(13) I(4)\n\
        L38 H_C5(3) H_C3(2)\n\
        L37 H_C1(1) H_C14(1)\n\
        L36 H_C12(0) I(90)\n\
        L35 I(81) I(72)\n\
        L34 I(63) I(54)\n\
        L33 H_R5(1) H_R6(3)\n\
        L32 I(23) I(14)\n\
        L31 I(5) H_C4(3)\n\
        L30 H_C2(2) H_C15(2)\n\
        L29 H_C13(1) H_C11(0)\n\
        L28 I(91) I(82)\n\
        L27 I(73) I(64)\n\
        L26 H_R4(0) H_R5(2)\n\
        L25 I(33) I(24)\n\
        L24 I(15) I(6)\n\
        L23 H_C3(3) H_C1(2)\n\
        L22 H_C14(2) H_C12(1)\n\
        L21 H_C10(0) I(92)\n\
        L20 I(83) I(74)\n\
        L19 I(65) H_R4(1)\n\
        L18 H_R5(3) I(34)\n\
        L17 CC(3) CC(2)\n\
        L16 CC(1) CC(0)\n\
        L15 DT(3) DT(2)\n\
        L14 DT(1) DT(0)\n\
        L13 Golay(11) Golay(10)\n\
        L12 D_Sync(47) D_Sync(46)\n\
        L11 D_Sync(45) D_Sync(44)\n\
        L10 D_Sync(43) D_Sync(42)\n\
        L9 D_Sync(41) D_Sync(40)\n\
        L8 D_Sync(39) D_Sync(38)\n\
        L7 D_Sync(37) D_Sync(36)\n\
        L6 D_Sync(35) D_Sync(34)\n\
        L5 D_Sync(33) D_Sync(32)\n\
        L4 D_Sync(31) D_Sync(30)\n\
        L3 D_Sync(29) D_Sync(28)\n\
        L2 D_Sync(27) D_Sync(26)\n\
        L1 D_Sync(25) D_Sync(24)\n\
        R1 D_Sync(23) D_Sync(22)\n\
        R2 D_Sync(21) D_Sync(20)\n\
        R3 D_Sync(19) D_Sync(18)\n\
        R4 D_Sync(17) D_Sync(16)\n\
        R5 D_Sync(15) D_Sync(14)\n\
        R6 D_Sync(13) D_Sync(12)\n\
        R7 D_Sync(11) D_Sync(10)\n\
        R8 D_Sync(9) D_Sync(8)\n\
        R9 D_Sync(7) D_Sync(6)\n\
        R10 D_Sync(5) D_Sync(4)\n\
        R11 D_Sync(3) D_Sync(2)\n\
        R12 D_Sync(1) D_Sync(0)\n\
        R13 Golay(9) Golay(8)\n\
        R14 Golay(7) Golay(6)\n\
        R15 Golay(5) Golay(4)\n\
        R16 Golay(3) Golay(2)\n\
        R17 Golay(1) Golay(0)\n\
        R18 I(25) I(16)\n\
        R19 I(7) H_C2(3)\n\
        R20 H_C15(3) H_C13(2)\n\
        R21 H_C11(1) H_C9(0)\n\
        R22 I(93) I(84)\n\
        R23 I(75) H_R3(0)\n\
        R24 H_R4(2) I(44)\n\
        R25 I(35) I(26)\n\
        R26 I(17) I(8)\n\
        R27 H_C1(3) H_C14(3)\n\
        R28 H_C12(2) H_C10(1)\n\
        R29 H_C8(0) I(94)\n\
        R30 I(85) I(76)\n\
        R31 H_R3(1) H_R4(3)\n\
        R32 I(45) I(36)\n\
        R33 I(27) I(18)\n\
        R34 I(9) H_R9(0)\n\
        R35 H_C13(3) H_C11(2)\n\
        R36 H_C9(1) H_C7(0)\n\
        R37 I(95) I(86)\n\
        R38 H_R2(0) H_R3(2)\n\
        R39 I(55) I(46)\n\
        R40 I(37) I(28)\n\
        R41 I(19) I(10)\n\
        R42 H_R9(1) H_C12(3)\n\
        R43 H_C10(2) H_C8(1)\n\
        R44 H_C6(0) R(0)\n\
        R45 I(87) H_R2(1)\n\
        R46 H_R3(3) I(56)\n\
        R47 I(47) I(38)\n\
        R48 I(29) I(20)\n\
        R49 H_R8(0) H_R9(2)\n\
        R50 H_C11(3) H_C9(2)\n\
        R51 H_C7(1) H_C5(0)\n\
        R52 R(1) H_R1(0)\n\
        R53 H_R2(2) I(66)\n\
        R54 I(57) I(48)\n\
        R55 I(39) I(30)\n\
        R56 I(21) H_R8(1)\n\
        R57 H_R9(3) H_C10(3)\n\
        R58 H_C8(2) H_C6(1)\n\
        R59 H_C4(0) R(2)\n\
        R60 H_R1(1) H_R2(3)\n\
        R61 I(67) I(58)\n\
        R62 I(49) I(40)\n\
        R63 I(31) H_R7(0)\n\
        R64 H_R8(2) I(0)\n\
        R65 H_C9(3) H_C7(2)\n\
        R66 H_C5(1) H_C3(0)";

    /// Table E.6 (the voice burst with embedded signalling fragment 1), its centre: the
    /// 24 symbols L12 to R12, as the 48 bits of the centre field.
    const TABLE_E6_CENTRE: &str = "\
        L12 CC(3) CC(2)\n\
        L11 CC(1) CC(0)\n\
        L10 PI LCSS(1)\n\
        L9 LCSS(0) QR(8)\n\
        L8 LC(71) LC(60)\n\
        L7 LC(49) LC(39)\n\
        L6 LC(29) LC(19)\n\
        L5 LC(9) PC(15)\n\
        L4 LC(70) LC(59)\n\
        L3 LC(48) LC(38)\n\
        L2 LC(28) LC(18)\n\
        L1 LC(8) PC(14)\n\
        R1 LC(69) LC(58)\n\
        R2 LC(47) LC(37)\n\
        R3 LC(27) LC(17)\n\
        R4 LC(7) PC(13)\n\
        R5 LC(68) LC(57)\n\
        R6 LC(46) LC(36)\n\
        R7 LC(26) LC(16)\n\
        R8 LC(6) PC(12)\n\
        R9 QR(7) QR(6)\n\
        R10 QR(5) QR(4)\n\
        R11 QR(3) QR(2)\n\
        R12 QR(1) QR(0)";
}
