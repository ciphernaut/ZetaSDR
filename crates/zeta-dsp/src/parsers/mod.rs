//! Pure bytes-to-fields decoders of clear, standards-based link framing, each behind
//! its protocol's own check. Nothing here decrypts, targets a link or transmits.

use serde_json::Value;

pub mod dnp3;
pub mod iec101;
pub mod iec104;
pub mod modbus_rtu;

/// Lower-case hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The fields of `bytes` read by the parser `name`, or why it refused them.
pub fn parse(name: &str, bytes: &[u8]) -> Result<Value, String> {
    match name {
        "modbus_rtu" => modbus_rtu::parse(bytes),
        "dnp3" => dnp3::parse(bytes),
        "iec101" => iec101::parse(bytes),
        "iec104" => iec104::parse(bytes),
        other => Err(format!("unknown parser {other}")),
    }
}

/// The vectors every parser's tests share, built so no check value is typed by hand
/// (the Modbus frame's CRC bytes are the standard's own example, checked in its test).
#[cfg(test)]
pub(crate) mod vectors {
    use crate::crc::{CRC16_DNP, crc};

    pub fn modbus() -> Vec<u8> {
        vec![0x01, 0x03, 0x00, 0x00, 0x00, 0x0A, 0xC5, 0xCD]
    }

    /// A DNP3 frame: header from `ctrl`, `dest`, `src`, then `data` in 16-byte blocks.
    pub fn dnp3_frame(ctrl: u8, dest: u16, src: u16, data: &[u8]) -> Vec<u8> {
        let mut h = vec![0x05, 0x64, (5 + data.len()) as u8, ctrl];
        h.extend_from_slice(&dest.to_le_bytes());
        h.extend_from_slice(&src.to_le_bytes());
        let mut out = h.clone();
        out.extend_from_slice(&(crc(&CRC16_DNP, &h) as u16).to_le_bytes());
        for block in data.chunks(16) {
            out.extend_from_slice(block);
            out.extend_from_slice(&(crc(&CRC16_DNP, block) as u16).to_le_bytes());
        }
        out
    }

    pub fn dnp3() -> Vec<u8> {
        dnp3_frame(0xC4, 1, 0, &[0xC0, 0xC1, 0x01, 0x3C, 0x02, 0x06])
    }

    pub fn iec101() -> Vec<u8> {
        let body = [0x73, 0x01, 0x64, 0x01, 0x06, 0x01, 0x00, 0x00, 0x14];
        let cs = body.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        let mut v = vec![0x68, body.len() as u8, body.len() as u8, 0x68];
        v.extend_from_slice(&body);
        v.extend_from_slice(&[cs, 0x16]);
        v
    }

    pub fn iec104() -> Vec<u8> {
        vec![
            0x68, 0x0E, 0x02, 0x00, 0x02, 0x00, 0x64, 0x01, 0x06, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x14,
        ]
    }

    #[test]
    fn no_parser_panics_on_any_prefix_of_any_vector() {
        let all = [
            ("modbus_rtu", modbus()),
            ("dnp3", dnp3()),
            ("iec101", iec101()),
            ("iec104", iec104()),
        ];
        for (name, v) in &all {
            for n in 0..=v.len() {
                let _ = super::parse(name, &v[..n]);
            }
            // Every parser also meets every other's vectors.
            for (_, w) in &all {
                let _ = super::parse(name, w);
            }
        }
        assert!(super::parse("x", &[]).is_err());
    }
}
