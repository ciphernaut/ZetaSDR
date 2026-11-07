//! Framing rules: how a bit stream is cut into frames, and back.
//!
//! A [`Framing`] names the line code, the sync word (and an optional preamble that
//! must precede it exactly), the whitening, the length rule, the CRC and the bit order
//! of a burst protocol. [`decode_frames`] cuts frames out of one segment of sliced
//! bits; [`encode_frames`] is its inverse, for synthesised captures. Pure: no DSP.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crc::{CrcDef, CrcParam, crc_def};
use crate::slicer::bits_from_hex;

/// Longest frame after the sync, in bits: a `frame` event stays within 64 KiB.
pub const MAX_FRAME_BITS: usize = 65_536;

/// Longest sync word or preamble, in bits.
pub const WORD_BITS_MAX: u32 = 64;

/// Widest length field, in bits.
pub const LENGTH_FIELD_BITS_MAX: u32 = 32;

/// Bits per hex digit.
const HEX_DIGIT_BITS: u64 = 4;

/// Bits per byte.
const BYTE_BITS: usize = 8;

/// TI's PN9 whitening polynomial x^9 + x^5 + 1 (bit `k` for x^k).
const PN9_POLY: u32 = 0x221;

/// TI's PN9 seed.
const PN9_SEED: u32 = 0x1FF;

/// How bits are put on the line: `nrz` as is; `nrzi` NRZ-S, the HDLC/AX.25
/// convention (a 1 where the line bit equals the previous one, a 0 on a transition);
/// `manchester` `10` a 1 and `01` a 0, two line bits per bit; `differential` the XOR
/// of the line bit with the previous one (a 1 on a transition).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LineCode {
    #[default]
    Nrz,
    // NRZ-S, the HDLC/AX.25 convention: a 1 where the line bit equals the previous
    // one, a 0 on a transition. (Said in the enum's doc, the schema's description: a
    // doc comment here would make the schema a `oneOf` that AGENTS.md shows as `any`.)
    Nrzi,
    Manchester,
    Differential,
}

/// The order of the bits within each byte on the line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BitOrder {
    #[default]
    Msb,
    Lsb,
}

/// What a length field counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LengthUnit {
    Bits,
    Bytes,
}

impl LengthUnit {
    fn bits(self) -> i64 {
        match self {
            LengthUnit::Bits => 1,
            LengthUnit::Bytes => BYTE_BITS as i64,
        }
    }
}

/// A bit pattern: the first `bits` bits of `hex`, MSB first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Word {
    pub hex: String,
    /// 1..=64, and at most the hex's bits.
    pub bits: u32,
}

impl Word {
    fn check(&self, name: &str) -> Result<(), String> {
        if self.hex.is_empty() || !self.hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{name}.hex: hex"));
        }
        let have = self.hex.len() as u64 * HEX_DIGIT_BITS;
        if !(1..=WORD_BITS_MAX).contains(&self.bits) || u64::from(self.bits) > have {
            return Err(format!(
                "{name}.bits: 1..={WORD_BITS_MAX} and at most the hex's bits"
            ));
        }
        Ok(())
    }

    /// The word's bits, MSB first.
    pub fn to_bits(&self) -> Vec<u8> {
        bits_from_hex(&self.hex, self.bits as usize)
    }
}

/// The whitening XORed over the frame after the sync, byte-wise as TI's CC1101 does
/// it: each frame byte (as the row sees it, before `bit_order` on the line) is XORed
/// with the register's low 8 bits, then the register advances 8 bits; restarted at
/// each frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "WhiteningWire", into = "WhiteningWire")]
#[schemars(with = "WhiteningWire")]
pub enum Whitening {
    #[default]
    None,
    /// TI's PN9: `Lfsr { poly: 0x221, seed: 0x1FF }`.
    Pn9,
    /// A Fibonacci LFSR: `poly` the polynomial (bit `k` for x^k; its highest bit, the
    /// degree, is the register's width, the rest the taps), `seed` the start state.
    Lfsr { poly: u32, seed: u32 },
}

/// [`Whitening`] on the wire: `"none"`, `"pn9"` or `{"poly", "seed"}`.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
enum WhiteningWire {
    /// `none` or `pn9`.
    Name(String),
    Lfsr(LfsrWire),
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LfsrWire {
    /// The polynomial, bit `k` for x^k; not 0.
    poly: u32,
    seed: u32,
}

const WHITENING_ERR: &str = "whitening: none, pn9 or {poly, seed}";

// The name is checked as the value is read, so a bad name is refused with the same
// message `Framing::check` gives for the rest of the field.
impl TryFrom<WhiteningWire> for Whitening {
    type Error = String;
    fn try_from(w: WhiteningWire) -> Result<Self, String> {
        match w {
            WhiteningWire::Name(n) if n == "none" => Ok(Whitening::None),
            WhiteningWire::Name(n) if n == "pn9" => Ok(Whitening::Pn9),
            WhiteningWire::Name(_) => Err(WHITENING_ERR.into()),
            WhiteningWire::Lfsr(l) => Ok(Whitening::Lfsr {
                poly: l.poly,
                seed: l.seed,
            }),
        }
    }
}

impl From<Whitening> for WhiteningWire {
    fn from(w: Whitening) -> Self {
        match w {
            Whitening::None => WhiteningWire::Name("none".into()),
            Whitening::Pn9 => WhiteningWire::Name("pn9".into()),
            Whitening::Lfsr { poly, seed } => WhiteningWire::Lfsr(LfsrWire { poly, seed }),
        }
    }
}

/// The first `n` bytes of `w`'s sequence (all 0 for `none`): each the register's low
/// 8 bits, after which the register advances 8 bits (each shift right, the parity of
/// the tapped bits entering at the top).
pub fn whitening_bytes(w: &Whitening, n: usize) -> Vec<u8> {
    let (poly, seed) = match *w {
        Whitening::None => return vec![0; n],
        Whitening::Pn9 => (PN9_POLY, PN9_SEED),
        // `check` refuses a zero polynomial; it whitens nothing.
        Whitening::Lfsr { poly: 0, .. } => return vec![0; n],
        Whitening::Lfsr { poly, seed } => (poly, seed),
    };
    // The degree (the highest bit) is the register's width; the bits under it are
    // the taps. A degree-0 polynomial (1) is a one-bit register without taps.
    let degree = u32::BITS - 1 - poly.leading_zeros();
    let taps = poly & !(1u32 << degree);
    let width = degree.max(1);
    let mask = u32::MAX >> (u32::BITS - width);
    let mut reg = seed & mask;
    (0..n)
        .map(|_| {
            let out = (reg & 0xFF) as u8;
            for _ in 0..BYTE_BITS {
                let fb = (reg & taps).count_ones() & 1;
                reg = (reg >> 1) | (fb << (width - 1));
            }
            out
        })
        .collect()
}

/// A length field inside the frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LengthField {
    /// Where the field starts, in bits after the sync (in the frame's bytes as
    /// `bit_order` packs them).
    pub offset_bits: u32,
    /// 1..=32.
    pub bits: u32,
    /// The field's bytes most significant first; else least significant first.
    pub big_endian: bool,
    pub unit: LengthUnit,
    /// Added to the field's value, in `unit`s: the frame's bits after the sync are
    /// `(value + add) × unit`.
    pub add: i64,
}

/// How long a frame runs after its sync.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum Length {
    /// A fixed count of bits after the sync, 1..=65536.
    Fixed { fixed_bits: u32 },
    /// Read from a field.
    Field { field: LengthField },
    /// To the next sync hit, a seam or the input's end.
    ToNextSync { to_next_sync: bool },
}

/// The framing of a burst protocol: what `frame_decode` cuts and the frame
/// synthesisers write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct Framing {
    #[serde(default)]
    pub encoding: LineCode,
    pub sync: Word,
    /// Bits that must precede the sync exactly.
    #[serde(default)]
    pub preamble: Option<Word>,
    #[serde(default)]
    pub whitening: Whitening,
    pub length: Length,
    /// The CRC over the frame's bytes but the trailing field, against that field.
    #[serde(default)]
    pub crc: Option<CrcParam>,
    #[serde(default)]
    pub bit_order: BitOrder,
    /// Most sync bit errors, under `sync.bits`.
    #[serde(default)]
    pub max_errors: u32,
}

impl Framing {
    /// Refuse a framing that cannot be decoded, naming the field.
    pub fn check(&self) -> Result<(), String> {
        self.sync.check("sync")?;
        if let Some(p) = &self.preamble {
            p.check("preamble")?;
        }
        if self.max_errors >= self.sync.bits {
            return Err("max_errors: under sync.bits".into());
        }
        match &self.length {
            Length::Fixed { fixed_bits } => {
                if !(1..=MAX_FRAME_BITS as u32).contains(fixed_bits) {
                    return Err(format!("length.fixed_bits: 1..={MAX_FRAME_BITS}"));
                }
            }
            Length::Field { field } => {
                if !(1..=LENGTH_FIELD_BITS_MAX).contains(&field.bits) {
                    return Err(format!("length.field.bits: 1..={LENGTH_FIELD_BITS_MAX}"));
                }
            }
            Length::ToNextSync { to_next_sync } => {
                if !to_next_sync {
                    return Err("length.to_next_sync: true".into());
                }
            }
        }
        if let Whitening::Lfsr { poly: 0, .. } = self.whitening {
            return Err("whitening.poly: not 0".into());
        }
        if let Some(c) = &self.crc {
            c.resolve()?;
        }
        Ok(())
    }

    /// Line bits per bit.
    fn scale(&self) -> usize {
        if self.encoding == LineCode::Manchester {
            2
        } else {
            1
        }
    }

    /// Line bits before a sync that decoding it depends on: the preamble's, and the
    /// previous line bit for the differential codes.
    fn lookback(&self) -> usize {
        let pre = self.preamble.as_ref().map_or(0, |p| p.bits as usize);
        let ctx = usize::from(matches!(
            self.encoding,
            LineCode::Nrzi | LineCode::Differential
        ));
        self.scale() * pre + ctx
    }
}

/// One frame cut from a bit stream.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedFrame {
    /// The sync's first line bit in the input.
    pub at_bit: usize,
    pub sync_errors: u32,
    /// The frame after the sync, the CRC field included, packed by `bit_order` and
    /// unwhitened.
    pub bytes: Vec<u8>,
    /// `None` without a CRC.
    pub crc_ok: Option<bool>,
}

impl DecodedFrame {
    /// One past the frame's last line bit in the input, under `f`.
    pub fn end_bit(&self, f: &Framing) -> usize {
        self.at_bit + f.scale() * (f.sync.bits as usize + BYTE_BITS * self.bytes.len())
    }
}

/// What [`decode`] found in one segment.
#[derive(Debug, Default)]
pub(crate) struct Decoded {
    /// The frames cut, ascending.
    pub frames: Vec<DecodedFrame>,
    /// How many input bits are settled: the decoder may be called again from there
    /// with new bits appended.
    pub settled: usize,
    /// Every sync hit's `at_bit`.
    pub hits: Vec<usize>,
    /// The `at_bit` of each frame dropped for its length.
    pub bad: Vec<usize>,
    /// The `at_bit` of each frame whose rest is not yet in.
    pub pending: Vec<usize>,
    /// A `to_next_sync` frame closed only by the input's end: its `at_bit`, and the
    /// frame when its length is valid.
    pub open: Option<(usize, Option<DecodedFrame>)>,
}

/// Pack `bits` (whole bytes) into bytes by `order`.
pub(crate) fn pack(bits: &[u8], order: BitOrder) -> Vec<u8> {
    bits.chunks(BYTE_BITS)
        .map(|c| {
            let b = c.iter().fold(0u8, |a, &v| (a << 1) | (v & 1)) << (BYTE_BITS - c.len());
            match order {
                BitOrder::Msb => b,
                BitOrder::Lsb => b.reverse_bits(),
            }
        })
        .collect()
}

/// Unpack `bytes` into bits by `order`.
fn unpack(bytes: &[u8], order: BitOrder) -> Vec<u8> {
    bytes
        .iter()
        .flat_map(|&b| {
            let b = match order {
                BitOrder::Msb => b,
                BitOrder::Lsb => b.reverse_bits(),
            };
            (0..BYTE_BITS).rev().map(move |i| (b >> i) & 1)
        })
        .collect()
}

/// XOR `w`'s sequence over a frame's `bytes` (its own inverse).
fn whiten(bytes: &mut [u8], w: &Whitening) {
    if *w == Whitening::None {
        return;
    }
    let seq = whitening_bytes(w, bytes.len());
    for (b, p) in bytes.iter_mut().zip(seq) {
        *b ^= p;
    }
}

/// Bit `i` of `bytes`, MSB first.
fn bit_at(bytes: &[u8], i: usize) -> u64 {
    u64::from((bytes[i / BYTE_BITS] >> (BYTE_BITS - 1 - i % BYTE_BITS)) & 1)
}

/// The length field's value in `bytes`: its bits read MSB first; little-endian, its
/// 8-bit groups from `offset_bits` are taken least significant first.
fn read_field(bytes: &[u8], lf: &LengthField) -> u64 {
    let start = lf.offset_bits as usize;
    let n = lf.bits as usize;
    if lf.big_endian {
        (start..start + n).fold(0, |v, i| (v << 1) | bit_at(bytes, i))
    } else {
        let mut v = 0u64;
        for (g, from) in (start..start + n).step_by(BYTE_BITS).enumerate() {
            let to = (from + BYTE_BITS).min(start + n);
            let group = (from..to).fold(0, |a, i| (a << 1) | bit_at(bytes, i));
            v |= group << (BYTE_BITS * g);
        }
        v
    }
}

/// Write `v` into the length field of `bytes`, the inverse of [`read_field`].
fn write_field(bytes: &mut [u8], lf: &LengthField, v: u64) {
    let start = lf.offset_bits as usize;
    let n = lf.bits as usize;
    let mut put = |i: usize, bit: u64| {
        let m = 1u8 << (BYTE_BITS - 1 - i % BYTE_BITS);
        if bit == 1 {
            bytes[i / BYTE_BITS] |= m;
        } else {
            bytes[i / BYTE_BITS] &= !m;
        }
    };
    if lf.big_endian {
        for k in 0..n {
            put(start + k, (v >> (n - 1 - k)) & 1);
        }
    } else {
        for (g, from) in (start..start + n).step_by(BYTE_BITS).enumerate() {
            let to = (from + BYTE_BITS).min(start + n);
            let group = (v >> (BYTE_BITS * g)) & 0xFF;
            let w = to - from;
            for (k, i) in (from..to).enumerate() {
                put(i, (group >> (w - 1 - k)) & 1);
            }
        }
    }
}

/// The CRC field's byte count for `def`.
fn crc_bytes(def: &CrcDef) -> usize {
    usize::from(def.width).div_ceil(BYTE_BITS)
}

/// The CRC field as it trails the frame. The byte order follows the usual convention
/// for each CRC: big-endian for a non-reflected CRC (CCITT), little-endian for a
/// reflected one (`refout`: Modbus, DNP, X.25), as they are sent on the line.
pub(crate) fn crc_field(def: &CrcDef, value: u64) -> Vec<u8> {
    let n = crc_bytes(def);
    let be: Vec<u8> = (0..n)
        .rev()
        .map(|k| (value >> (BYTE_BITS * k)) as u8)
        .collect();
    if def.refout {
        be.into_iter().rev().collect()
    } else {
        be
    }
}

/// Whether `bytes` close with the CRC of the rest, under `def` (its field as
/// [`crc_field`] writes it). `bit_view` checks its CRC candidates with it, so a candidate
/// it lists decodes CRC-clean here.
pub(crate) fn crc_matches(def: &CrcDef, bytes: &[u8]) -> bool {
    let n = crc_bytes(def);
    if bytes.len() < n {
        return false;
    }
    let (body, field) = bytes.split_at(bytes.len() - n);
    crc_field(def, crc_def(def, body)) == field
}

/// The line decoded, per pair phase (one phase but for Manchester): Manchester takes
/// each pair's first line bit, which is the bit for `10` and `01` (a broken pair
/// reads as its first bit and fails the CRC).
fn line_decode(bits: &[u8], code: LineCode) -> Vec<Vec<u8>> {
    match code {
        LineCode::Nrz => vec![bits.to_vec()],
        LineCode::Nrzi | LineCode::Differential => {
            // NRZ-S: a 1 where the line holds; differential: a 1 where it changes.
            let hold = u8::from(code == LineCode::Nrzi);
            let mut prev = 0u8;
            vec![
                bits.iter()
                    .map(|&b| {
                        let d = b ^ prev ^ hold;
                        prev = b;
                        d
                    })
                    .collect(),
            ]
        }
        LineCode::Manchester => (0..2)
            .map(|ph| bits.iter().skip(ph).step_by(2).copied().collect())
            .collect(),
    }
}

/// The frames of one segment of line bits: what [`decode_frames`] returns, with the
/// counts and the pending frames the stage needs.
pub(crate) fn decode(bits: &[u8], f: &Framing) -> Decoded {
    decode_from(bits, f, false)
}

/// [`decode`] of a continuation: with `continued`, `bits` start with the context
/// line bit an earlier pass settled short of (nrzi and differential), whose own
/// decoded bit is unknown, so no sync or preamble may use it.
pub(crate) fn decode_from(bits: &[u8], f: &Framing, continued: bool) -> Decoded {
    let s = f.scale();
    let sync = f.sync.to_bits();
    let pre = f.preamble.as_ref().map(Word::to_bits).unwrap_or_default();
    let (sb, pl) = (sync.len(), pre.len());
    let lookback = f.lookback();
    let ctx = lookback - s * pl;
    // The first decoded index a sync's preamble (or the sync itself) may cover.
    let first = if continued { ctx } else { 0 };
    let crc = f
        .crc
        .as_ref()
        .and_then(|c| c.resolve().ok())
        .map(|(d, _)| d);
    let dec = line_decode(bits, f.encoding);
    // The sync's errors at decoded index `j` of `d`, `None` past the limit or without
    // the preamble right before it.
    let hit_at = |d: &[u8], j: usize| -> Option<u32> {
        if j < pl + first || d[j - pl..j] != pre[..] {
            return None;
        }
        let mut errors = 0u32;
        for (x, y) in d[j..j + sb].iter().zip(&sync) {
            errors += u32::from(x != y);
            if errors > f.max_errors {
                return None;
            }
        }
        Some(errors)
    };
    let cut = |d: &[u8], at: usize, errors: u32, b0: usize, nb: usize| -> DecodedFrame {
        let mut bytes = pack(&d[b0..b0 + nb], f.bit_order);
        whiten(&mut bytes, &f.whitening);
        DecodedFrame {
            at_bit: at,
            sync_errors: errors,
            crc_ok: crc.as_ref().map(|def| crc_matches(def, &bytes)),
            bytes,
        }
    };
    let valid = |nb: i64| {
        nb >= BYTE_BITS as i64 && nb % BYTE_BITS as i64 == 0 && nb <= MAX_FRAME_BITS as i64
    };
    let mut out = Decoded::default();
    // The end of the last frame cut: bits before it are never hunted again.
    let mut floor = 0usize;
    let mut first_pending: Option<usize> = None;
    let mut r = 0usize;
    let settle_at = |r: usize, floor: usize| {
        r.saturating_sub(lookback)
            .max(floor.saturating_sub(ctx))
            .min(r)
    };
    let end_settle = loop {
        let (ph, j) = (r % s, r / s);
        let d = &dec[ph];
        if j + sb > d.len() {
            break settle_at(r, floor);
        }
        let Some(errors) = hit_at(d, j) else {
            r += 1;
            continue;
        };
        out.hits.push(r);
        let b0 = j + sb;
        let nb: i64 = match &f.length {
            Length::Fixed { fixed_bits } => i64::from(*fixed_bits),
            Length::Field { field } => {
                let need = (field.offset_bits as usize + field.bits as usize).div_ceil(BYTE_BITS)
                    * BYTE_BITS;
                if b0 + need > d.len() {
                    out.pending.push(r);
                    first_pending.get_or_insert(settle_at(r, floor));
                    r += s * sb;
                    continue;
                }
                let mut head = pack(&d[b0..b0 + need], f.bit_order);
                whiten(&mut head, &f.whitening);
                let v = read_field(&head, field) as i64;
                v.saturating_add(field.add)
                    .saturating_mul(field.unit.bits())
            }
            Length::ToNextSync { .. } => {
                let next = (b0..)
                    .take_while(|&k| k + sb <= d.len())
                    .find(|&k| hit_at(d, k).is_some());
                match next {
                    Some(k) => (k - pl) as i64 - b0 as i64,
                    // Past the longest frame, no later sync can close it validly:
                    // dropped as a bad length now, so its bits settle.
                    None if d.len() - b0 > MAX_FRAME_BITS => (d.len() - b0) as i64,
                    None => {
                        let nb = (d.len() - b0) as i64;
                        let frame = valid(nb).then(|| cut(d, r, errors, b0, nb as usize));
                        out.open = Some((r, frame));
                        break settle_at(r, floor);
                    }
                }
            }
        };
        if !valid(nb) {
            out.bad.push(r);
            r += s * sb;
            continue;
        }
        let nb = nb as usize;
        if b0 + nb > d.len() {
            out.pending.push(r);
            first_pending.get_or_insert(settle_at(r, floor));
            r += s * sb;
            continue;
        }
        out.frames.push(cut(d, r, errors, b0, nb));
        r = ph + s * (b0 + nb);
        floor = r;
    };
    // A Manchester frame ending at the last pair of an odd count reads past the input.
    out.settled = first_pending
        .map_or(end_settle, |p| p.min(end_settle))
        .min(bits.len());
    out
}

/// The frames of one segment of line bits under `f`, ascending, and how many input
/// bits are settled: when a frame's rest is not yet in, settling stops before its
/// sync (and the preamble and line context it depends on), so the decoder may be
/// called again from there with new bits appended. A `to_next_sync` frame closed only
/// by the input's end comes last and lies past the settled count; one already past
/// [`MAX_FRAME_BITS`] with no sync after it is dropped and settled past.
///
/// The line code is decoded first, then the sync hunted at every line bit (at most
/// `max_errors` bit errors, the preamble's bits right before it exactly). A frame
/// whose length is not whole bytes, under one byte, over [`MAX_FRAME_BITS`] or past
/// the input is dropped, and the hunt resumes after its sync.
pub fn decode_frames(bits: &[u8], f: &Framing) -> (Vec<DecodedFrame>, usize) {
    let d = decode(bits, f);
    let mut frames = d.frames;
    if let Some((_, Some(open))) = d.open {
        frames.push(open);
    }
    (frames, d.settled)
}

/// Whether a body of `body_len` bytes frames and decodes under `f` ([`encode_frames`]
/// panics otherwise): the frame (the CRC included) must fit [`MAX_FRAME_BITS`]; a fixed
/// length must equal it; with a `field` length rule the body must reach the field's end,
/// the frame must be whole units and its length must fit the field.
pub fn check_body(f: &Framing, body_len: usize) -> Result<(), String> {
    let crc_n = f
        .crc
        .as_ref()
        .and_then(|c| c.resolve().ok())
        .map_or(0, |(d, _)| crc_bytes(&d));
    let frame_bits = (body_len + crc_n) * BYTE_BITS;
    if frame_bits > MAX_FRAME_BITS {
        return Err(format!(
            "a {body_len}-byte body and its {crc_n}-byte CRC pass {MAX_FRAME_BITS} bits"
        ));
    }
    match &f.length {
        Length::Fixed { fixed_bits } if frame_bits != *fixed_bits as usize => Err(format!(
            "length.fixed_bits: {fixed_bits} bits is {} B, body {body_len} + crc {crc_n} is {} B",
            *fixed_bits as usize / BYTE_BITS,
            body_len + crc_n
        )),
        Length::Field { field } => {
            let end = (field.offset_bits + field.bits) as usize;
            if body_len * BYTE_BITS < end {
                return Err(format!(
                    "a {body_len}-byte body ends before the length field (bit {end})"
                ));
            }
            let total = frame_bits as i64;
            let unit = field.unit.bits();
            if total % unit != 0 {
                return Err("the frame is not a whole number of length units".into());
            }
            let v = total / unit - field.add;
            if v < 0 || (field.bits < u64::BITS && (v as u64) >= (1u64 << field.bits)) {
                return Err(format!("the length {v} does not fit the length field"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The line bits of `bodies` framed by `f`, the inverse of [`decode_frames`]: per
/// frame the preamble, the sync, then the body with a `field` length written into it
/// (the body must reach the field's end) and the CRC appended when given, the bytes
/// whitened, then ordered by `bit_order`; the line code is encoded over everything (Manchester
/// doubles the bits).
pub fn encode_frames(bodies: &[Vec<u8>], f: &Framing) -> Vec<u8> {
    let crc = f
        .crc
        .as_ref()
        .and_then(|c| c.resolve().ok())
        .map(|(d, _)| d);
    let crc_n = crc.as_ref().map_or(0, crc_bytes);
    let sync = f.sync.to_bits();
    let pre = f.preamble.as_ref().map(Word::to_bits).unwrap_or_default();
    let mut bits = Vec::new();
    for body in bodies {
        let mut bytes = body.clone();
        check_body(f, bytes.len()).unwrap_or_else(|e| panic!("encode_frames: {e}"));
        if let Length::Field { field } = &f.length {
            let total = ((bytes.len() + crc_n) * BYTE_BITS) as i64;
            let v = total / field.unit.bits() - field.add;
            write_field(&mut bytes, field, v as u64);
        }
        if let Some(def) = &crc {
            let field = crc_field(def, crc_def(def, &bytes));
            bytes.extend(field);
        }
        whiten(&mut bytes, &f.whitening);
        bits.extend_from_slice(&pre);
        bits.extend_from_slice(&sync);
        bits.extend(unpack(&bytes, f.bit_order));
    }
    match f.encoding {
        LineCode::Nrz => bits,
        LineCode::Nrzi | LineCode::Differential => {
            let hold = u8::from(f.encoding == LineCode::Nrzi);
            let mut prev = 0u8;
            bits.iter()
                .map(|&d| {
                    prev ^= d ^ hold;
                    prev
                })
                .collect()
        }
        LineCode::Manchester => bits.iter().flat_map(|&d| [d, 1 - d]).collect(),
    }
}

/// Test helpers shared with `frame_decode`'s tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{BitOrder, Framing, Length, LineCode, Whitening, Word};
    use crate::crc::CrcParam;

    /// A framing on `sync` (its first `bits` bits) cut by `length`: NRZ, no whitening,
    /// CRC-16-CCITT, MSB first, no preamble, no sync errors.
    pub fn fx(sync: &str, bits: u32, length: Length) -> Framing {
        Framing {
            encoding: LineCode::Nrz,
            sync: Word {
                hex: sync.into(),
                bits,
            },
            preamble: None,
            whitening: Whitening::None,
            length,
            crc: Some(CrcParam::Name("crc16-ccitt".into())),
            bit_order: BitOrder::Msb,
            max_errors: 0,
        }
    }

    /// `n` pseudo-random bodies of `len` bytes.
    pub fn bodies(n: usize, len: usize, seed: u64) -> Vec<Vec<u8>> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                (0..len)
                    .map(|_| {
                        s ^= s << 13;
                        s ^= s >> 7;
                        s ^= s << 17;
                        (s >> 24) as u8
                    })
                    .collect()
            })
            .collect()
    }

    /// [`bodies`] with body `k` cut to `lens[k]` bytes (each at most 64).
    pub fn bodies_of(lens: &[usize], seed: u64) -> Vec<Vec<u8>> {
        bodies(lens.len(), 64, seed)
            .into_iter()
            .zip(lens)
            .map(|(mut b, &l)| {
                b.truncate(l);
                b
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::test_support::{bodies, bodies_of, fx};
    use super::*;

    const TI_PN9: [u8; 8] = [0xFF, 0xE1, 0x1D, 0x9A, 0xED, 0x85, 0x33, 0x24];

    /// Parse `v` as a `Framing` and check it, as the stage's `check_params` does.
    fn checked(v: serde_json::Value) -> Result<(), String> {
        let f: Framing = serde_json::from_value(v).map_err(|e| e.to_string())?;
        f.check()
    }

    fn assert_round_trip(raw: &[u8], f: &Framing, bodies: &[Vec<u8>]) {
        let (frames, _) = decode_frames(raw, f);
        assert_eq!(frames.len(), bodies.len(), "{frames:?}");
        for (fr, b) in frames.iter().zip(bodies) {
            assert_eq!(&fr.bytes[..b.len()], &b[..]);
            assert_eq!(fr.bytes.len(), b.len() + 2);
            assert_eq!(fr.crc_ok, Some(true));
            assert_eq!(fr.sync_errors, 0);
        }
        assert!(frames.windows(2).all(|w| w[0].at_bit < w[1].at_bit));
    }

    #[test]
    fn round_trip_on_each_encoding() {
        for enc in [
            LineCode::Nrz,
            LineCode::Nrzi,
            LineCode::Manchester,
            LineCode::Differential,
        ] {
            let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
            f.encoding = enc;
            let bs = bodies(10, 32, 1);
            // One leading bit: for Manchester the frames then sit at the odd pair phase.
            let mut raw = vec![0u8];
            raw.extend(encode_frames(&bs, &f));
            let scale = if enc == LineCode::Manchester { 2 } else { 1 };
            assert_eq!(raw.len(), 1 + scale * 10 * (16 + 272), "{enc:?}");
            assert_round_trip(&raw, &f, &bs);
            let (frames, _) = decode_frames(&raw, &f);
            assert_eq!(frames[0].at_bit, 1, "{enc:?}");
            assert_eq!(frames[1].at_bit, 1 + scale * 288, "{enc:?}");
        }
        // NRZ-S holds the line for a 1; differential changes it for a 1.
        assert_eq!(
            line_decode(&[0, 0, 1, 1], LineCode::Nrzi),
            [vec![1, 1, 0, 1]]
        );
        assert_eq!(
            line_decode(&[0, 0, 1, 1], LineCode::Differential),
            [vec![0, 0, 1, 0]]
        );
        let bs = bodies(10, 32, 1);
        let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
        f.encoding = LineCode::Nrzi;
        let nrzi = encode_frames(&bs, &f);
        f.encoding = LineCode::Differential;
        let differential = encode_frames(&bs, &f);
        assert_ne!(nrzi, differential);
        assert!(decode_frames(&nrzi, &f).0.is_empty());
    }

    #[test]
    fn round_trip_on_each_length_rule() {
        let bs = bodies(6, 34 - 2, 2);
        let f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
        assert_round_trip(&encode_frames(&bs, &f), &f, &bs);

        let lens = [5, 20, 33, 8, 47, 12];
        let f = fx(
            "2dd4",
            16,
            Length::Field {
                field: LengthField {
                    offset_bits: 0,
                    bits: 8,
                    big_endian: true,
                    unit: LengthUnit::Bytes,
                    add: 3,
                },
            },
        );
        let mut bs = bodies_of(&lens, 3);
        let raw = encode_frames(&bs, &f);
        // The encoder writes the length byte: the bytes after it and the CRC.
        for b in &mut bs {
            b[0] = (b.len() + 2 - 3) as u8;
        }
        assert_round_trip(&raw, &f, &bs);

        let f = fx("1acffc1d", 32, Length::ToNextSync { to_next_sync: true });
        let bs = bodies_of(&lens, 4);
        assert_round_trip(&encode_frames(&bs, &f), &f, &bs);
    }

    #[test]
    fn pn9_matches_the_published_sequence() {
        // Byte-wise, TI's way: the register's low 8 bits, then 8 shifts.
        assert_eq!(whitening_bytes(&Whitening::Pn9, 8), TI_PN9);
        // Through the decoder, msb (the default): zero bits after the sync come out
        // as the sequence's bytes, whatever the bit order.
        let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 64 });
        f.crc = None;
        f.whitening = Whitening::Pn9;
        let mut raw = crate::slicer::bits_from_hex("2dd4", 16);
        raw.extend([0u8; 64]);
        let (frames, _) = decode_frames(&raw, &f);
        assert_eq!(frames[0].bytes, TI_PN9);
        assert_eq!(frames[0].crc_ok, None);
        f.bit_order = BitOrder::Lsb;
        assert_eq!(decode_frames(&raw, &f).0[0].bytes, TI_PN9);
        // A CC1101 stream (bytes XORed with the sequence, sent MSB first) decodes
        // with pn9 and msb.
        let bs = bodies(4, 30, 5);
        let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 256 });
        f.whitening = Whitening::Pn9;
        let mut cc1101 = Vec::new();
        for b in &bs {
            let mut bytes = b.clone();
            let crc = crate::crc::crc(&crate::crc::CRC16_CCITT, &bytes) as u16;
            bytes.extend(crc.to_be_bytes());
            let pn = whitening_bytes(&Whitening::Pn9, bytes.len());
            cc1101.extend(crate::slicer::bits_from_hex("2dd4", 16));
            for (x, p) in bytes.iter().zip(pn) {
                cc1101.extend((0..8).rev().map(|i| ((x ^ p) >> i) & 1));
            }
        }
        assert_eq!(encode_frames(&bs, &f), cc1101);
        assert_round_trip(&cc1101, &f, &bs);
        f.whitening = Whitening::Lfsr {
            poly: 0x221,
            seed: 0x1FF,
        };
        assert_eq!(whitening_bytes(&f.whitening, 8), TI_PN9);
        assert_round_trip(&encode_frames(&bs, &f), &f, &bs);
        // A whitened length field is read after unwhitening.
        let mut f = fx(
            "2dd4",
            16,
            Length::Field {
                field: LengthField {
                    offset_bits: 0,
                    bits: 8,
                    big_endian: true,
                    unit: LengthUnit::Bytes,
                    add: 3,
                },
            },
        );
        f.whitening = Whitening::Pn9;
        let mut bs = bodies_of(&[9, 30, 17], 6);
        let raw = encode_frames(&bs, &f);
        for b in &mut bs {
            b[0] = (b.len() - 1) as u8;
        }
        assert_round_trip(&raw, &f, &bs);
    }

    #[test]
    fn lsb_bit_order_reverses_each_byte() {
        let bs = bodies(3, 32, 6);
        let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
        let msb = encode_frames(&bs, &f);
        f.bit_order = BitOrder::Lsb;
        let lsb = encode_frames(&bs, &f);
        assert_round_trip(&lsb, &f, &bs);
        assert_ne!(msb, lsb);
        for (m, l) in msb.chunks(288).zip(lsb.chunks(288)) {
            assert_eq!(m[..16], l[..16], "the sync is not reordered");
            for (mb, lb) in m[16..].chunks(8).zip(l[16..].chunks(8)) {
                let rev: Vec<u8> = mb.iter().rev().copied().collect();
                assert_eq!(rev, lb);
            }
        }
    }

    #[test]
    fn max_errors_admits_a_damaged_sync_and_a_preamble_is_required() {
        let bs = bodies(1, 32, 7);
        let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
        let mut raw = encode_frames(&bs, &f);
        raw[3] ^= 1;
        assert!(decode_frames(&raw, &f).0.is_empty());
        f.max_errors = 1;
        let (frames, _) = decode_frames(&raw, &f);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].sync_errors, 1);
        assert_eq!(frames[0].crc_ok, Some(true));

        let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
        let bare = encode_frames(&bs, &f);
        f.preamble = Some(Word {
            hex: "aaaa".into(),
            bits: 16,
        });
        assert!(decode_frames(&bare, &f).0.is_empty());
        let with = encode_frames(&bs, &f);
        assert_eq!(with.len(), bare.len() + 16);
        let (frames, _) = decode_frames(&with, &f);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].at_bit, 16);
    }

    #[test]
    fn a_bad_length_field_drops_the_frame_and_goes_on() {
        let f = fx(
            "2dd4",
            16,
            Length::Field {
                field: LengthField {
                    offset_bits: 0,
                    bits: 8,
                    big_endian: true,
                    unit: LengthUnit::Bytes,
                    add: 3,
                },
            },
        );
        let bs = bodies_of(&[20, 24, 28, 22, 26, 21], 8);
        let mut raw = Vec::new();
        let mut starts = Vec::new();
        for b in &bs {
            starts.push(raw.len());
            raw.extend(encode_frames(std::slice::from_ref(b), &f));
        }
        // Frame 3's length byte reads 0xFF: 258 bytes, past the input.
        for i in 0..8 {
            raw[starts[2] + 16 + i] = 1;
        }
        let (frames, settled) = decode_frames(&raw, &f);
        let at: Vec<usize> = frames.iter().map(|fr| fr.at_bit).collect();
        assert_eq!(at, [starts[0], starts[1], starts[3], starts[4], starts[5]]);
        assert!(frames.iter().all(|fr| fr.crc_ok == Some(true)));
        assert_eq!(settled, starts[2], "settling stops before frame 3's sync");
        // Called again with the rest, frame 3 is still not cut.
        let (again, _) = decode_frames(&raw[settled..], &f);
        assert_eq!(again.len(), 3);
    }

    #[test]
    fn the_crc_field_is_big_endian_unless_reflected() {
        let body = b"123456789".to_vec();
        let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 88 });
        let (frames, _) = decode_frames(&encode_frames(std::slice::from_ref(&body), &f), &f);
        assert_eq!(frames[0].bytes[9..], [0x29, 0xB1]);
        f.crc = Some(crate::crc::CrcParam::Name("crc16-modbus".into()));
        let (frames, _) = decode_frames(&encode_frames(&[body], &f), &f);
        assert_eq!(frames[0].bytes[9..], [0x37, 0x4B]);
        assert_eq!(frames[0].crc_ok, Some(true));
    }

    #[test]
    fn check_names_the_bad_field() {
        let base = || json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}});
        assert_eq!(checked(base()), Ok(()));
        let mut v = base();
        v["sync"]["hex"] = json!("zz");
        assert_eq!(checked(v), Err("sync.hex: hex".into()));
        let mut v = base();
        v["sync"]["bits"] = json!(70);
        let e = checked(v).unwrap_err();
        assert!(e.starts_with("sync.bits: 1..=64"), "{e}");
        let mut v = base();
        v["sync"]["bits"] = json!(20);
        let e = checked(v).unwrap_err();
        assert!(e.starts_with("sync.bits: 1..=64"), "{e}");
        let mut v = base();
        v["max_errors"] = json!(16);
        assert_eq!(checked(v), Err("max_errors: under sync.bits".into()));
        let mut v = base();
        v["whitening"] = json!("pn7");
        assert_eq!(
            checked(v),
            Err("whitening: none, pn9 or {poly, seed}".into())
        );
        let mut v = base();
        v["whitening"] = json!({"poly": 0, "seed": 1});
        assert_eq!(checked(v), Err("whitening.poly: not 0".into()));
        let mut v = base();
        v["crc"] = json!("crc99");
        let e = checked(v).unwrap_err();
        assert!(e.starts_with("crc: unknown name"), "{e}");
        let mut v = base();
        v["preamble"] = json!({"hex": "aa", "bits": 9});
        let e = checked(v).unwrap_err();
        assert!(e.starts_with("preamble.bits"), "{e}");
        let mut v = base();
        v["length"] = json!({"fixed_bits": 0});
        assert_eq!(checked(v), Err("length.fixed_bits: 1..=65536".into()));
        let mut v = base();
        v["length"] = json!({"field": {"offset_bits": 0, "bits": 33, "big_endian": true, "unit": "bytes", "add": 0}});
        assert_eq!(checked(v), Err("length.field.bits: 1..=32".into()));
        let mut v = base();
        v["extra"] = json!(1);
        assert!(checked(v).is_err());
        // An untagged length variant refuses another variant's key.
        let mut v = base();
        v["length"] = json!({"fixed_bits": 272, "to_next_sync": true});
        assert!(checked(v).is_err());
        let mut v = base();
        v["length"] = json!({"to_next_sync": true, "extra": 1});
        assert!(checked(v).is_err());
        assert!(
            checked(json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"to_next_sync": true}, "whitening": "pn9", "encoding": "manchester", "bit_order": "lsb", "crc": "crc16-modbus"}))
                .is_ok()
        );
    }

    #[test]
    fn a_manchester_frame_missing_only_its_last_chip_settles_within_the_input() {
        let mut f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
        f.encoding = LineCode::Manchester;
        let raw = encode_frames(&bodies(1, 32, 1), &f);
        // The last chip is the complement of the last bit: the first-of-pair reading
        // does not need it, so the frame is whole and the settled count would pass the end.
        let cut = &raw[..raw.len() - 1];
        let (frames, settled) = decode_frames(cut, &f);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].crc_ok, Some(true));
        assert!(settled <= cut.len(), "settled {settled} of {}", cut.len());
    }
}
