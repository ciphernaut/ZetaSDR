//! Meteomodem M20 radiosonde frame generator: builds the 70-byte frame an M20 sends once
//! a second, for the `m20` synthesiser family (`zeta modes check m20_decode`).
//!
//! Written from public descriptions, not from any decoder's source: Meteomodem's M20
//! data sheet (FSK, a 9600 bit/s line, one transmission a second; the copy at
//! <https://www.klofas.com/blog/2024/keflavik-iceland-radiosonde-receiver/M20%20EN.pdf>),
//! the radiosonde_auto_rx wiki (the M10/M20 frames carry no frame counter, so a decoder
//! numbers them by their GPS time; the JSON field list at
//! <https://github.com/projecthorus/radiosonde_auto_rx/wiki/RS----Auto_RX-JSON-Formats>)
//! and the SondeHub serial-number notes
//! (<https://github.com/projecthorus/sondehub-infra/wiki/Radiosonde-Serial-Numbers>).
//! Those descriptions stop at the line rate, so nearly everything below was settled
//! against `m20mod` (sonde-decoders 1.8.1), run only as a black-box oracle: its
//! `--rawhex` mode reads frames as hex text and `-r -v` prints the check value it
//! computes, so the frame layout and the check were measured byte by byte, and the line
//! coding and the header were found by feeding it audio and reading what it decoded.
//!
//! - **Line**: 2-FSK at [`BAUD`] (9600) half-symbols a second, Manchester coded (4800
//!   bit/s); the data sheet's 9600 is the half-symbol rate, and the oracle reports 5
//!   samples a symbol at 48 kHz. Each data bit is
//!   differentially coded before the Manchester step: a 1 keeps the level of the bit
//!   before, a 0 inverts it (oracle). Because only changes carry data, the Manchester
//!   phase and the FSK polarity do not matter, and the oracle decodes either.
//! - **Header**: the 32 half-symbols [`HEADER`] right before the frame (oracle). Found by
//!   feeding the oracle random half-symbols until it decoded a frame, then flipping each
//!   half-symbol before that frame in turn: 30 of the 32 lose the frame and two are
//!   tolerated either way; this module sends one of the accepted values for those two.
//!   Between frames the line idles on alternating half-symbols.
//! - **Frame**: [`FRAME_LEN`] bytes, most significant bit first. Byte 0 is the length less
//!   one (`0x45`) and byte 1 the type (`0x20`, which the oracle reports as `subtype`).
//!   Measured fields (all big endian): the altitude in cm (u24 at [`ALT`]), the east and
//!   north speeds in cm/s (i16 at [`VEL_E`] and [`VEL_N`]), the GPS time of week in
//!   seconds (u24 at [`TOW`]), the serial (three bytes at [`SERIAL`]), the climb in cm/s
//!   (i16 at [`VEL_V`]), the GPS week (u16 at [`WEEK`]) and the latitude and longitude in
//!   1e-6 degree (i32 at [`LAT`] and [`LON`]). Bytes 22-23 hold a second check the oracle
//!   reports as unset (`(oo)`) while they are zero; it does not gate the decode, so they
//!   stay zero, like the rest of the frame (sensor and auxiliary data).
//! - **Check**: the last two bytes, big endian, over the first 68 ([`crc16_m20`]). It is
//!   not a CRC: one byte at a time, the high byte of the 16-bit register takes the old
//!   low byte and the low byte becomes a GF(2)-linear function of the old register and
//!   the input byte, from a zero register (oracle: 300 random frames all agree). The
//!   function's columns ([`CHECK_BYTE_COLS`], [`CHECK_REG_COLS`]) were measured by
//!   flipping single bits of the last three bytes and reading the check the oracle printed.
//! - **Serial**: `M20-YMM-S-NNNNN` as the oracle prints the id (`M20-` and the three
//!   parts). The first serial byte's low seven bits are `Y * 12 + MM - 1` (a year digit and
//!   a month); the next three bits (the first byte's top bit, then the second byte's two
//!   low bits) are `S - 1`; the remaining 14 bits (the second byte's top six, then the
//!   third byte) are `NNNNN`. The oracle prints the last part as one digit of `N / 8192`
//!   and four of `N % 8192`, so `NNNNN` is kept to 0..=8191, where both readings agree.
//! - **Time**: the oracle prints a JSON line only for a frame whose time differs from the
//!   one before, so frame `n` carries [`TOW_START`] + `n` seconds in GPS week [`GPS_WEEK`]
//!   (2026-11-01 12:00:00 GPS time onwards).

use crate::dfm_frame::manchester_encode;

/// Manchester half-symbols a second.
pub const BAUD: u32 = 9600;
/// The FSK deviation, Hz. No description gives one; this value fits the row's 12.5 kHz
/// channel and its 5 kHz NBFM demodulator, and the oracle decodes it through that chain.
pub const DEVIATION_HZ: f64 = 3600.0;
/// The header half-symbols as sent, first in the most significant bit.
pub const HEADER: u32 = 0x6666_B366;
/// Half-symbols in the header.
pub const HEADER_CHIPS: usize = 32;
/// Bytes in a frame, the check included.
pub const FRAME_LEN: usize = 70;
/// Half-symbols in a frame as sent: the header, then two per frame bit.
pub const FRAME_CHIPS: usize = HEADER_CHIPS + 2 * 8 * FRAME_LEN;
/// Byte 0: the frame length less one.
pub const LENGTH: u8 = (FRAME_LEN - 1) as u8;
/// Byte 1: the frame type.
pub const FRAME_TYPE: u8 = 0x20;
/// Field offsets (bytes, big endian).
pub const ALT: usize = 8;
pub const VEL_E: usize = 11;
pub const VEL_N: usize = 13;
pub const TOW: usize = 15;
pub const SERIAL: usize = 18;
pub const VEL_V: usize = 24;
pub const WEEK: usize = 26;
pub const LAT: usize = 28;
pub const LON: usize = 32;
/// Where the check sits: the two bytes after the data it covers.
pub const CHECK: usize = FRAME_LEN - 2;
/// The check's input-byte term: the low byte's image of each input bit, bit 0 first.
pub const CHECK_BYTE_COLS: [u8; 8] = [0xA0, 0x01, 0x02, 0x05, 0x0A, 0x14, 0x28, 0x50];
/// The check's register term: the low byte's image of each register bit, bit 0 first.
pub const CHECK_REG_COLS: [u8; 16] = [
    0x41, 0x82, 0x44, 0x88, 0x50, 0xA0, 0x00, 0x01, 0x02, 0x05, 0x0A, 0x14, 0x28, 0x50, 0xA0, 0x00,
];
/// GPS week of the synthesised flight: the one starting Sunday 2026-11-01.
pub const GPS_WEEK: u16 = 2443;
/// GPS seconds of the week of frame 0: Sunday 12:00:00.
pub const TOW_START: u32 = 43_200;
/// The horizontal speed the generator reports: 5 m/s towards the east.
const VEL_E_CM_S: i16 = 500;
/// Months a year in the serial's first part.
const MONTHS: u8 = 12;
/// The largest serial number whose two printed readings agree (see the module comment).
pub const SERIAL_NUMBER_MAX: u16 = 8191;
/// Largest `S` in the serial: three bits hold `S - 1`.
pub const SERIAL_S_MAX: u8 = 8;

/// What a frame carries: the serial as `m20mod` prints the id (`M20-YMM-S-NNNNN`), the
/// position (degrees, metres), the climb in m/s and the frame's index in the
/// transmission (its GPS time, a second a frame).
#[derive(Clone, Debug, PartialEq)]
pub struct M20Params {
    pub serial: String,
    pub lat: f64,
    pub lon: f64,
    pub alt_m: f64,
    pub vel_v: f64,
    pub frame: u32,
}

/// The serial's parts `(Y, MM, S, NNNNN)` from `M20-YMM-S-NNNNN`, or `None` when it is
/// not that shape or a part is out of range (Y 0-9, MM 01-12, S 1-8, NNNNN 0-8191).
pub fn serial_parts(serial: &str) -> Option<(u8, u8, u8, u16)> {
    let rest = serial.strip_prefix("M20-")?;
    let mut parts = rest.split('-');
    let (ymm, s, n) = (parts.next()?, parts.next()?, parts.next()?);
    let digits = |t: &str, len: usize| t.len() == len && t.bytes().all(|c| c.is_ascii_digit());
    if parts.next().is_some() || !digits(ymm, 3) || !digits(s, 1) || !digits(n, 5) {
        return None;
    }
    let y = ymm[..1].parse::<u8>().ok()?;
    let mm = ymm[1..].parse::<u8>().ok()?;
    let s = s.parse::<u8>().ok()?;
    let n = n.parse::<u16>().ok()?;
    ((1..=MONTHS).contains(&mm) && (1..=SERIAL_S_MAX).contains(&s) && n <= SERIAL_NUMBER_MAX)
        .then_some((y, mm, s, n))
}

/// The three serial bytes for `M20-YMM-S-NNNNN` (zeros when the serial does not parse).
pub fn serial_bytes(serial: &str) -> [u8; 3] {
    let Some((y, mm, s, n)) = serial_parts(serial) else {
        return [0; 3];
    };
    let s = s - 1;
    [
        (y * MONTHS + mm - 1) | ((s & 1) << 7),
        (s >> 1) | (((n & 0x3F) as u8) << 2),
        (n >> 6) as u8,
    ]
}

/// The M20 frame check over `data` (the frame's first 68 bytes): see the module comment.
pub fn crc16_m20(data: &[u8]) -> u16 {
    let image = |cols: &[u8], x: u32| {
        cols.iter()
            .enumerate()
            .filter(|(i, _)| (x >> i) & 1 == 1)
            .fold(0u8, |acc, (_, c)| acc ^ c)
    };
    data.iter().fold(0u16, |reg, &b| {
        let low = image(&CHECK_REG_COLS, u32::from(reg)) ^ image(&CHECK_BYTE_COLS, u32::from(b));
        (reg << 8) | u16::from(low)
    })
}

/// Writes `v`'s low `n` bytes big endian at `at`.
fn put(f: &mut [u8], at: usize, n: usize, v: u32) {
    for (k, byte) in f[at..at + n].iter_mut().enumerate() {
        *byte = (v >> (8 * (n - 1 - k))) as u8;
    }
}

/// One frame's 70 bytes, the check included.
pub fn m20_frame(p: &M20Params) -> [u8; FRAME_LEN] {
    let mut f = [0u8; FRAME_LEN];
    f[0] = LENGTH;
    f[1] = FRAME_TYPE;
    put(&mut f, ALT, 3, (p.alt_m * 100.0).round().max(0.0) as u32);
    put(&mut f, VEL_E, 2, u32::from(VEL_E_CM_S as u16));
    put(&mut f, VEL_N, 2, 0);
    put(&mut f, TOW, 3, TOW_START + p.frame);
    f[SERIAL..SERIAL + 3].copy_from_slice(&serial_bytes(&p.serial));
    put(
        &mut f,
        VEL_V,
        2,
        u32::from((p.vel_v * 100.0).round() as i16 as u16),
    );
    put(&mut f, WEEK, 2, u32::from(GPS_WEEK));
    put(&mut f, LAT, 4, (p.lat * 1e6).round() as i32 as u32);
    put(&mut f, LON, 4, (p.lon * 1e6).round() as i32 as u32);
    let check = crc16_m20(&f[..CHECK]);
    put(&mut f, CHECK, 2, u32::from(check));
    f
}

/// One frame as sent: the header half-symbols, then the frame differentially and
/// Manchester coded ([`FRAME_CHIPS`] half-symbols, 120 ms at [`BAUD`]).
pub fn m20_frame_bits(p: &M20Params) -> Vec<u8> {
    let mut chips: Vec<u8> = (0..HEADER_CHIPS)
        .map(|k| ((HEADER >> (HEADER_CHIPS - 1 - k)) & 1) as u8)
        .collect();
    let mut level = 0u8;
    let levels: Vec<u8> = m20_frame(p)
        .iter()
        .flat_map(|&byte| (0..8).map(move |k| (byte >> (7 - k)) & 1))
        .map(|bit| {
            level ^= bit ^ 1;
            level
        })
        .collect();
    chips.extend(manchester_encode(&levels));
    chips
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(frame: u32) -> M20Params {
        M20Params {
            serial: "M20-611-2-01234".into(),
            lat: -27.3842,
            lon: 153.1175,
            alt_m: 5000.0,
            vel_v: 4.5,
            frame,
        }
    }

    /// The test's own line decoder: Manchester pairs to levels, then a bit is 1 where the
    /// level did not change from the bit before.
    fn line_decode(chips: &[u8], before: u8) -> Vec<u8> {
        let levels: Vec<u8> = chips
            .chunks(2)
            .map(|c| match c {
                [0, 1] => 1,
                [1, 0] => 0,
                _ => panic!("not Manchester: {c:?}"),
            })
            .collect();
        let mut prev = before;
        let bits: Vec<u8> = levels
            .iter()
            .map(|&l| {
                let b = u8::from(l == prev);
                prev = l;
                b
            })
            .collect();
        bits.chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
            .collect()
    }

    fn be(f: &[u8], at: usize, n: usize) -> i64 {
        f[at..at + n]
            .iter()
            .fold(0i64, |a, &b| (a << 8) | i64::from(b))
    }

    #[test]
    fn a_frame_has_the_documented_layout() {
        let p = params(3);
        let chips = m20_frame_bits(&p);
        assert_eq!(chips.len(), FRAME_CHIPS);
        let header: Vec<u8> = (0..HEADER_CHIPS)
            .map(|k| ((HEADER >> (HEADER_CHIPS - 1 - k)) & 1) as u8)
            .collect();
        assert_eq!(&chips[..HEADER_CHIPS], &header[..]);
        let f = line_decode(&chips[HEADER_CHIPS..], 0);
        assert_eq!(f, m20_frame(&p).to_vec());
        assert_eq!((f[0], f[1]), (0x45, 0x20));
        assert_eq!(be(&f, ALT, 3), 500_000);
        assert_eq!(be(&f, VEL_E, 2), 500);
        assert_eq!(be(&f, VEL_N, 2), 0);
        assert_eq!(be(&f, TOW, 3), i64::from(TOW_START + 3));
        assert_eq!(&f[SERIAL..SERIAL + 3], &serial_bytes(&p.serial));
        assert_eq!(be(&f, VEL_V, 2), 450);
        assert_eq!(be(&f, WEEK, 2), i64::from(GPS_WEEK));
        assert_eq!(be(&f, LAT, 4) as i32, -27_384_200);
        assert_eq!(be(&f, LON, 4) as i32, 153_117_500);
        assert_eq!(be(&f, CHECK, 2) as u16, crc16_m20(&f[..CHECK]));
    }

    /// The check against values `m20mod -r -v --rawhex` printed for these frames (the
    /// in-tree table; the decoder is not run here).
    #[test]
    fn the_crc_matches_the_in_tree_table() {
        let frame = |fill: &dyn Fn(usize) -> u8| -> Vec<u8> {
            let mut f = vec![LENGTH, FRAME_TYPE];
            f.extend((2..CHECK).map(fill));
            f
        };
        let table: [(Vec<u8>, u16); 4] = [
            (frame(&|_| 0), 0xBC8B),
            (frame(&|i| i as u8), 0x91B6),
            (frame(&|_| 0xFF), 0x1148),
            (frame(&|i| (i * 37 + 11) as u8), 0x432B),
        ];
        for (data, check) in table {
            assert_eq!(crc16_m20(&data), check, "{:02x?}", &data[..6]);
        }
    }

    #[test]
    fn the_serial_is_coded_as_the_oracle_reads_it() {
        assert_eq!(serial_parts("M20-611-2-01234"), Some((6, 11, 2, 1234)));
        // M20-105-3-04296 was read back from these bytes by the oracle.
        assert_eq!(serial_bytes("M20-105-3-04296"), [0x10, 0x21, 0x43]);
        for bad in [
            "M20-6112-01234",
            "M20-600-2-01234",
            "M20-613-2-01234",
            "M20-611-0-01234",
            "M20-611-9-01234",
            "M20-611-2-08192",
            "M10-611-2-01234",
            "M20-611-2-1234",
        ] {
            assert_eq!(serial_parts(bad), None, "{bad}");
        }
    }

    #[test]
    fn frames_count_gps_seconds() {
        let t = |n| be(&m20_frame(&params(n)), TOW, 3);
        assert_eq!(t(1) - t(0), 1);
        assert_eq!(t(59) - t(0), 59);
    }
}
