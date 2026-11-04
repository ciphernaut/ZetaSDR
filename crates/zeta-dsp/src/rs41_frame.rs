//! Vaisala RS41 frame generator: builds the 320-byte regular frame an RS41-SG sends once a
//! second, for the `rs41` synthesiser family (`zeta modes check rs41_decode`).
//!
//! Written from the public protocol description, not from any decoder's source: the
//! bazjo/RS41_Decoding notes (<https://github.com/bazjo/RS41_Decoding>, the frame format
//! in its README and the block tables in `RS41-SGP/README.md`). Where the notes are
//! silent (the Reed-Solomon code's parameters, the CRC's byte order) the conventions
//! below were checked against the descrambled example frames the same repository
//! publishes (`RS41-SGM/example_raw_data`). `rs41mod` served only as the oracle: it
//! decodes the frames this module builds (`zeta modes check rs41_decode`).
//!
//! - **Line**: 4800 baud GFSK, one frame a second, preceded by 320 bits of alternating
//!   0/1 ending in a 1. The bytes are sent least significant bit first (the notes call
//!   the header "bitwise little endian"); the multi-byte fields inside the blocks are
//!   little endian too.
//! - **Header**: bytes 0x000-0x007, `86 35 F4 40 93 DF 1A 60` once descrambled, so
//!   `10 B6 CA 11 22 96 12 F8` as sent.
//! - **Scrambling**: every byte of the frame, the header included, is XORed with a fixed
//!   64-byte mask ([`MASK`], repeating) before transmission.
//! - **Reed-Solomon**: bytes 0x008-0x037 are 48 parity bytes, two RS(255,231) codewords
//!   over GF(256) with field polynomial 0x11D. The codewords interleave the bytes from
//!   0x038 to the end (132 each): the even offsets from 0x038 make the first codeword
//!   (parity at 0x008-0x01F), the odd offsets the second (parity at 0x020-0x037); each is
//!   shortened by 99 zero bytes. In polynomial terms parity byte `j` is the coefficient of
//!   `x^j` and data byte `i` that of `x^(24+i)`, and the generator's roots are
//!   `alpha^0..alpha^23` (first consecutive root 0): every codeword of the example
//!   frames has zero syndromes under exactly these conventions.
//! - **Blocks**: 0x038 is the frame type (`0x0F` regular), then the blocks, each a block
//!   id, a data length, the data and a CRC-16/CCITT (poly 0x1021, init 0xFFFF) of the data,
//!   stored little endian (likewise checked against the example frames): STATUS (`0x79`,
//!   40 bytes: frame number, the 8-character serial, battery, flags, the subframe number
//!   and a 16-byte piece of the calibration subframe), MEAS (`0x7A`, 42), GPSINFO (`0x7C`,
//!   30: GPS week, time of week in ms, 12 satellite slots), GPSRAW (`0x7D`, 89), GPSPOS
//!   (`0x7B`, 21: ECEF position in cm and velocity in cm/s, all signed, then satellites
//!   used, speed accuracy and pDOP) and EMPTY (`0x76`, zeros to the end of the frame).
//!
//! What the generator fills: the serial, the frame number and the subframe number in
//! STATUS (the subframe itself, the calibration data, stays zero), satellites and a time
//! in GPSINFO, the position (WGS84 ECEF) and the climb `vel_v` along the local vertical
//! in GPSPOS. MEAS and GPSRAW are zero.
//!
//! Against the oracle (`rs41mod --json --ecc --crc`, sonde-decoders 1.8.1): the first
//! frame built this way decoded, id, position, climb and time included, with nothing but
//! the fields above non-zero; the calibration subframe and the PTU block may stay zero.
//! The polarity is a 1 bit on the higher tone (+deviation): the same audio inverted
//! decodes nothing.

/// The frame header, descrambled.
pub const HEADER: [u8; 8] = [0x86, 0x35, 0xF4, 0x40, 0x93, 0xDF, 0x1A, 0x60];

/// The scrambling mask: frame byte `i` is sent XORed with `MASK[i % 64]`.
pub const MASK: [u8; 64] = [
    0x96, 0x83, 0x3E, 0x51, 0xB1, 0x49, 0x08, 0x98, 0x32, 0x05, 0x59, 0x0E, 0xF9, 0x44, 0xC6, 0x26,
    0x21, 0x60, 0xC2, 0xEA, 0x79, 0x5D, 0x6D, 0xA1, 0x54, 0x69, 0x47, 0x0C, 0xDC, 0xE8, 0x5C, 0xF1,
    0xF7, 0x76, 0x82, 0x7F, 0x07, 0x99, 0xA2, 0x2C, 0x93, 0x7C, 0x30, 0x63, 0xF5, 0x10, 0x2E, 0x61,
    0xD0, 0xBC, 0xB4, 0xB6, 0x06, 0xAA, 0xF4, 0x23, 0x78, 0x6E, 0x3B, 0xAE, 0xBF, 0x7B, 0x4C, 0xC1,
];

/// A regular frame's length in bytes.
pub const FRAME_LEN: usize = 320;
/// Bits of alternating 0/1 sent before each frame.
pub const PREAMBLE_BITS: usize = 320;

/// Where the frame type byte sits; the codewords' data starts here.
const FRAME_TYPE_AT: usize = 0x38;
/// The STATUS block's subframe count: 51 pieces of 16 bytes, numbered 0..=50.
const SUBFRAMES: u32 = 51;

const WGS84_A: f64 = 6_378_137.0;
const WGS84_F: f64 = 1.0 / 298.257_223_563;

/// CRC-16/CCITT: polynomial 0x1021, initial value 0xFFFF, no reflection, no final XOR.
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    crate::crc::crc(&crate::crc::CRC16_CCITT, data) as u16
}

/// GF(256) with field polynomial 0x11D, alpha = 2.
struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
}

impl Gf {
    fn new() -> Self {
        let (mut exp, mut log) = ([0u8; 512], [0u8; 256]);
        let mut x = 1u16;
        // alpha has order 255, so the table simply runs on past it and repeats.
        for (i, e) in exp.iter_mut().enumerate() {
            *e = x as u8;
            if i < 255 {
                log[x as usize] = i as u8;
            }
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11D;
            }
        }
        Gf { exp, log }
    }

    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[usize::from(self.log[usize::from(a)]) + usize::from(self.log[usize::from(b)])]
        }
    }
}

/// The RS41's Reed-Solomon (255,231) encoder: GF(256) polynomial 0x11D, generator
/// `(x - alpha^0)(x - alpha^1)...(x - alpha^23)`.
pub struct Rs255_231 {
    gf: Gf,
    /// The generator's coefficients, `g[k]` that of `x^k` (`g[24]` = 1).
    g: [u8; 25],
}

impl Default for Rs255_231 {
    fn default() -> Self {
        Self::new()
    }
}

impl Rs255_231 {
    pub fn new() -> Self {
        let gf = Gf::new();
        let mut g = [0u8; 25];
        g[0] = 1;
        for k in 0..24 {
            // Multiply by (x + alpha^k).
            let root = gf.exp[k];
            for j in (1..=k + 1).rev() {
                g[j] = g[j - 1] ^ gf.mul(g[j], root);
            }
            g[0] = gf.mul(g[0], root);
        }
        Rs255_231 { gf, g }
    }

    /// The parity of `data`, data byte `i` being the coefficient of `x^(24+i)` and parity
    /// byte `j` that of `x^j` (the RS41's order): the remainder of `data(x) * x^24` by the
    /// generator.
    pub fn encode(&self, data: &[u8; 231]) -> [u8; 24] {
        let mut reg = [0u8; 24];
        for &d in data.iter().rev() {
            let fb = d ^ reg[23];
            for j in (1..24).rev() {
                reg[j] = reg[j - 1] ^ self.gf.mul(fb, self.g[j]);
            }
            reg[0] = self.gf.mul(fb, self.g[0]);
        }
        reg
    }
}

/// What one frame carries: the sonde's 8-character serial, its position (degrees, metres
/// above the WGS84 ellipsoid), its climb in m/s and the frame number.
#[derive(Clone, Debug, PartialEq)]
pub struct Rs41Params {
    pub serial: String,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
    pub vel_v: f64,
    pub frame_no: u32,
}

/// Append one block: id, length, `data`, CRC (little endian).
fn block(frame: &mut Vec<u8>, id: u8, data: &[u8]) {
    frame.push(id);
    frame.push(data.len() as u8);
    frame.extend_from_slice(data);
    frame.extend_from_slice(&crc16_ccitt(data).to_le_bytes());
}

/// WGS84 geodetic to ECEF, metres.
fn lla_to_ecef(lat: f64, lon: f64, alt: f64) -> [f64; 3] {
    let e2 = WGS84_F * (2.0 - WGS84_F);
    let (la, lo) = (lat.to_radians(), lon.to_radians());
    let n = WGS84_A / (1.0 - e2 * la.sin().powi(2)).sqrt();
    [
        (n + alt) * la.cos() * lo.cos(),
        (n + alt) * la.cos() * lo.sin(),
        (n * (1.0 - e2) + alt) * la.sin(),
    ]
}

/// One regular RS41 frame for `p`, scrambled as sent (320 bytes).
pub fn rs41_frame(p: &Rs41Params) -> Vec<u8> {
    let mut f = Vec::with_capacity(FRAME_LEN);
    f.extend_from_slice(&HEADER);
    f.resize(FRAME_TYPE_AT, 0);
    f.push(0x0F);

    let mut status = [0u8; 40];
    status[0..2].copy_from_slice(&(p.frame_no as u16).to_le_bytes());
    let serial = p.serial.as_bytes();
    status[2..2 + serial.len().min(8)].copy_from_slice(&serial[..serial.len().min(8)]);
    status[0x0A] = 28; // 2.8 V
    status[0x0D] = 0x01; // flight mode, ascending
    status[0x10] = 20; // reference area 20 C
    status[0x15] = 7; // full transmit power
    status[0x16] = (SUBFRAMES - 1) as u8;
    status[0x17] = (p.frame_no % SUBFRAMES) as u8;
    block(&mut f, 0x79, &status);

    block(&mut f, 0x7A, &[0u8; 42]);

    let mut info = [0u8; 30];
    // GPS week 2390 (2025-10-26) and a time of week a frame per second on.
    info[0..2].copy_from_slice(&2390u16.to_le_bytes());
    let tow_ms = (100_000 + p.frame_no) * 1000;
    info[2..6].copy_from_slice(&tow_ms.to_le_bytes());
    for (slot, prn) in [1u8, 3, 7, 11, 14, 17, 19, 22].iter().enumerate() {
        info[6 + 2 * slot] = *prn;
        info[7 + 2 * slot] = 0xF4; // mesQI 7, 40 dBHz
    }
    block(&mut f, 0x7C, &info);

    block(&mut f, 0x7D, &[0u8; 89]);

    let mut pos = [0u8; 21];
    let xyz = lla_to_ecef(p.lat, p.lon, p.alt_m);
    let (la, lo) = (p.lat.to_radians(), p.lon.to_radians());
    let up = [la.cos() * lo.cos(), la.cos() * lo.sin(), la.sin()];
    for k in 0..3 {
        let cm = (xyz[k] * 100.0).round() as i32;
        pos[4 * k..4 * k + 4].copy_from_slice(&cm.to_le_bytes());
        let v = (p.vel_v * up[k] * 100.0).round() as i16;
        pos[12 + 2 * k..14 + 2 * k].copy_from_slice(&v.to_le_bytes());
    }
    pos[0x12] = 8; // satellites in the solution
    pos[0x13] = 1; // speed accuracy 10 cm/s
    pos[0x14] = 12; // pDOP 1.2
    block(&mut f, 0x7B, &pos);

    let empty = FRAME_LEN - f.len() - 4;
    block(&mut f, 0x76, &vec![0u8; empty]);
    debug_assert_eq!(f.len(), FRAME_LEN);

    let rs = Rs255_231::new();
    for half in 0..2 {
        let mut data = [0u8; 231];
        for (i, d) in data.iter_mut().take(132).enumerate() {
            *d = f[FRAME_TYPE_AT + 2 * i + half];
        }
        f[8 + 24 * half..32 + 24 * half].copy_from_slice(&rs.encode(&data));
    }

    for (i, b) in f.iter_mut().enumerate() {
        *b ^= MASK[i % MASK.len()];
    }
    f
}

/// The bits sent for one frame: the preamble, then each byte of [`rs41_frame`] least
/// significant bit first.
pub fn rs41_frame_bits(p: &Rs41Params) -> Vec<u8> {
    let mut bits: Vec<u8> = (0..PREAMBLE_BITS).map(|i| (i % 2) as u8).collect();
    for b in rs41_frame(p) {
        bits.extend((0..8).map(|k| (b >> k) & 1));
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Rs41Params {
        Rs41Params {
            serial: "S1234567".into(),
            lat: -27.4,
            lon: 153.1,
            alt_m: 12_000.0,
            vel_v: 5.0,
            frame_no: 7,
        }
    }

    /// The frame with the scrambling mask removed.
    fn descrambled(p: &Rs41Params) -> Vec<u8> {
        rs41_frame(p)
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ MASK[i % MASK.len()])
            .collect()
    }

    /// The codeword evaluated at `alpha^k`, the RS41 order (parity at x^0..x^23, data
    /// byte i at x^(24+i)).
    fn syndrome(data: &[u8; 231], parity: &[u8; 24], k: usize) -> u8 {
        let gf = Gf::new();
        let r = gf.exp[k];
        let mut s = 0u8;
        for &c in data.iter().rev().chain(parity.iter().rev()) {
            s = gf.mul(s, r) ^ c;
        }
        s
    }

    #[test]
    fn crc16_matches_a_known_vector() {
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
    }

    #[test]
    fn rs_parity_corrects_itself() {
        let rs = Rs255_231::new();
        let mut data = [0u8; 231];
        for (i, d) in data.iter_mut().enumerate() {
            *d = (i * 37 + 11) as u8;
        }
        let parity = rs.encode(&data);
        // A codeword: every syndrome is zero, and encoding again reproduces the parity.
        assert!((0..24).all(|k| syndrome(&data, &parity, k) == 0));
        assert_eq!(rs.encode(&data), parity);
        // Twelve flipped bytes (the code's correction capacity) change the parity and
        // leave nonzero syndromes against the old one.
        let mut bad = data;
        for i in 0..12 {
            bad[i * 19] ^= 0x5A;
        }
        assert_ne!(rs.encode(&bad), parity);
        assert!((0..24).any(|k| syndrome(&bad, &parity, k) != 0));
    }

    #[test]
    fn frame_has_the_public_header_and_length() {
        let p = params();
        let raw = rs41_frame(&p);
        assert_eq!(raw.len(), 320);
        // As sent (scrambled), and with the mask removed.
        assert_eq!(raw[..8], [0x10, 0xB6, 0xCA, 0x11, 0x22, 0x96, 0x12, 0xF8]);
        let f = descrambled(&p);
        assert_eq!(f[..8], HEADER);
        assert_eq!(f[0x38], 0x0F);
        // The blocks fill the frame exactly, each with its CRC.
        let mut pos = 0x39;
        let mut ids = Vec::new();
        while pos < 320 {
            let (id, len) = (f[pos], usize::from(f[pos + 1]));
            let data = &f[pos + 2..pos + 2 + len];
            let crc = u16::from_le_bytes([f[pos + 2 + len], f[pos + 3 + len]]);
            assert_eq!(crc, crc16_ccitt(data), "block {id:#x}");
            ids.push(id);
            pos += len + 4;
        }
        assert_eq!(pos, 320);
        assert_eq!(ids, [0x79, 0x7A, 0x7C, 0x7D, 0x7B, 0x76]);
        // The serial and the frame number in STATUS.
        assert_eq!(u16::from_le_bytes([f[0x3B], f[0x3C]]), 7);
        assert_eq!(&f[0x3D..0x45], b"S1234567");
        // Both interleaved codewords carry their parity.
        let rs = Rs255_231::new();
        for half in 0..2 {
            let mut data = [0u8; 231];
            for (i, d) in data.iter_mut().take(132).enumerate() {
                *d = f[0x38 + 2 * i + half];
            }
            assert_eq!(rs.encode(&data)[..], f[8 + 24 * half..32 + 24 * half]);
        }
    }

    #[test]
    fn gpspos_block_round_trips() {
        let p = params();
        let f = descrambled(&p);
        let at = 0x112;
        assert_eq!(f[at], 0x7B);
        let b = &f[at + 2..at + 2 + 21];
        let i32_at = |o: usize| f64::from(i32::from_le_bytes(b[o..o + 4].try_into().unwrap()));
        let i16_at = |o: usize| f64::from(i16::from_le_bytes(b[o..o + 2].try_into().unwrap()));
        let (x, y, z) = (i32_at(0) / 100.0, i32_at(4) / 100.0, i32_at(8) / 100.0);
        let (lat, lon, alt) = ecef_to_lla(x, y, z);
        // 1 m is about 9e-6 degrees.
        assert!((lat - p.lat).abs() < 9e-6, "{lat}");
        assert!((lon - p.lon).abs() < 9e-6, "{lon}");
        assert!((alt - p.alt_m).abs() < 1.0, "{alt}");
        // The velocity is the climb along the local vertical.
        let (vx, vy, vz) = (i16_at(12) / 100.0, i16_at(14) / 100.0, i16_at(16) / 100.0);
        let (la, lo) = (lat.to_radians(), lon.to_radians());
        let up = vx * la.cos() * lo.cos() + vy * la.cos() * lo.sin() + vz * la.sin();
        assert!((up - p.vel_v).abs() < 0.03, "{up}");
    }

    /// WGS84 ECEF to geodetic (Bowring's method, iterated).
    fn ecef_to_lla(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
        let e2 = WGS84_F * (2.0 - WGS84_F);
        let p = x.hypot(y);
        let lon = y.atan2(x);
        let mut lat = z.atan2(p * (1.0 - e2));
        let mut alt = 0.0;
        for _ in 0..10 {
            let n = WGS84_A / (1.0 - e2 * lat.sin().powi(2)).sqrt();
            alt = p / lat.cos() - n;
            lat = z.atan2(p * (1.0 - e2 * n / (n + alt)));
        }
        (lat.to_degrees(), lon.to_degrees(), alt)
    }
}
