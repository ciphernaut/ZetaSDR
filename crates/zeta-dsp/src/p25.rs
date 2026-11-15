//! P25 phase 1 (the FDMA common air interface): its codes, its data units, a framer that
//! turns the 4-level FSK front end's soft dibits ([`crate::fsk4`]) into units, and an
//! encoder that builds a call or a control channel for the `p25` synthesiser family.
//!
//! **Sources.** TIA-102.BAAA (the common air interface) and TIA-102.BABA (the IMBE vocoder)
//! are not freely published and were not to hand, so no clause number is cited and none
//! could be confirmed against them. Some values below come from a published description
//! and were also compared, value by value, with open-source decoders used as oracles;
//! others have no published description to hand and are taken from where the named
//! decoders agree. Each list says which. The code and its structure are this module's
//! own; no decoder's functions were copied, and the interleave, the mask and the Hamming
//! (15,11) check bits are written as derivations, not tables.
//! - **Published description:** Daniels Electronics, *TG-001 P25 Radio Systems* training
//!   guide, revision 2-0-0 (January 2007), chapter 4 "Anatomy of the Common Air
//!   Interface" and chapters 1 and 3. It gives the frame sync `$5575F5FF77FF`; the network
//!   identifier (NID: 12-bit NAC, 4-bit DUID, a (63,16,23) primitive BCH code plus a 64th
//!   parity bit, `P` = 1 for the two LDUs and 0 for the other units); the DUIDs; a status
//!   symbol for every 70 bits and the four status values; each data unit's length and
//!   layout (HDU 792 bits, LDU1 and LDU2 1728, TDU 144, TDU with link control 432) with the
//!   parameters of every code; the link control formats `$00` (group) and `$03` (unit to
//!   unit) and the emergency bit; the encryption sync (MI 72, ALGID 8, KID 16 bits),
//!   ALGID `$80` for clear speech and `$84` for AES; the dibit-to-deviation mapping; the
//!   order of an LDU's blocks. The sync, the NID and its parity bits, the status symbols'
//!   places, the units' lengths, the fields' order and the LDU's blocks were also compared
//!   with OP25 (and the parity bits with DSD); the ALGIDs and the mapping were not.
//! - **No published description to hand; taken from where the decoders below agree:**
//!   the field GF(2^6) with `x^6 + x + 1`; the BCH generator as the narrow-sense primitive
//!   code of designed distance 23 in that field (`BCH_GENERATOR`, which a test re-derives
//!   from that definition); the NID parity bit of DUIDs no unit is defined for (0); the
//!   Reed-Solomon generators' roots `α^1` to `α^(n-k)`, systematic, data first; the Golay
//!   generator `0xC75` and its bit order (data in the top bits, the extended code's parity
//!   bit last); the Hamming (15,11) and (10,6,3) check bits; the IMBE pseudo-random mask
//!   and the words it covers; the 8 × 23 frame's orientation (which row and column holds
//!   each bit); the IMBE interleave; every bit position inside an LDU.
//! - **The decoders** (compared entry by entry by hand; none is in this tree):
//!   OP25 (`gr-op25_repeater`, GPL-3): the BCH generator and the NID encoder's rows
//!   (parity bit included), the Golay (23,12) and (24,12) tables, the Hamming (15,11) and
//!   (10,6,3) tables, the three Reed-Solomon generator matrices, the mask generator, and
//!   every bit position of an LDU (voice, link control, low-speed data). DSD (ISC
//!   licence): the IMBE interleave schedule (`p25p1_const.h`), the Hamming (10,6,3)
//!   generator matrix and the NID parity table. mbelib-neo 2.6.11 (GPL-2.0+, the library
//!   `zeta-mbe` links): the 8 × 23 frame it takes, its Golay and Hamming tables and bit
//!   order, and the mask and where it lies. The Hamming (10,6,3) check bits are the one
//!   table here taken as data (`HAMMING10_CHECKS`, six values, as OP25 and DSD both
//!   give them); the Golay (24,12) code also matches the matrix M17's specification
//!   prints (a test compares them).
//!
//! **The signal.** 4800 dibits/s, C4FM at ±1800 and ±600 Hz ([`crate::fsk4`]). Each data
//! unit starts with the 24-dibit frame sync and the 32-dibit NID. From the sync's first
//! dibit, every 36th dibit (index 35, 71, …) is a status symbol, removed before anything is
//! decoded; the counts below are the unit's information bits, status symbols taken out.
//! - **NID**: NAC and DUID (16 bits), the 47 check bits, the parity bit. `nid_decode`
//!   corrects up to 11 errors over all 64 bits (Berlekamp–Massey, then the parity bit
//!   checked against the DUID) and refuses more.
//! - **HDU** (`0x0`): MI, MFID, ALGID, KID, TGID (120 bits, 20 hexbits) → Reed-Solomon
//!   (36,20,17) → each hexbit a shortened Golay (18,6,8) word → 648 bits, then 10 null bits.
//! - **LDU1** (`0x5`) and **LDU2** (`0xA`): nine IMBE frames of 144 bits, 240 bits of
//!   link control (LDU1) or encryption sync (LDU2) in six blocks of 40 after voice frames 2
//!   to 7, and 32 bits of low-speed data after frame 8. Link control: 72 bits (12 hexbits)
//!   → Reed-Solomon (24,12,13); encryption sync: 96 bits (16 hexbits) → Reed-Solomon
//!   (24,16,9); either way each of the 24 hexbits a Hamming (10,6,3) word. Low-speed data
//!   is two (16,8,5) words, sent here as zeros (the code is linear, so zero data codes to
//!   zeros) and not read.
//! - **TDU** (`0x3`): 28 null bits. **TDU with link control** (`0xF`): the link control
//!   through Reed-Solomon (24,12,13), its hexbits in pairs as 12 Golay (24,12,8) words,
//!   then 20 null bits.
//! - **TSDU** (`0x7`) and **PDU** (`0xC`) are reported by their NID and not parsed.
//!
//! **An IMBE frame** (88 bits as vectors `u0`–`u7` of 12, 12, 12, 12, 11, 11, 11 and 7
//! bits) is four Golay (23,12) words, three Hamming (15,11) words and `u7` bare: 144 bits.
//! Words 1 to 6 are masked by a pseudo-random sequence seeded from `u0`:
//! `p(0) = 16·u0`, `p(n) = (173·p(n−1) + 13849) mod 65536`, mask bit `n` = `p(n) ≥ 32768`,
//! over each word from its top bit down. [`ImbeSoft`] holds it as `zeta-mbe` (and the
//! library) takes it: 8 rows of 23 cells, word `i` in row `i` with its top bit in the
//! row's highest used column (22 for the Golay rows, 14 for the Hamming rows, 6 for `u7`),
//! the unused cells 0, the mask still on. On the air the 144 bits, in word order (`c0`
//! first, each from its top bit), go row by row into 6 rows of 24 and are sent column by
//! column, two rows to a dibit, the dibit's two bits swapped in odd columns; this
//! reproduces DSD's interleave schedule entry for entry and OP25's voice bit positions.

use std::sync::OnceLock;

use crate::fsk4::{SoftDibit, find_sync, sync_dibits};

/// P25 phase 1's symbol rate, baud.
pub const P25_BAUD: u32 = 4800;
/// The outer symbols' deviation, Hz (the inner ones a third of it).
pub const P25_DEVIATION_HZ: f64 = 1800.0;
/// The 48-bit frame sync, first bit first from bit 47.
pub const P25_SYNC: u64 = 0x5575_F5FF_77FF;
/// The algorithm ID of clear (unencrypted) speech.
pub const P25_ALGID_CLEAR: u8 = 0x80;
/// The algorithm ID of AES, the one the synthesiser's encrypted calls name.
pub const P25_ALGID_AES: u8 = 0x84;
/// Bytes of one IMBE frame as the vocoder takes it: 8 rows of 23, one per bit.
pub const IMBE_FEED_BYTES: usize = 184;
/// A voice superframe (LDU1 then LDU2), ms.
pub const P25_SUPERFRAME_MS: u64 = 360;
/// Dibits in an LDU, status symbols included (180 ms).
pub const LDU_DIBITS: usize = 864;
/// Dibits in a trunking signalling unit of three blocks, status symbols included (75 ms).
pub const TSDU_DIBITS: usize = 360;

const DUID_HDU: u8 = 0x0;
const DUID_TDU: u8 = 0x3;
const DUID_LDU1: u8 = 0x5;
const DUID_TSDU: u8 = 0x7;
const DUID_LDU2: u8 = 0xA;
const DUID_PDU: u8 = 0xC;
const DUID_TDULC: u8 = 0xF;

/// Dibits in the other units, status symbols included.
const HDU_DIBITS: usize = 396;
const TDU_DIBITS: usize = 72;
const TDULC_DIBITS: usize = 216;
/// One status symbol in every 36 dibits from the sync's first, the last of each 36.
const STATUS_PERIOD: usize = 36;
/// The status symbol of a radio talking direct ("unknown, use for talk-around").
const STATUS_TALKAROUND: u8 = 0b00;
/// The status symbol of a site's outbound channel with nothing inbound ("inbound
/// channel is idle").
const STATUS_IDLE: u8 = 0b11;

const SYNC_DIBITS: usize = 24;
const SYNC_BITS: usize = 48;
/// Frame sync and NID: the unit's first 56 information dibits, 57 with the status symbol
/// inside the NID.
const HEADER_BITS: usize = 112;
const HEADER_RAW_DIBITS: usize = 57;
/// Dibits of the sync that may be wrong; the NID's code then rejects a false match.
const SYNC_MAX_ERRORS: usize = 4;

/// The BCH (63,16,23) generator, `x^47 + …` (octal 6331 1413 6723 5453).
const BCH_GENERATOR: u64 = 0o6331_1413_6723_5453;
const BCH_CHECK_BITS: u32 = 47;
const BCH_BITS: usize = 63;
/// Errors the NID code corrects.
const BCH_T: usize = 11;

/// GF(2^6)'s primitive polynomial, `x^6 + x + 1`.
const GF_POLY: u16 = 0x43;
const GF_ORDER: usize = 63;

/// The Golay (23,12) generator, `x^11 + x^10 + x^6 + x^5 + x^4 + x^2 + 1`.
const GOLAY23_POLY: u32 = 0xC75;
const GOLAY_DATA_BITS: u32 = 12;
const GOLAY23_CHECK_BITS: u32 = 11;
/// The Hamming (10,6,3) check bits of each data bit, the top one first.
const HAMMING10_CHECKS: [u8; 6] = [0b1110, 0b1101, 0b1011, 0b0111, 0b0011, 0b1100];
const HAMMING_CHECK_BITS: u32 = 4;

/// Bits of `u0`–`u7`.
const IMBE_VECTOR_BITS: [u32; 8] = [12, 12, 12, 12, 11, 11, 11, 7];
/// Bits of the coded words `c0`–`c7`, one per row of [`ImbeSoft`].
const IMBE_WORD_BITS: [usize; 8] = [23, 23, 23, 23, 15, 15, 15, 7];
const IMBE_COLS: usize = 23;
const IMBE_FRAME_BITS: usize = 144;
/// The interleave's array: 6 rows of 24 bits.
const IMBE_INTERLEAVE_COLS: usize = 24;
const IMBE_MASK_SEED: u32 = 16;
const IMBE_MASK_MUL: u32 = 173;
const IMBE_MASK_ADD: u32 = 13849;
const IMBE_MASK_TOP: u32 = 0x8000;
/// The voice payload of a synthesised call: zeros. A silence frame is widely quoted from
/// captures, but no public source that states the standard's value was found, so it is
/// not used. Zeros are a valid frame of the lowest gain.
const VOICE_PAYLOAD: [u16; 8] = [0; 8];

/// Where each voice frame starts among an LDU's information bits.
const LDU_VOICE_AT: [usize; 9] = [112, 256, 440, 624, 808, 992, 1176, 1360, 1536];
/// Where each 40-bit block of link control or encryption sync starts.
const LDU_LC_AT: [usize; 6] = [400, 584, 768, 952, 1136, 1320];
const LDU_LC_BLOCK_BITS: usize = 40;
/// The 24 Hamming words of link control or encryption sync, 10 bits each.
const LDU_LC_BITS: usize = 240;
const LDU_INFO_BITS: usize = 1680;
const HDU_INFO_BITS: usize = 770;
const TDULC_INFO_BITS: usize = 420;
/// The header code word: 36 Golay (18,6,8) words.
const HDU_CODED_BITS: usize = 648;
/// The terminator's link control: 12 Golay (24,12,8) words.
const TDULC_CODED_BITS: usize = 288;
const TSDU_INFO_BITS: usize = 700;

/// Link control opcodes: group voice channel user, unit-to-unit voice channel user.
const LCO_GROUP: u8 = 0x00;
const LCO_PRIVATE: u8 = 0x03;
/// The link control format's protected bit: the rest of the word is encrypted.
const LCF_PROTECTED: u8 = 0x80;
const LCO_MASK: u8 = 0x3F;
/// Service options: emergency, protected (encrypted) speech.
const SVC_EMERGENCY: u8 = 0x80;
const SVC_PROTECTED: u8 = 0x40;
const LC_HEXBITS: usize = 12;
const ES_HEXBITS: usize = 16;
const HEADER_HEXBITS: usize = 20;
const HEXBIT_MASK: u8 = 0x3F;

/// Whether a group or a unit-to-unit call (shared with the other voice stages).
pub use crate::voice_call::Call;

/// A voice call's link control: who is calling whom. `dst` is the talkgroup (16 bits)
/// of a group call or the called radio (24 bits) of a private one; `src` the calling
/// radio (24 bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkControl {
    pub call: Call,
    pub dst: u32,
    pub src: u32,
    pub mfid: u8,
    pub emergency: bool,
}

/// The encryption sync's algorithm and key IDs ([`P25_ALGID_CLEAR`] for clear speech).
/// The message indicator is not kept: nothing here decrypts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncSync {
    pub algid: u8,
    pub kid: u16,
}

/// One IMBE frame as `zeta-mbe imbe7200x4400` reads it: mbelib's 8 rows of 23, one soft
/// byte per bit (0 a certain zero, 255 a certain one), the mask still on, the unused
/// cells 0.
pub type ImbeSoft = [u8; IMBE_FEED_BYTES];

/// A decoded data unit. A field that could not be corrected is `None`; `errors` counts
/// the bits the unit's codes corrected (NID, Hamming, Reed-Solomon symbols' bits and the
/// IMBE frames' Golay and Hamming words).
#[derive(Clone, Debug, PartialEq)]
pub enum P25Unit {
    /// The header: its encryption sync and talkgroup.
    Header {
        enc: Option<EncSync>,
        dst: Option<u32>,
    },
    Ldu1 {
        lc: Option<LinkControl>,
        voice: Box<[ImbeSoft; 9]>,
        errors: u32,
    },
    Ldu2 {
        enc: Option<EncSync>,
        voice: Box<[ImbeSoft; 9]>,
        errors: u32,
    },
    /// A terminator; with its link control when it carries one.
    Terminator { lc: Option<LinkControl> },
    /// A trunking signalling unit (not parsed).
    Trunking,
    /// A packet data unit (not parsed).
    Packet,
    /// A DUID no data unit is defined for.
    Other(u8),
}

/// Turns soft dibits into data units: the sync search in both polarities, the NID, the
/// status symbols out, the unit's codes. State carries across [`push`](Self::push) calls.
///
/// Polarity: once a sync tells it, [`inverted`](Self::inverted) says whether the
/// discriminator is upside down. The caller passes that to
/// [`crate::fsk4::Fsk4Demod::invert`] after each push, so the framer takes every later
/// push's dibits as already turned, and turns those it holds itself.
#[derive(Default)]
pub struct P25Framer {
    /// Dibits not yet consumed, in the signal's polarity as far as known.
    buf: Vec<SoftDibit>,
    inverted: Option<bool>,
}

impl P25Framer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Units found, each with the sample position of its sync and its NAC.
    pub fn push(&mut self, dibits: &[SoftDibit]) -> Vec<(f64, u16, P25Unit)> {
        // The caller turned the demodulator after the last push, so these arrive turned.
        let applied = self.inverted.unwrap_or(false);
        self.buf.extend_from_slice(dibits);
        let sync = sync_dibits(P25_SYNC, SYNC_BITS as u32);
        let mut out = Vec::new();
        let mut pos = 0;
        loop {
            let Some((i, flipped)) = find_sync(&self.buf[pos..], &sync, SYNC_MAX_ERRORS) else {
                // Keep what could still be the start of a sync.
                pos = pos.max(self.buf.len().saturating_sub(SYNC_DIBITS - 1));
                break;
            };
            let start = pos + i;
            if self.buf.len() < start + HEADER_RAW_DIBITS {
                pos = start;
                break;
            }
            let mut head: Vec<SoftDibit> = self.buf[start..start + HEADER_RAW_DIBITS].to_vec();
            if flipped {
                head.iter_mut().for_each(flip);
            }
            let head = info_bits(&head);
            let word = head[SYNC_BITS..HEADER_BITS]
                .iter()
                .fold(0u64, |w, &b| w << 1 | u64::from(b >= 128));
            let Some((nac, duid, nid_errors)) = nid_decode(word) else {
                pos = start + 1;
                continue;
            };
            if flipped {
                self.buf[start..].iter_mut().for_each(flip);
                self.inverted = Some(!applied);
            } else if self.inverted.is_none() {
                self.inverted = Some(applied);
            }
            let at = self.buf[start].at;
            let Some(len) = unit_dibits(duid) else {
                let unit = match duid {
                    DUID_TSDU => P25Unit::Trunking,
                    DUID_PDU => P25Unit::Packet,
                    d => P25Unit::Other(d),
                };
                out.push((at, nac, unit));
                pos = start + HEADER_RAW_DIBITS;
                continue;
            };
            if self.buf.len() < start + len {
                pos = start;
                break;
            }
            let bits = info_bits(&self.buf[start..start + len]);
            out.push((at, nac, decode_unit(duid, &bits, nid_errors)));
            pos = start + len;
        }
        self.buf.drain(..pos);
        out
    }

    /// Whether the discriminator is inverted; known after the first sync.
    pub fn inverted(&self) -> Option<bool> {
        self.inverted
    }
}

/// `d` in the other polarity: each level negated, so the first bit flips.
fn flip(d: &mut SoftDibit) {
    d.dibit ^= 0b10;
    d.soft[0] = 255 - d.soft[0];
}

/// The unit's length in dibits, status symbols included, for the units read whole.
fn unit_dibits(duid: u8) -> Option<usize> {
    match duid {
        DUID_HDU => Some(HDU_DIBITS),
        DUID_TDU => Some(TDU_DIBITS),
        DUID_LDU1 | DUID_LDU2 => Some(LDU_DIBITS),
        DUID_TDULC => Some(TDULC_DIBITS),
        _ => None,
    }
}

/// The soft bits of `dibits` (a unit from its sync's first dibit) with the status
/// symbols taken out.
fn info_bits(dibits: &[SoftDibit]) -> Vec<u8> {
    dibits
        .iter()
        .enumerate()
        .filter(|(i, _)| i % STATUS_PERIOD != STATUS_PERIOD - 1)
        .flat_map(|(_, d)| d.soft)
        .collect()
}

/// The unit `duid` from its information bits (soft, from the sync's first bit).
fn decode_unit(duid: u8, bits: &[u8], nid_errors: u32) -> P25Unit {
    let hard: Vec<u8> = bits.iter().map(|&b| u8::from(b >= 128)).collect();
    let body = &hard[HEADER_BITS..];
    match duid {
        DUID_HDU => {
            let (mut hex, _) = hdu_hexbits(&body[..HDU_CODED_BITS]);
            match RS_HEADER.decode(&mut hex) {
                Some(_) => {
                    let w = word_of(&hex[..HEADER_HEXBITS]);
                    P25Unit::Header {
                        enc: Some(EncSync {
                            algid: (w >> 32) as u8,
                            kid: (w >> 16) as u16,
                        }),
                        dst: Some(u32::from(w as u16)),
                    }
                }
                None => P25Unit::Header {
                    enc: None,
                    dst: None,
                },
            }
        }
        DUID_LDU1 | DUID_LDU2 => {
            let voice: Box<[ImbeSoft; 9]> = Box::new(std::array::from_fn(|f| {
                imbe_from_bits(&bits[LDU_VOICE_AT[f]..LDU_VOICE_AT[f] + IMBE_FRAME_BITS])
            }));
            let block: Vec<u8> = LDU_LC_AT
                .iter()
                .flat_map(|&at| hard[at..at + LDU_LC_BLOCK_BITS].iter().copied())
                .collect();
            let (mut hex, hamming) = ldu_hexbits(&block);
            let imbe: u32 = voice.iter().map(|f| imbe_decode(f).1).sum();
            let (rs, k) = if duid == DUID_LDU1 {
                (&RS_LC, LC_HEXBITS)
            } else {
                (&RS_ES, ES_HEXBITS)
            };
            let before = hex.clone();
            let fixed = rs.decode(&mut hex).map(|_| &hex[..k]);
            let rs_bits: u32 = match fixed {
                Some(_) => before
                    .iter()
                    .zip(&hex)
                    .map(|(a, b)| (a ^ b).count_ones())
                    .sum(),
                None => 0,
            };
            let errors = nid_errors + hamming + rs_bits + imbe;
            if duid == DUID_LDU1 {
                P25Unit::Ldu1 {
                    lc: fixed.and_then(parse_lc),
                    voice,
                    errors,
                }
            } else {
                P25Unit::Ldu2 {
                    enc: fixed.and_then(parse_es),
                    voice,
                    errors,
                }
            }
        }
        DUID_TDULC => {
            let (mut hex, _) = tdulc_hexbits(&body[..TDULC_CODED_BITS]);
            P25Unit::Terminator {
                lc: RS_LC
                    .decode(&mut hex)
                    .and_then(|_| parse_lc(&hex[..LC_HEXBITS])),
            }
        }
        _ => P25Unit::Terminator { lc: None },
    }
}

// ---- encoding ----

/// A clear or encrypted call as dibits: header, `superframes` × (LDU1, LDU2), terminator
/// with link control. The voice frames carry one fixed payload (`VOICE_PAYLOAD`); the
/// message indicator is zero.
pub fn encode_call(nac: u16, lc: LinkControl, enc: EncSync, superframes: u32) -> Vec<u8> {
    let protected = enc.algid != P25_ALGID_CLEAR;
    let lc_hex = RS_LC.encode(&hexbits(lc_word(&lc, protected), LC_HEXBITS));
    let es_hex = RS_ES.encode(&hexbits(es_word(&enc), ES_HEXBITS));
    let tgid = match lc.call {
        Call::Group => lc.dst as u16,
        Call::Private => 0,
    };
    let header_hex = RS_HEADER.encode(&hexbits(header_word(&enc, tgid), HEADER_HEXBITS));
    let voice = imbe_encode(&VOICE_PAYLOAD);

    let mut hdu = header_bits(nac, DUID_HDU);
    hdu.extend(hdu_bits(&header_hex));
    let mut out = unit_symbols(hdu, HDU_INFO_BITS, STATUS_TALKAROUND);
    for _ in 0..superframes {
        for (duid, hex) in [(DUID_LDU1, &lc_hex), (DUID_LDU2, &es_hex)] {
            let mut bits = header_bits(nac, duid);
            bits.resize(LDU_INFO_BITS, 0);
            for at in LDU_VOICE_AT {
                bits[at..at + IMBE_FRAME_BITS].copy_from_slice(&imbe_tx_bits(&voice));
            }
            let lc_bits = ldu_lc_bits(hex);
            for (block, at) in lc_bits.chunks(LDU_LC_BLOCK_BITS).zip(LDU_LC_AT) {
                bits[at..at + LDU_LC_BLOCK_BITS].copy_from_slice(block);
            }
            out.extend(unit_symbols(bits, LDU_INFO_BITS, STATUS_TALKAROUND));
        }
    }
    let mut tdulc = header_bits(nac, DUID_TDULC);
    tdulc.extend(tdulc_bits(&lc_hex));
    out.extend(unit_symbols(tdulc, TDULC_INFO_BITS, STATUS_TALKAROUND));
    out
}

/// A control channel: `units` trunking signalling units of three blocks with idle
/// content. The blocks are null bits, not trellis-coded signalling: nothing here parses
/// them, so they stand only for the unit's length.
pub fn encode_control(nac: u16, units: u32) -> Vec<u8> {
    (0..units)
        .flat_map(|_| unit_symbols(header_bits(nac, DUID_TSDU), TSDU_INFO_BITS, STATUS_IDLE))
        .collect()
}

/// The frame sync and NID of a unit, as bits.
fn header_bits(nac: u16, duid: u8) -> Vec<u8> {
    let mut bits = word_bits(P25_SYNC, SYNC_BITS);
    bits.extend(word_bits(nid_encode(nac, duid), HEADER_BITS - SYNC_BITS));
    bits
}

/// The low `n` bits of `word`, the top one first.
fn word_bits(word: u64, n: usize) -> Vec<u8> {
    (0..n).rev().map(|i| (word >> i & 1) as u8).collect()
}

/// A unit's dibits from its information bits (padded with null bits to `info_bits`),
/// a status symbol `status` in every 36th place.
fn unit_symbols(mut bits: Vec<u8>, info_bits: usize, status: u8) -> Vec<u8> {
    bits.resize(info_bits, 0);
    let mut out = Vec::with_capacity(info_bits / 2 * STATUS_PERIOD / (STATUS_PERIOD - 1));
    for pair in bits.chunks(2) {
        if out.len() % STATUS_PERIOD == STATUS_PERIOD - 1 {
            out.push(status);
        }
        out.push(pair[0] << 1 | pair[1]);
    }
    if out.len() % STATUS_PERIOD == STATUS_PERIOD - 1 {
        out.push(status);
    }
    out
}

// ---- fields ----

/// The 72-bit link control word: LCF, MFID, service options, then for a group call a
/// reserved octet and the 16-bit talkgroup, for a private one the 24-bit called radio,
/// then the 24-bit source.
fn lc_word(lc: &LinkControl, protected: bool) -> u128 {
    let (lco, dst) = match lc.call {
        Call::Group => (LCO_GROUP, lc.dst & 0xFFFF),
        Call::Private => (LCO_PRIVATE, lc.dst & 0xFF_FFFF),
    };
    let mut svc = 0;
    if lc.emergency {
        svc |= SVC_EMERGENCY;
    }
    if protected {
        svc |= SVC_PROTECTED;
    }
    u128::from(lco) << 64
        | u128::from(lc.mfid) << 56
        | u128::from(svc) << 48
        | u128::from(dst) << 24
        | u128::from(lc.src & 0xFF_FFFF)
}

/// The link control in 12 hexbits, when it is a voice channel user's and not encrypted.
fn parse_lc(hex: &[u8]) -> Option<LinkControl> {
    let w = word_of(hex);
    let lcf = (w >> 64) as u8;
    if lcf & LCF_PROTECTED != 0 {
        return None;
    }
    let (call, dst) = match lcf & LCO_MASK {
        LCO_GROUP => (Call::Group, (w >> 24) as u32 & 0xFFFF),
        LCO_PRIVATE => (Call::Private, (w >> 24) as u32 & 0xFF_FFFF),
        _ => return None,
    };
    Some(LinkControl {
        call,
        dst,
        src: w as u32 & 0xFF_FFFF,
        mfid: (w >> 56) as u8,
        emergency: (w >> 48) as u8 & SVC_EMERGENCY != 0,
    })
}

/// The 96-bit encryption sync: MI (zero here), ALGID, KID.
fn es_word(enc: &EncSync) -> u128 {
    u128::from(enc.algid) << 16 | u128::from(enc.kid)
}

fn parse_es(hex: &[u8]) -> Option<EncSync> {
    let w = word_of(hex);
    Some(EncSync {
        algid: (w >> 16) as u8,
        kid: w as u16,
    })
}

/// The header's 120 bits: MI (zero here), MFID (zero), ALGID, KID, TGID.
fn header_word(enc: &EncSync, tgid: u16) -> u128 {
    u128::from(enc.algid) << 32 | u128::from(enc.kid) << 16 | u128::from(tgid)
}

/// The low `6n` bits of `word` as `n` hexbits, the top one first.
fn hexbits(word: u128, n: usize) -> Vec<u8> {
    (0..n)
        .rev()
        .map(|i| (word >> (6 * i)) as u8 & HEXBIT_MASK)
        .collect()
}

fn word_of(hex: &[u8]) -> u128 {
    hex.iter().fold(0, |w, &h| w << 6 | u128::from(h))
}

/// `words` as bits, `width` to a word, each from its top bit.
fn coded_bits(words: impl Iterator<Item = u32>, width: usize) -> Vec<u8> {
    words.flat_map(|w| word_bits(u64::from(w), width)).collect()
}

/// Bits (hard, one per byte) gathered into `width`-bit words.
fn words_of(bits: &[u8], width: usize) -> impl Iterator<Item = u32> + '_ {
    bits.chunks(width)
        .map(|c| c.iter().fold(0u32, |w, &b| w << 1 | u32::from(b & 1)))
}

/// An LDU's 240 bits of link control or encryption sync from its 24 hexbits.
fn ldu_lc_bits(hex: &[u8]) -> Vec<u8> {
    coded_bits(
        hex.iter()
            .map(|&h| u32::from(hamming_encode(&HAMMING10_CHECKS, u16::from(h)))),
        10,
    )
}

/// The 24 hexbits from an LDU's 240 bits, each Hamming word corrected (one error), and
/// the bits corrected.
fn ldu_hexbits(bits: &[u8]) -> (Vec<u8>, u32) {
    let mut errors = 0;
    let hex = words_of(&bits[..LDU_LC_BITS], 10)
        .map(|w| {
            let (h, e) = hamming_decode(&HAMMING10_CHECKS, w as u16);
            errors += e;
            h as u8
        })
        .collect();
    (hex, errors)
}

/// A terminator's 288 bits of link control: hexbit pairs as Golay (24,12,8) words.
fn tdulc_bits(hex: &[u8]) -> Vec<u8> {
    coded_bits(
        hex.chunks(2)
            .map(|p| golay24_encode(u16::from(p[0]) << 6 | u16::from(p[1]))),
        24,
    )
}

/// The 24 hexbits from a terminator's 288 bits, and the bits corrected. A word with more
/// errors than the code corrects keeps its received data bits.
fn tdulc_hexbits(bits: &[u8]) -> (Vec<u8>, u32) {
    let mut errors = 0;
    let hex = words_of(bits, 24)
        .flat_map(|w| {
            let d = match golay24_decode(w) {
                Some((d, e)) => {
                    errors += e;
                    d
                }
                None => (w >> GOLAY_DATA_BITS) as u16,
            };
            [(d >> 6) as u8, d as u8 & HEXBIT_MASK]
        })
        .collect();
    (hex, errors)
}

/// A header's 648 bits: each hexbit a Golay (18,6,8) word, the (24,12,8) word of the
/// hexbit with its six leading zeros dropped.
fn hdu_bits(hex: &[u8]) -> Vec<u8> {
    coded_bits(hex.iter().map(|&h| golay24_encode(u16::from(h))), 18)
}

/// The 36 hexbits from a header's 648 bits, and the bits corrected.
fn hdu_hexbits(bits: &[u8]) -> (Vec<u8>, u32) {
    let mut errors = 0;
    let hex = words_of(bits, 18)
        .map(
            |w| match golay24_decode(w).filter(|&(d, _)| d <= u16::from(HEXBIT_MASK)) {
                Some((d, e)) => {
                    errors += e;
                    d as u8
                }
                None => (w >> GOLAY_DATA_BITS) as u8 & HEXBIT_MASK,
            },
        )
        .collect();
    (hex, errors)
}

// ---- GF(2^6) ----

const fn gf_tables() -> ([u8; GF_ORDER], [u8; 64]) {
    let mut exp = [0u8; GF_ORDER];
    let mut log = [0u8; 64];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < GF_ORDER {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x40 != 0 {
            x ^= GF_POLY;
        }
        i += 1;
    }
    (exp, log)
}

const GF: ([u8; GF_ORDER], [u8; 64]) = gf_tables();

/// `α^i`.
fn alpha(i: usize) -> u8 {
    GF.0[i % GF_ORDER]
}

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        alpha(usize::from(GF.1[usize::from(a)]) + usize::from(GF.1[usize::from(b)]))
    }
}

/// `a / b`, `b` not zero.
fn gf_div(a: u8, b: u8) -> u8 {
    if a == 0 {
        0
    } else {
        alpha(usize::from(GF.1[usize::from(a)]) + GF_ORDER - usize::from(GF.1[usize::from(b)]))
    }
}

/// `p(x)`, coefficients lowest first.
fn eval_low(p: &[u8], x: u8) -> u8 {
    p.iter().rev().fold(0, |acc, &c| gf_mul(acc, x) ^ c)
}

/// `p(x)`, coefficients highest first.
fn eval_high(p: &[u8], x: u8) -> u8 {
    p.iter().fold(0, |acc, &c| gf_mul(acc, x) ^ c)
}

/// The error locator for syndromes `s` (`s[0]` = `S1`), lowest coefficient first, by
/// Berlekamp–Massey.
fn berlekamp_massey(s: &[u8]) -> Vec<u8> {
    let mut c = vec![1u8];
    let mut b = vec![1u8];
    let mut l = 0;
    let mut m = 1;
    let mut last = 1u8;
    for n in 0..s.len() {
        let d = (1..=l.min(c.len() - 1)).fold(s[n], |d, i| d ^ gf_mul(c[i], s[n - i]));
        if d == 0 {
            m += 1;
            continue;
        }
        let coef = gf_div(d, last);
        let mut next = c.clone();
        next.resize(next.len().max(b.len() + m), 0);
        for (i, &bi) in b.iter().enumerate() {
            next[i + m] ^= gf_mul(coef, bi);
        }
        if 2 * l <= n {
            b = std::mem::replace(&mut c, next);
            l = n + 1 - l;
            last = d;
            m = 1;
        } else {
            c = next;
            m += 1;
        }
    }
    while c.len() > 1 && c.last() == Some(&0) {
        c.pop();
    }
    c
}

/// The degrees below `n` where the locator `lambda` puts an error (its roots are their
/// inverses), when it has as many as its degree.
fn error_degrees(lambda: &[u8], n: usize) -> Option<Vec<usize>> {
    let found: Vec<usize> = (0..n)
        .filter(|&p| eval_low(lambda, alpha(GF_ORDER - p % GF_ORDER)) == 0)
        .collect();
    (found.len() == lambda.len() - 1).then_some(found)
}

// ---- the NID's BCH (63,16,23) ----

/// The NID's parity bit: 1 for the two LDUs, 0 for every other DUID.
fn nid_parity(duid: u8) -> u64 {
    u64::from(duid == DUID_LDU1 || duid == DUID_LDU2)
}

/// The 64-bit NID of `nac` and `duid`: the 16 data bits (NAC first), the 47 check bits
/// (the data times `x^47` modulo [`BCH_GENERATOR`]), the parity bit.
fn nid_encode(nac: u16, duid: u8) -> u64 {
    let data = u64::from(nac & 0xFFF) << 4 | u64::from(duid & 0xF);
    let mut r = data << BCH_CHECK_BITS;
    for i in (BCH_CHECK_BITS..BCH_BITS as u32).rev() {
        if r >> i & 1 == 1 {
            r ^= BCH_GENERATOR << (i - BCH_CHECK_BITS);
        }
    }
    (data << BCH_CHECK_BITS | r) << 1 | nid_parity(duid)
}

/// NAC, DUID and the bits corrected, when `word` is within 11 bits of a NID.
fn nid_decode(word: u64) -> Option<(u16, u8, u32)> {
    let code = word >> 1;
    // Bit i is the coefficient of x^i; the syndromes are the word at α^1 to α^22.
    let s: Vec<u8> = (1..=2 * BCH_T)
        .map(|j| {
            (0..BCH_BITS)
                .filter(|i| code >> i & 1 == 1)
                .fold(0, |a, i| a ^ alpha(i * j))
        })
        .collect();
    let mut fixed = code;
    let mut errors = 0;
    if s.iter().any(|&x| x != 0) {
        let lambda = berlekamp_massey(&s);
        if lambda.len() - 1 > BCH_T {
            return None;
        }
        for p in error_degrees(&lambda, BCH_BITS)? {
            fixed ^= 1 << p;
            errors += 1;
        }
    }
    let data = (fixed >> BCH_CHECK_BITS) as u16;
    let (nac, duid) = (data >> 4, (data & 0xF) as u8);
    let want = nid_encode(nac, duid);
    if want >> 1 != fixed {
        return None;
    }
    errors += ((want ^ word) & 1) as u32;
    (errors <= BCH_T as u32).then_some((nac, duid, errors))
}

// ---- Reed-Solomon over GF(2^6) ----

/// A shortened Reed-Solomon code: `n` hexbits, the first `k` the data; generator roots
/// `α^1` to `α^(n-k)`, the first hexbit the highest power.
struct Rs {
    n: usize,
    k: usize,
}

const RS_LC: Rs = Rs { n: 24, k: 12 };
const RS_ES: Rs = Rs { n: 24, k: 16 };
const RS_HEADER: Rs = Rs { n: 36, k: 20 };

impl Rs {
    /// The generator, highest coefficient first.
    fn generator(&self) -> Vec<u8> {
        (1..=self.n - self.k).fold(vec![1u8], |g, i| {
            let mut next = g.clone();
            next.push(0);
            for (j, &c) in g.iter().enumerate() {
                next[j + 1] ^= gf_mul(c, alpha(i));
            }
            next
        })
    }

    /// `data` (`k` hexbits) and its `n - k` check hexbits.
    fn encode(&self, data: &[u8]) -> Vec<u8> {
        let g = self.generator();
        let mut w = data.to_vec();
        w.resize(self.n, 0);
        for i in 0..self.k {
            let c = w[i];
            for (j, &gj) in g.iter().enumerate().skip(1) {
                w[i + j] ^= gf_mul(gj, c);
            }
        }
        let mut out = data.to_vec();
        out.extend_from_slice(&w[self.k..]);
        out
    }

    /// Corrects `word` in place; the symbols corrected, or `None` past the code's reach.
    fn decode(&self, word: &mut [u8]) -> Option<u32> {
        let checks = self.n - self.k;
        let syndromes =
            |w: &[u8]| -> Vec<u8> { (1..=checks).map(|j| eval_high(w, alpha(j))).collect() };
        let s = syndromes(word);
        if s.iter().all(|&x| x == 0) {
            return Some(0);
        }
        let lambda = berlekamp_massey(&s);
        if lambda.len() - 1 > checks / 2 {
            return None;
        }
        let degrees = error_degrees(&lambda, self.n)?;
        // Forney: the error at X is Ω(X⁻¹) / Λ'(X⁻¹), Ω = S·Λ mod x^(n-k).
        let omega: Vec<u8> = (0..checks)
            .map(|i| (0..=i.min(lambda.len() - 1)).fold(0, |a, j| a ^ gf_mul(s[i - j], lambda[j])))
            .collect();
        let derivative: Vec<u8> = (1..lambda.len())
            .map(|i| if i % 2 == 1 { lambda[i] } else { 0 })
            .collect();
        let mut fixed = word.to_vec();
        for &p in &degrees {
            let x_inv = alpha(GF_ORDER - p % GF_ORDER);
            let den = eval_low(&derivative, x_inv);
            if den == 0 {
                return None;
            }
            fixed[self.n - 1 - p] ^= gf_div(eval_low(&omega, x_inv), den);
        }
        if syndromes(&fixed).iter().any(|&x| x != 0) {
            return None;
        }
        word.copy_from_slice(&fixed);
        Some(degrees.len() as u32)
    }
}

// ---- Golay (23,12) and the Hamming codes ----

/// The 11 check bits of `data` (12 bits): `data · x^11` modulo [`GOLAY23_POLY`].
fn golay23_check(data: u16) -> u32 {
    let mut r = u32::from(data & 0xFFF) << GOLAY23_CHECK_BITS;
    for i in (GOLAY23_CHECK_BITS..GOLAY23_CHECK_BITS + GOLAY_DATA_BITS).rev() {
        if r >> i & 1 == 1 {
            r ^= GOLAY23_POLY << (i - GOLAY23_CHECK_BITS);
        }
    }
    r
}

/// The Golay (23,12) word of `data`: the data in bits 22..11, the check bits below.
fn golay23_encode(data: u16) -> u32 {
    u32::from(data & 0xFFF) << GOLAY23_CHECK_BITS | golay23_check(data)
}

fn golay23_syndrome(word: u32) -> u32 {
    golay23_check((word >> GOLAY23_CHECK_BITS) as u16) ^ (word & 0x7FF)
}

/// Every error pattern of weight 3 or less by its syndrome: the code is perfect, so
/// that is every syndrome.
fn golay23_table() -> &'static [u32] {
    static TABLE: OnceLock<Vec<u32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = vec![0u32; 1 << GOLAY23_CHECK_BITS];
        let n = GOLAY23_CHECK_BITS + GOLAY_DATA_BITS;
        for a in 0..n {
            for b in a..n {
                for c in b..n {
                    // OR, not XOR: a repeated position must stay set, so a = b, b = c
                    // give the weight 2 patterns and a = b = c the weight 1 ones.
                    let e = (1u32 << a) | (1 << b) | (1 << c);
                    t[golay23_syndrome(e) as usize] = e;
                }
            }
        }
        t
    })
}

/// The data of a Golay (23,12) word, up to three errors corrected, and the bits
/// corrected.
fn golay23_decode(word: u32) -> (u16, u32) {
    let e = golay23_table()[golay23_syndrome(word) as usize];
    (((word ^ e) >> GOLAY23_CHECK_BITS) as u16, e.count_ones())
}

/// The Golay (24,12,8) word of `data`: its (23,12) word, then an even-parity bit.
fn golay24_encode(data: u16) -> u32 {
    let w = golay23_encode(data);
    w << 1 | (w.count_ones() & 1)
}

/// The data of a Golay (24,12,8) word with up to three errors corrected, and the bits
/// corrected; `None` for four (always detected) or more that it sees.
fn golay24_decode(word: u32) -> Option<(u16, u32)> {
    let word = word & 0xFF_FFFF;
    let (data, _) = golay23_decode(word >> 1);
    let errors = (golay24_encode(data) ^ word).count_ones();
    (errors <= 3).then_some((data, errors))
}

/// The IMBE Hamming (15,11) check bits of each data bit, the top one first: the 4-bit
/// values with two or more ones, in descending order.
const HAMMING15_CHECKS: [u8; 11] = {
    let mut out = [0u8; 11];
    let (mut v, mut i) = (15u8, 0);
    while i < 11 {
        if v.count_ones() >= 2 {
            out[i] = v;
            i += 1;
        }
        v -= 1;
    }
    out
};

/// The check bits of `data` under `checks` (one entry per data bit, the top one first).
fn hamming_checks(checks: &[u8], data: u16) -> u16 {
    let k = checks.len();
    checks
        .iter()
        .enumerate()
        .filter(|(i, _)| data >> (k - 1 - i) & 1 == 1)
        .fold(0, |a, (_, &c)| a ^ u16::from(c))
}

/// The word of `data`: the data, then its four check bits.
fn hamming_encode(checks: &[u8], data: u16) -> u16 {
    data << HAMMING_CHECK_BITS | hamming_checks(checks, data)
}

/// The data of a Hamming word with one error corrected, and the bits corrected (0 when
/// the syndrome names no single error, the data left as received).
fn hamming_decode(checks: &[u8], word: u16) -> (u16, u32) {
    let k = checks.len();
    let data = word >> HAMMING_CHECK_BITS & ((1 << k) - 1);
    let syndrome = (hamming_checks(checks, data) ^ word) & 0xF;
    if syndrome.count_ones() <= 1 {
        return (data, syndrome.count_ones());
    }
    match checks.iter().position(|&c| u16::from(c) == syndrome) {
        Some(i) => (data ^ 1 << (k - 1 - i), 1),
        None => (data, 0),
    }
}

fn hamming15_encode(data: u16) -> u16 {
    hamming_encode(&HAMMING15_CHECKS, data & 0x7FF)
}

// ---- IMBE frames ----

/// For each bit sent (in an LDU, a voice frame's 144 in order), its cell in
/// [`ImbeSoft`].
const IMBE_CELLS: [u8; IMBE_FRAME_BITS] = {
    let mut out = [0u8; IMBE_FRAME_BITS];
    let mut t = 0;
    while t < IMBE_FRAME_BITS {
        let (dibit, bit) = (t / 2, t % 2);
        let (col, pair) = (dibit / 3, dibit % 3);
        let row = 2 * pair + if col % 2 == 0 { bit } else { 1 - bit };
        // Its place in word order, c0 first, each word top bit first.
        let mut at = IMBE_INTERLEAVE_COLS * row + col;
        let mut word = 0;
        while at >= IMBE_WORD_BITS[word] {
            at -= IMBE_WORD_BITS[word];
            word += 1;
        }
        out[t] = (word * IMBE_COLS + IMBE_WORD_BITS[word] - 1 - at) as u8;
        t += 1;
    }
    out
};

/// The mask over words 1 to 6, seeded from `u0`, as words aligned with the code words.
fn imbe_mask(u0: u16) -> [u32; 8] {
    let mut p = IMBE_MASK_SEED * u32::from(u0);
    let mut mask = [0u32; 8];
    for (word, m) in mask.iter_mut().enumerate().take(7).skip(1) {
        for col in (0..IMBE_WORD_BITS[word]).rev() {
            p = (IMBE_MASK_MUL * p + IMBE_MASK_ADD) & 0xFFFF;
            if p & IMBE_MASK_TOP != 0 {
                *m |= 1 << col;
            }
        }
    }
    mask
}

/// The 88 bits `u` (`u0`–`u7`) as a coded frame, certain bits.
fn imbe_encode(u: &[u16; 8]) -> ImbeSoft {
    let mask = imbe_mask(u[0]);
    let mut frame = [0u8; IMBE_FEED_BYTES];
    for (i, &v) in u.iter().enumerate() {
        let v = v & ((1 << IMBE_VECTOR_BITS[i]) - 1);
        let word = match i {
            0..=3 => golay23_encode(v),
            4..=6 => u32::from(hamming15_encode(v)),
            _ => u32::from(v),
        } ^ mask[i];
        for col in 0..IMBE_WORD_BITS[i] {
            frame[i * IMBE_COLS + col] = 255 * (word >> col & 1) as u8;
        }
    }
    frame
}

/// `u0`–`u7` from a frame (hard decisions), and the bits its codes corrected.
fn imbe_decode(frame: &ImbeSoft) -> ([u16; 8], u32) {
    let word = |i: usize| {
        (0..IMBE_WORD_BITS[i]).fold(0u32, |w, c| {
            w | u32::from(frame[i * IMBE_COLS + c] >= 128) << c
        })
    };
    let (u0, mut errors) = golay23_decode(word(0));
    let mask = imbe_mask(u0);
    let mut u = [0u16; 8];
    u[0] = u0;
    for (i, ui) in u.iter_mut().enumerate().skip(1) {
        let w = word(i) ^ mask[i];
        let (v, e) = match i {
            1..=3 => golay23_decode(w),
            4..=6 => hamming_decode(&HAMMING15_CHECKS, w as u16),
            _ => (w as u16, 0),
        };
        *ui = v;
        errors += e;
    }
    (u, errors)
}

/// A frame's 144 bits in the order sent (hard).
fn imbe_tx_bits(frame: &ImbeSoft) -> [u8; IMBE_FRAME_BITS] {
    IMBE_CELLS.map(|c| u8::from(frame[usize::from(c)] >= 128))
}

/// A frame from its 144 soft bits in the order sent.
fn imbe_from_bits(bits: &[u8]) -> ImbeSoft {
    let mut frame = [0u8; IMBE_FEED_BYTES];
    for (&c, &b) in IMBE_CELLS.iter().zip(bits) {
        frame[usize::from(c)] = b;
    }
    frame
}

#[cfg(test)]
mod tests {
    use num_complex::Complex;

    use super::*;
    use crate::fsk4::{Fsk4Demod, Fsk4Params, Shape};
    use crate::synth::c4fm_modulate;

    /// The test identities: invented, outside registered ranges.
    const NAC: u16 = 291;
    const SRC: u32 = 9_990_001;
    const DST: u32 = 65_000;

    fn group() -> LinkControl {
        LinkControl {
            call: Call::Group,
            dst: DST,
            src: SRC,
            mfid: 0,
            emergency: false,
        }
    }

    fn private() -> LinkControl {
        LinkControl {
            call: Call::Private,
            dst: DST,
            src: SRC,
            mfid: 0,
            emergency: true,
        }
    }

    /// A small deterministic generator for the error patterns.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        /// `count` distinct positions below `n`.
        fn positions(&mut self, count: usize, n: usize) -> Vec<usize> {
            let mut p: Vec<usize> = Vec::new();
            while p.len() < count {
                let i = self.below(n);
                if !p.contains(&i) {
                    p.push(i);
                }
            }
            p
        }
    }

    /// Hard dibits as the demodulator gives them, certain, one sample apart.
    fn soft(dibits: &[u8]) -> Vec<SoftDibit> {
        dibits
            .iter()
            .enumerate()
            .map(|(i, &d)| SoftDibit {
                dibit: d,
                soft: [255 * (d >> 1), 255 * (d & 1)],
                at: i as f64,
            })
            .collect()
    }

    /// Bits (one per byte, 0 or 1) as the decoders take them: soft, certain.
    fn certain(bits: &[u8]) -> Vec<u8> {
        bits.iter().map(|&b| 255 * b).collect()
    }

    #[test]
    fn the_nid_round_trips_and_corrects_eleven_errors() {
        let mut rng = Lcg(1);
        for duid in 0..16u8 {
            let word = nid_encode(NAC, duid);
            assert_eq!(nid_decode(word), Some((NAC, duid, 0)), "duid {duid:x}");
            for _ in 0..50 {
                let e = rng.positions(11, 64).iter().fold(0u64, |e, &i| e | 1 << i);
                assert_eq!(nid_decode(word ^ e), Some((NAC, duid, 11)), "duid {duid:x}");
            }
        }
        // Twelve errors are past the code's reach: any answer is a wrong one.
        let defined = [
            DUID_HDU, DUID_TDU, DUID_LDU1, DUID_TSDU, DUID_LDU2, DUID_PDU, DUID_TDULC,
        ];
        let trials = 100_000;
        let wrong = (0..trials)
            .filter(|t| {
                let e = rng.positions(12, 64).iter().fold(0u64, |e, &i| e | 1 << i);
                nid_decode(nid_encode(NAC, defined[t % defined.len()]) ^ e).is_some()
            })
            .count();
        assert!(wrong * 1000 <= trials, "{wrong} of {trials} wrong");
    }

    #[test]
    fn link_control_round_trips() {
        let mut rng = Lcg(2);
        for lc in [group(), private()] {
            let hex = RS_LC.encode(&hexbits(lc_word(&lc, false), 12));
            let bits = ldu_lc_bits(&hex);
            assert_eq!(bits.len(), 240);
            let (got, errors) = ldu_hexbits(&certain(&bits));
            assert_eq!((parse_lc(&got[..12]), errors), (Some(lc), 0));

            // One bit in every Hamming(10,6,3) word.
            let mut b = bits.clone();
            for w in 0..24 {
                b[10 * w + rng.below(10)] ^= 1;
            }
            let (mut got, errors) = ldu_hexbits(&certain(&b));
            assert_eq!(errors, 24);
            assert_eq!(RS_LC.decode(&mut got), Some(0));
            assert_eq!(parse_lc(&got[..12]), Some(lc));

            // Six hexbits past their Hamming words: Reed-Solomon (24,12,13) takes them.
            let mut b = bits.clone();
            for w in rng.positions(6, 24) {
                b[10 * w] ^= 1;
                b[10 * w + 1] ^= 1;
            }
            let (mut got, _) = ldu_hexbits(&certain(&b));
            assert_eq!(RS_LC.decode(&mut got), Some(6));
            assert_eq!(parse_lc(&got[..12]), Some(lc));

            // The terminator's Golay(24,12,8) words: three bits each.
            let bits = tdulc_bits(&hex);
            assert_eq!(bits.len(), 288);
            let mut b = bits.clone();
            for w in 0..12 {
                for i in rng.positions(3, 24) {
                    b[24 * w + i] ^= 1;
                }
            }
            let (mut got, errors) = tdulc_hexbits(&certain(&b));
            assert_eq!(errors, 36);
            assert_eq!(RS_LC.decode(&mut got), Some(0));
            assert_eq!(parse_lc(&got[..12]), Some(lc));
        }
        // Six symbol errors anywhere, in the code alone.
        let data: Vec<u8> = (0..12).map(|i| (i * 5 + 1) as u8 & 0x3F).collect();
        let clean = RS_LC.encode(&data);
        let mut word = clean.clone();
        for i in rng.positions(6, 24) {
            word[i] ^= 1 + rng.below(63) as u8;
        }
        assert_eq!(RS_LC.decode(&mut word), Some(6));
        assert_eq!(word, clean);
    }

    #[test]
    fn encryption_sync_round_trips() {
        let mut rng = Lcg(3);
        for algid in [P25_ALGID_CLEAR, 0xAA] {
            let enc = EncSync { algid, kid: 7 };
            let hex = RS_ES.encode(&hexbits(es_word(&enc), 16));
            let mut b = ldu_lc_bits(&hex);
            // Four symbols past their Hamming words: Reed-Solomon (24,16,9) takes them.
            for w in rng.positions(4, 24) {
                b[10 * w] ^= 1;
                b[10 * w + 1] ^= 1;
            }
            let (mut got, _) = ldu_hexbits(&certain(&b));
            assert_eq!(RS_ES.decode(&mut got), Some(4));
            assert_eq!(parse_es(&got[..16]), Some(enc));

            // The header: Golay(18,6,8) words, three bits each, then eight symbols
            // Reed-Solomon (36,20,17) must take.
            let hex = RS_HEADER.encode(&hexbits(header_word(&enc, DST as u16), 20));
            let mut b = hdu_bits(&hex);
            assert_eq!(b.len(), 648);
            for w in 0..36 {
                for i in rng.positions(3, 18) {
                    b[18 * w + i] ^= 1;
                }
            }
            let (got, errors) = hdu_hexbits(&certain(&b));
            assert_eq!((got.clone(), errors), (hex.clone(), 108));
            let mut got = got;
            for i in rng.positions(8, 36) {
                got[i] ^= 1 + rng.below(63) as u8;
            }
            assert_eq!(RS_HEADER.decode(&mut got), Some(8));
            assert_eq!(got, hex);
        }
    }

    /// The pseudo-random mask as the IMBE coding states it, written out here apart from
    /// the module's: `p(0) = 16 u0`, `p(n) = (173 p(n-1) + 13849) mod 65536`, bit `n` of
    /// the mask is `p(n) >= 32768`, laid over the six protected words after the first,
    /// each from its top cell down.
    fn mask_cells(u0: u16) -> Vec<(usize, usize, u8)> {
        let mut p = 16 * u32::from(u0);
        let mut cells = Vec::new();
        for (row, len) in [(1, 23), (2, 23), (3, 23), (4, 15), (5, 15), (6, 15)] {
            for col in (0..len).rev() {
                p = (173 * p + 13849) % 65536;
                cells.push((row, col, u8::from(p >= 32768)));
            }
        }
        cells
    }

    #[test]
    fn an_imbe_frame_survives_its_own_coding() {
        let u: [u16; 8] = [0xA5C, 0x3F1, 0x812, 0x0FF, 0x5AA, 0x123, 0x7FE, 0x55];
        let frame = imbe_encode(&u);
        let bit = |row: usize, col: usize| u32::from(frame[23 * row + col] >= 128);
        let word = |row: usize, len: usize| (0..len).fold(0, |w, c| w | bit(row, c) << c);
        for &v in &frame {
            assert!(v == 0 || v == 255, "a coded frame is certain: {v}");
        }
        // Unused cells: past the Hamming words' 15 and the last vector's 7.
        for row in 4..7 {
            assert!((15..23).all(|c| frame[23 * row + c] == 0));
        }
        assert!((7..23).all(|c| frame[23 * 7 + c] == 0));
        // Row 0 is the Golay word of u0, unmasked, its top bit in column 22.
        assert_eq!(word(0, 23), golay23_encode(u[0]));
        assert_eq!(word(0, 23) >> 11, u32::from(u[0]));
        // Rows 1 to 6 carry the mask over their code words.
        let mut unmasked = frame;
        for (row, col, m) in mask_cells(u[0]) {
            unmasked[23 * row + col] ^= 255 * m;
        }
        let plain = |row: usize, len: usize| {
            (0..len).fold(0u32, |w, c| {
                w | u32::from(unmasked[23 * row + c] >= 128) << c
            })
        };
        for (row, &v) in u.iter().enumerate().take(7).skip(1) {
            let want = if row < 4 {
                golay23_encode(v)
            } else {
                u32::from(hamming15_encode(v))
            };
            assert_eq!(plain(row, IMBE_WORD_BITS[row]), want, "row {row}");
        }
        assert_eq!(word(7, 7), u32::from(u[7]));
        assert_eq!(imbe_decode(&frame), (u, 0));

        // Three errors in a masked Golay row are corrected.
        let mut hit = frame;
        for col in [0, 9, 22] {
            hit[23 * 2 + col] ^= 255;
        }
        assert_eq!(imbe_decode(&hit), (u, 3));
        // Two in c0, whose data seeds the mask over the rest.
        let mut hit = frame;
        for col in [4, 17] {
            hit[col] ^= 255;
        }
        assert_eq!(imbe_decode(&hit), (u, 2));
    }

    #[test]
    fn the_interleave_sends_every_used_cell_once() {
        let mut cells: Vec<usize> = IMBE_CELLS.iter().map(|&c| usize::from(c)).collect();
        cells.sort_unstable();
        let used: Vec<usize> = (0..8)
            .flat_map(|row| (0..IMBE_WORD_BITS[row]).map(move |col| IMBE_COLS * row + col))
            .collect();
        assert_eq!(cells, used);
        // The first dibit: the top bits of c0 and c1.
        assert_eq!(&IMBE_CELLS[..2], &[22, 23 + 21]);
    }

    #[test]
    fn the_bch_generator_is_the_designed_distance_23_code() {
        // The product of (x + α^i) over every power conjugate to 1..=22: the narrow-sense
        // primitive BCH code of designed distance 23. Its coefficients are 0 or 1.
        let mut roots: Vec<usize> = Vec::new();
        for i in 1..=2 * BCH_T {
            let mut c = i;
            while !roots.contains(&c) {
                roots.push(c);
                c = 2 * c % GF_ORDER;
            }
        }
        let g = roots.iter().fold(vec![1u8], |g, &r| {
            // Lowest coefficient first: g · (x + α^r).
            let mut next = vec![0u8; g.len() + 1];
            for (j, &c) in g.iter().enumerate() {
                next[j] ^= gf_mul(c, alpha(r));
                next[j + 1] ^= c;
            }
            next
        });
        assert_eq!(g.len(), BCH_CHECK_BITS as usize + 1);
        let word = g.iter().enumerate().fold(0u64, |w, (i, &c)| {
            assert!(c <= 1);
            w | u64::from(c) << i
        });
        assert_eq!(word, BCH_GENERATOR);
    }

    #[test]
    fn the_terminator_golay_is_the_m17_specifications_golay() {
        // The IMBE Golay (23,12) with a parity bit is the code M17's specification writes
        // out as a generator matrix: a check of the polynomial against an independent table.
        for data in 0..1u16 << GOLAY_DATA_BITS {
            assert_eq!(golay24_encode(data), crate::m17::golay24_encode(data));
        }
    }

    /// Every error pattern of `weight` bits among the low `n`.
    fn patterns(n: u32, weight: u32) -> Vec<u32> {
        (0..1u32 << n)
            .filter(|e| e.count_ones() == weight)
            .collect()
    }

    #[test]
    fn golay_23_corrects_every_pattern_of_three_or_fewer_errors() {
        for data in [0u16, 0xFFF, 0xA5C, 0x3F1, 0x812] {
            let word = golay23_encode(data);
            assert_eq!(golay23_decode(word), (data, 0));
            for weight in 1..=3 {
                for e in patterns(23, weight) {
                    assert_eq!(golay23_decode(word ^ e), (data, weight), "error {e:06x}");
                }
            }
        }
    }

    #[test]
    fn golay_24_corrects_three_errors_and_detects_four() {
        for data in [0u16, 0xFFF, 0x5A3, 0x03F] {
            let word = golay24_encode(data);
            assert_eq!(golay24_decode(word), Some((data, 0)));
            for weight in 1..=3 {
                for e in patterns(24, weight) {
                    assert_eq!(golay24_decode(word ^ e), Some((data, weight)), "{e:06x}");
                }
            }
            for e in patterns(24, 4) {
                assert_eq!(golay24_decode(word ^ e), None, "error {e:06x}");
            }
        }
    }

    #[test]
    fn status_symbols_are_removed() {
        let call = encode_call(
            NAC,
            group(),
            EncSync {
                algid: P25_ALGID_CLEAR,
                kid: 0,
            },
            1,
        );
        let hdu = 396;
        let ldu1 = &call[hdu..hdu + LDU_DIBITS];
        let units = P25Framer::new().push(&soft(ldu1));
        assert_eq!(units.len(), 1);
        let (_, nac, P25Unit::Ldu1 { lc, errors, .. }) = &units[0] else {
            panic!("{:?}", units[0]);
        };
        assert_eq!((*nac, *lc, *errors), (NAC, Some(group()), 0));

        // The same dibits read straight through, status symbols and all.
        let raw: Vec<u8> = ldu1[..LDU_DIBITS - LDU_DIBITS / STATUS_PERIOD]
            .iter()
            .flat_map(|d| [255 * (d >> 1), 255 * (d & 1)])
            .collect();
        let P25Unit::Ldu1 { lc, .. } = decode_unit(DUID_LDU1, &raw, 0) else {
            panic!("not an LDU1");
        };
        assert_eq!(lc, None);
    }

    /// `dibits` through the C4FM modulator at 48 kHz, followed by 10 ms with the carrier
    /// off so the receive filters give out the last symbols, conjugated when `invert`.
    fn on_air(dibits: &[u8], invert: bool) -> Vec<Complex<f32>> {
        let mut iq = c4fm_modulate(dibits, P25_BAUD, P25_DEVIATION_HZ, 48_000);
        iq.extend(std::iter::repeat_n(Complex::new(0.0, 0.0), 480));
        if invert {
            iq.iter_mut().for_each(|s| *s = s.conj());
        }
        iq
    }

    fn demod() -> Fsk4Demod {
        Fsk4Demod::new(
            48_000,
            Fsk4Params {
                symbol_rate_baud: P25_BAUD,
                deviation_hz: P25_DEVIATION_HZ,
                shape: Shape::C4fm,
            },
        )
        .expect("rate")
    }

    /// The units of `iq`, fed in uneven pieces as a stage would, the demodulator turned
    /// as the framer says.
    fn receive(iq: &[Complex<f32>], framer: &mut P25Framer) -> Vec<(f64, u16, P25Unit)> {
        let mut demod = demod();
        let mut units = Vec::new();
        for chunk in iq.chunks(777) {
            units.extend(framer.push(&demod.feed(chunk)));
            demod.invert(framer.inverted().unwrap_or(false));
        }
        units
    }

    #[test]
    fn a_control_channel_gives_trunking_units() {
        let units = receive(
            &on_air(&encode_control(NAC, 3), false),
            &mut P25Framer::new(),
        );
        let want: Vec<(u16, P25Unit)> = vec![(NAC, P25Unit::Trunking); 3];
        let got: Vec<(u16, P25Unit)> = units.into_iter().map(|u| (u.1, u.2)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn a_call_decodes_to_its_units() {
        let clear = EncSync {
            algid: P25_ALGID_CLEAR,
            kid: 0,
        };
        let call = encode_call(NAC, group(), clear, 3);
        for invert in [false, true] {
            let mut framer = P25Framer::new();
            let units = receive(&on_air(&call, invert), &mut framer);
            assert_eq!(framer.inverted(), Some(invert));
            assert_eq!(units.len(), 8, "invert {invert}: {units:?}");
            assert!(units.iter().all(|u| u.1 == NAC));
            assert_eq!(
                units[0].2,
                P25Unit::Header {
                    enc: Some(clear),
                    dst: Some(DST)
                }
            );
            let silence = imbe_decode(&imbe_encode(&[0; 8])).0;
            for sf in 0..3 {
                let P25Unit::Ldu1 { lc, voice, errors } = &units[1 + 2 * sf].2 else {
                    panic!("{:?}", units[1 + 2 * sf]);
                };
                assert_eq!((*lc, *errors), (Some(group()), 0));
                assert!(voice.iter().all(|f| imbe_decode(f) == (silence, 0)));
                let P25Unit::Ldu2 { enc, errors, .. } = &units[2 + 2 * sf].2 else {
                    panic!("{:?}", units[2 + 2 * sf]);
                };
                assert_eq!((*enc, *errors), (Some(clear), 0));
            }
            assert_eq!(units[7].2, P25Unit::Terminator { lc: Some(group()) });
            // Each unit's position is its sync's, a whole unit after the last.
            let sps = 10.0;
            assert!((units[2].0 - units[1].0 - LDU_DIBITS as f64 * sps).abs() < 2.0 * sps);
        }
    }
}
