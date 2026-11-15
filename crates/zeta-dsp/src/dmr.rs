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
//! - **Voice superframe** (-1 5.1.2): an optional voice LC header, superframes of bursts A to
//!   F (360 ms), then a terminator with link control. Idle bursts carry table D.2's 96 fill
//!   bits.
//! - **Vocoder socket** (-1 6.1): 216 bits, VS(215) first; three 72-bit frames, VF(1)'s
//!   most significant bit first.
//!
//! **PI and privacy.** -1 V2.5.1 names the EMB's PI bit the "pre-emption and power control
//! indicator" (9.3.2; table 9.19's caption, and earlier editions, call it the privacy
//! indicator). [`DmrContent::Voice`]'s `privacy` is that bit as received, named for the
//! earlier use; the encoder sets it on an encrypted call. [`FullLc::encrypted`] is the
//! privacy bit of the service options (-2 table 7.11), which -2 does not define further.
//! Nothing here decrypts.
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
//! - **The voice payload** of an encoded call is a frame coded here from 49 zero bits. A
//!   DMR silence frame is widely quoted, from hotspot software, but no public source that
//!   states it as the standard's was found, so it is not used.
//!
//! **The framer.** It searches the four syncs and takes the rest of a burst by position:
//! each dibit's [`SoftDibit::at`], never a count of dibits, since the front end gives held
//! symbols out late and a handheld's bursts each start a fresh acquisition. The symbol
//! period is measured from the spacing of the dibits' positions. A repeater's burst takes
//! its slot from the CACH before it; a handheld's has none (`slot` is `None`). Bursts B to F
//! have no sync: after a burst A the framer expects them a slot period apart and reads
//! each one's EMB, refusing one whose EMB does not decode or names another colour code.
//! Polarity: a data burst whose slot type and BPTC both check tells it, and
//! [`DmrFramer::inverted`] says it, as P25's framer does.

use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::fsk4::{SoftDibit, sync_dibits};

/// Whether a group or a unit-to-unit call (shared with the other voice stages).
pub use crate::voice_call::Call;

/// DMR's symbol rate, baud (-1 10.2.1).
pub const DMR_BAUD: u32 = 4800;
/// The outer symbols' deviation, Hz (-1 10.2.2.1); the inner ones a third of it.
pub const DMR_DEVIATION_HZ: f64 = 1944.0;
/// The root-raised-cosine roll-off (-1 10.2.2.2).
pub const DMR_RRC_ALPHA: f64 = 0.2;
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
/// The 49 parameter bits of an encoded call's voice, by row: zeros.
const VOICE_PAYLOAD: [u16; 4] = [0; 4];

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
    /// A voice burst: `first` for burst A (the sync); `lc` the embedded link control, on
    /// the burst that completes it (E); `privacy` the EMB's PI bit (`false` on burst A,
    /// which has no EMB); `errors` the bits corrected by the EMB's code, the embedded link
    /// control's and the three frames' Golay words.
    Voice {
        first: bool,
        lc: Option<FullLc>,
        privacy: bool,
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

/// A voice superframe in progress on one slot: bursts B to F are expected after its A.
struct Superframe {
    slot: Option<u8>,
    origin: Origin,
    /// Burst A's centre field, a sample position.
    sync_at: f64,
    /// The burst expected next: 1 for B to 5 for F.
    next: usize,
    /// Embedded fragments so far, from burst B on.
    fragments: Vec<Vec<u8>>,
}

impl Superframe {
    fn expected(&self, sps: f64) -> f64 {
        self.sync_at + (self.next * SLOT_PERIOD_DIBITS) as f64 * sps
    }
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
    superframes: Vec<Superframe>,
    /// The colour code last read on each slot (no slot, 1, 2).
    color_codes: [Option<u8>; 3],
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
                .superframes
                .iter()
                .enumerate()
                .map(|(k, s)| (k, s.expected(sps)))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((k, at)) = next
                && sync_at.is_none_or(|s| at < s - SAME_BURST_SYMBOLS * sps)
            {
                if !self.covers(at, sps) {
                    self.searched = resume;
                    break;
                }
                out.extend(self.expected_burst(k, at, sps));
                continue;
            }
            let Some((i, origin, kind)) = sync else {
                self.searched = resume;
                break;
            };
            let at = self.buf[i].at;
            if !self.covers(at, sps) {
                self.searched = resume;
                break;
            }
            // A sync where a burst was expected ends that superframe.
            self.superframes
                .retain(|s| (s.expected(sps) - at).abs() > SAME_BURST_SYMBOLS * sps);
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
                    self.superframes.clear();
                    (bits, cach, kind) = (turned, turned_cach, Kind::Data);
                } else if kind == Kind::Data {
                    // Unknown polarity and codes that do not check: not read.
                    return None;
                }
            }
        }
        let slot = (origin == Origin::Repeater).then(|| (cach_tact(&cach).0 >> 2 & 1) + 1);
        let at = centre - LEFT_DIBITS as f64 * sps;
        self.superframes.retain(|s| s.slot != slot);
        match kind {
            Kind::Voice => {
                self.superframes.push(Superframe {
                    slot,
                    origin,
                    sync_at: centre,
                    next: 1,
                    fragments: Vec::new(),
                });
                let (voice, errors) = voice_frames(&bits);
                Some(DmrBurst {
                    at,
                    slot,
                    origin,
                    color_code: None,
                    content: DmrContent::Voice {
                        first: true,
                        lc: None,
                        privacy: false,
                        voice,
                        errors,
                    },
                })
            }
            Kind::Data => {
                let (data, _) = GOLAY20.decode(slot_type_word(&bits))?;
                let (cc, dt) = ((data >> 4) as u8, (data & 0xF) as u8);
                self.color_codes[slot_key(slot)] = Some(cc);
                let lc = |mask| {
                    bptc_decode(&info_bits(&bits))
                        .and_then(|(info, _)| lc_decode(&info, mask))
                        .map(|(lc, _)| lc)
                };
                let content = match dt {
                    DT_VOICE_LC_HEADER => DmrContent::VoiceHeader(lc(MASK_VOICE_LC_HEADER)),
                    DT_TERMINATOR_LC => DmrContent::Terminator(lc(MASK_TERMINATOR_LC)),
                    DT_PI_HEADER => DmrContent::PrivacyHeader,
                    DT_CSBK => DmrContent::Control,
                    DT_IDLE => DmrContent::Idle,
                    other => DmrContent::Other(other),
                };
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

    /// Superframe `k`'s next burst (B to F), its centre field at `centre`; `None` when its
    /// EMB does not decode or names another colour code.
    fn expected_burst(&mut self, k: usize, centre: f64, sps: f64) -> Option<DmrBurst> {
        let (bits, _) = self.read(centre, sps);
        let sf = &mut self.superframes[k];
        sf.next += 1;
        let (slot, origin) = (sf.slot, sf.origin);
        let known = self.color_codes[slot_key(slot)];
        let emb = QR16
            .decode(emb_word_of(&bits))
            .filter(|&(d, _)| known.is_none_or(|cc| u32::from(cc) == d >> 3));
        let mut lc = None;
        let mut lc_errors = 0;
        if let Some((data, _)) = emb {
            let embedded = bits[HALF_BITS + EMB_HALF_BITS..][..EMBEDDED_BITS].to_vec();
            match (data & 3) as u8 {
                LCSS_FIRST => sf.fragments = vec![embedded],
                LCSS_CONTINUATION if !sf.fragments.is_empty() => sf.fragments.push(embedded),
                LCSS_LAST if !sf.fragments.is_empty() => {
                    sf.fragments.push(embedded);
                    if let Some((b, e)) = embedded_decode(&sf.fragments) {
                        lc = parse_lc(&b);
                        lc_errors = e;
                    }
                    sf.fragments.clear();
                }
                _ => sf.fragments.clear(),
            }
        } else {
            sf.fragments.clear();
        }
        if sf.next >= VOICE_BURSTS {
            self.superframes.remove(k);
        }
        let (data, emb_errors) = emb?;
        let cc = (data >> 3) as u8;
        self.color_codes[slot_key(slot)] = Some(cc);
        let (voice, ambe_errors) = voice_frames(&bits);
        Some(DmrBurst {
            at: centre - LEFT_DIBITS as f64 * sps,
            slot,
            origin,
            color_code: Some(cc),
            content: DmrContent::Voice {
                first: false,
                lc,
                privacy: data >> 2 & 1 == 1,
                voice,
                errors: emb_errors + lc_errors + ambe_errors,
            },
        })
    }

    /// Drops the dibits nothing can reach any more: those before the CACH of a burst whose
    /// sync starts at `searched`, or of the next expected burst.
    fn trim(&mut self, sps: f64) {
        let back = (LEFT_DIBITS + CACH_DIBITS + 2) as f64 * sps;
        let search_at = self
            .buf
            .get(self.searched)
            .or(self.buf.last())
            .map_or(f64::INFINITY, |d| d.at);
        let needed = self
            .superframes
            .iter()
            .map(|s| s.expected(sps))
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

/// One slot of [`encode_repeater`]: a call of `superframes` with link control `lc`, or,
/// with `None`, Idle bursts for as long as such a call would last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotPlan {
    pub lc: Option<FullLc>,
    pub superframes: u32,
}

/// A repeater's continuous downlink with CACH: both slots, each a call or idle. A call is
/// a voice LC header, `superframes` voice superframes of a fixed voice payload, a
/// terminator; a slot that finishes first idles until the other does. The CACHs carry the
/// Null short LC. Dibits.
pub fn encode_repeater(color_code: u8, slots: [SlotPlan; 2]) -> Vec<u8> {
    let idle = idle_burst(color_code);
    let bursts = slots.map(|plan| match plan.lc {
        Some(lc) => call_bursts(color_code, &lc, plan.superframes, Origin::Repeater),
        None => vec![idle; call_bursts_len(plan.superframes)],
    });
    let frames = bursts.iter().map(Vec::len).max().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * SLOT_PERIOD_DIBITS);
    for f in 0..frames {
        for (s, list) in bursts.iter().enumerate() {
            let busy = slots[s].lc.is_some() && f < list.len();
            let lcss = SHORT_LC_LCSS[(2 * f + s) % SHORT_LC_LCSS.len()];
            out.extend(dibits_of(&cach_bits(busy, s as u8, lcss)));
            out.extend(dibits_of(list.get(f).unwrap_or(&idle)));
        }
    }
    out
}

/// A handheld's bursts: one slot in two, silence (no symbols) between. The call is as
/// [`encode_repeater`]'s, with the handheld's syncs and no CACH; each burst is followed by
/// the rest of its 60 ms as `None`.
pub fn encode_handheld(color_code: u8, lc: FullLc, superframes: u32) -> Vec<Option<u8>> {
    let mut out = Vec::new();
    for burst in call_bursts(color_code, &lc, superframes, Origin::Handheld) {
        out.extend(dibits_of(&burst).into_iter().map(Some));
        out.extend(std::iter::repeat_n(None, SLOT_PERIOD_DIBITS - BURST_DIBITS));
    }
    out
}

fn call_bursts_len(superframes: u32) -> usize {
    2 + VOICE_BURSTS * superframes as usize
}

/// A call's bursts on one slot: the voice LC header, `superframes` × bursts A to F, the
/// terminator with link control.
fn call_bursts(cc: u8, lc: &FullLc, superframes: u32, origin: Origin) -> Vec<[u8; BURST_BITS]> {
    let data = |dt, mask| {
        data_burst(
            sync_word(origin, Kind::Data),
            slot_type(cc, dt),
            &bptc_encode(&lc_info(lc, mask)),
        )
    };
    let vs = voice_socket(&[ambe_encode(&VOICE_PAYLOAD); 3]);
    let fragments = embedded_encode(&lc_bytes(lc));
    let emb = |lcss| emb_word(cc, lc.encrypted, lcss);
    let mut out = Vec::with_capacity(call_bursts_len(superframes));
    out.push(data(DT_VOICE_LC_HEADER, MASK_VOICE_LC_HEADER));
    for _ in 0..superframes {
        out.push(voice_burst(
            &vs,
            &sync_centre(sync_word(origin, Kind::Voice)),
        ));
        for (fragment, lcss) in fragments.iter().zip(EMBEDDED_LCSS) {
            out.push(voice_burst(&vs, &emb_centre(emb(lcss), fragment)));
        }
        // Burst F: the Null embedded message.
        out.push(voice_burst(
            &vs,
            &emb_centre(emb(LCSS_SINGLE), &[0; EMBEDDED_BITS]),
        ));
    }
    out.push(data(DT_TERMINATOR_LC, MASK_TERMINATOR_LC));
    out
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

    /// The bursts of `iq`, fed in uneven pieces as a stage would, the demodulator turned
    /// when the framer says the polarity changed, flushed at the end.
    fn receive(iq: &[Complex<f32>], framer: &mut DmrFramer) -> Vec<DmrBurst> {
        let mut demod = Fsk4Demod::new(
            48_000,
            Fsk4Params {
                symbol_rate_baud: DMR_BAUD,
                deviation_hz: DMR_DEVIATION_HZ,
                shape: Shape::Rrc {
                    alpha: DMR_RRC_ALPHA,
                },
            },
        )
        .expect("rate");
        let mut turned = false;
        let mut bursts = Vec::new();
        for chunk in iq.chunks(777) {
            bursts.extend(framer.push(&demod.feed(chunk)));
            if let Some(inverted) = framer.inverted()
                && inverted != turned
            {
                demod.invert(inverted);
                turned = inverted;
            }
        }
        bursts.extend(framer.push(&demod.flush()));
        bursts
    }

    /// Samples from one burst of a slot to the next (60 ms at 48 kHz).
    const SLOT_PERIOD: f64 = 2880.0;

    /// `bursts` are a call of `superframes` with link control `lc` on slot `slot`: the
    /// header, the voice bursts (the sync on every sixth, the embedded link control on
    /// every burst E), the terminator, each a slot period after the last.
    fn is_a_call(bursts: &[&DmrBurst], lc: FullLc, superframes: usize, slot: Option<u8>) {
        assert_eq!(bursts.len(), 2 + 6 * superframes, "{bursts:#?}");
        assert!(bursts.iter().all(|b| b.slot == slot));
        assert_eq!(bursts[0].content, DmrContent::VoiceHeader(Some(lc)));
        assert_eq!(bursts[0].color_code, Some(CC));
        for (i, b) in bursts[1..=6 * superframes].iter().enumerate() {
            let DmrContent::Voice {
                first,
                lc: embedded,
                privacy,
                voice,
                ..
            } = &b.content
            else {
                panic!("burst {i}: {b:?}");
            };
            assert_eq!(*first, i % 6 == 0, "burst {i}");
            assert_eq!(*embedded, (i % 6 == 4).then_some(lc), "burst {i}");
            assert_eq!(*privacy, lc.encrypted, "burst {i}");
            assert_eq!(b.color_code, (i % 6 != 0).then_some(CC), "burst {i}");
            assert!(
                voice.iter().all(|f| ambe_decode(f).0 == [0; 4]),
                "burst {i}"
            );
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
        let call = encode_repeater(
            CC,
            [
                SlotPlan {
                    lc: Some(group()),
                    superframes: 3,
                },
                SlotPlan {
                    lc: None,
                    superframes: 3,
                },
            ],
        );
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
            for b in &one[1..19] {
                let DmrContent::Voice { errors, .. } = b.content else {
                    unreachable!()
                };
                assert_eq!(errors, 0, "invert {invert}: {b:?}");
            }
            assert_eq!(two.len(), 20, "invert {invert}");
            assert!(two.iter().all(|b| b.slot == Some(2)
                && b.content == DmrContent::Idle
                && b.color_code == Some(CC)));
        }
    }

    #[test]
    fn two_calls_are_told_apart_by_slot() {
        let other = FullLc {
            src: SRC_2,
            dst: DST_2,
            ..group()
        };
        let call = encode_repeater(
            CC,
            [
                SlotPlan {
                    lc: Some(group()),
                    superframes: 2,
                },
                SlotPlan {
                    lc: Some(other),
                    superframes: 2,
                },
            ],
        );
        let bursts = receive(&on_air(&call, false), &mut DmrFramer::new());
        for (slot, lc) in [(1, group()), (2, other)] {
            let mine: Vec<&DmrBurst> = bursts.iter().filter(|b| b.slot == Some(slot)).collect();
            is_a_call(&mine, lc, 2, Some(slot));
        }
        assert_eq!(bursts.len(), 2 * 14);
    }

    /// `bursts` from a handheld's call of `superframes` with link control `lc`, each in its
    /// 60 ms: every one but bursts F, which may be missing (below), and each as
    /// [`is_a_call`] has it.
    ///
    /// Burst F carries the Null embedded message and the zero voice payload, so 100 of its
    /// 132 symbols sit on one level, and the 4FSK front end does not acquire it as a burst
    /// on its own (a burst B, 92 of 132 on one level, it does): reported to the front
    /// end's owner, not worked around here.
    fn is_a_handheld_call(bursts: &[DmrBurst], lc: FullLc, superframes: usize) {
        assert!(
            bursts
                .iter()
                .all(|b| b.origin == Origin::Handheld && b.slot.is_none())
        );
        let total = 2 + 6 * superframes;
        let mut seen = vec![false; total];
        for b in bursts {
            let k = (b.at / SLOT_PERIOD).round() as usize;
            assert!(k < total && !seen[k], "burst at {}", b.at);
            assert!(
                (b.at - k as f64 * SLOT_PERIOD).abs() < 10.0,
                "burst at {}",
                b.at
            );
            seen[k] = true;
            let want = match k {
                0 => DmrContent::VoiceHeader(Some(lc)),
                k if k == total - 1 => DmrContent::Terminator(Some(lc)),
                k => {
                    let letter = (k - 1) % 6;
                    let DmrContent::Voice { voice, errors, .. } = &b.content else {
                        panic!("burst {k}: {b:?}");
                    };
                    assert!(
                        voice.iter().all(|f| ambe_decode(f).0 == [0; 4]),
                        "burst {k}"
                    );
                    assert_eq!(b.color_code, (letter != 0).then_some(CC), "burst {k}");
                    DmrContent::Voice {
                        first: letter == 0,
                        lc: (letter == 4).then_some(lc),
                        privacy: letter != 0 && lc.encrypted,
                        voice: voice.clone(),
                        errors: *errors,
                    }
                }
            };
            assert_eq!(b.content, want, "burst {k}");
        }
        let missing: Vec<usize> = (0..total).filter(|&k| !seen[k]).collect();
        assert!(
            missing.iter().all(|k| k % 6 == 0 && *k > 0),
            "missing {missing:?}"
        );
    }

    #[test]
    fn a_handheld_call_decodes_without_a_cach() {
        let call = encode_handheld(CC, group(), 3);
        // A burst every 60 ms, 27.5 ms on.
        assert_eq!(call.len(), 20 * 288);
        assert_eq!(call.iter().filter(|d| d.is_some()).count(), 20 * 132);
        let bursts = receive(&on_air_gated(&call), &mut DmrFramer::new());
        is_a_handheld_call(&bursts, group(), 3);
        // An encrypted call says so in its link control and in every EMB.
        let lc = FullLc {
            encrypted: true,
            ..group()
        };
        let bursts = receive(
            &on_air_gated(&encode_handheld(CC, lc, 1)),
            &mut DmrFramer::new(),
        );
        is_a_handheld_call(&bursts, lc, 1);
    }

    #[test]
    fn a_mid_call_join_has_the_lc_within_a_superframe() {
        let call = encode_repeater(
            CC,
            [
                SlotPlan {
                    lc: Some(group()),
                    superframes: 3,
                },
                SlotPlan {
                    lc: None,
                    superframes: 3,
                },
            ],
        );
        let iq = on_air(&call, false);
        // Join inside the second superframe's burst C, past its burst B.
        let join = (1 + 6 + 2) * SLOT_PERIOD as usize + 1300;
        let bursts = receive(&iq[join..], &mut DmrFramer::new());
        let voice = |b: &&DmrBurst| matches!(b.content, DmrContent::Voice { .. });
        let sync = bursts
            .iter()
            .filter(voice)
            .find(|b| matches!(b.content, DmrContent::Voice { first: true, .. }))
            .expect("a voice sync");
        let with_lc = bursts
            .iter()
            .find(|b| matches!(b.content, DmrContent::Voice { lc: Some(_), .. }))
            .expect("the embedded link control");
        let DmrContent::Voice { lc, .. } = with_lc.content else {
            unreachable!()
        };
        assert_eq!(lc, Some(group()));
        let superframe = DMR_SUPERFRAME_MS as f64 * 48.0;
        assert!(
            with_lc.at - sync.at < superframe,
            "{} {}",
            sync.at,
            with_lc.at
        );
        assert!(with_lc.at < 2.0 * superframe, "{}", with_lc.at);
        assert_eq!(
            bursts.last().map(|b| &b.content),
            Some(&DmrContent::Idle),
            "the last burst is slot 2's"
        );
        assert!(
            bursts
                .iter()
                .any(|b| b.content == DmrContent::Terminator(Some(group())))
        );
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
