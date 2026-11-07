//! DNP3 link layer: `05 64 LEN CTRL DEST SRC CRC`, then user data in blocks of up to
//! 16 bytes, each followed by its CRC-16/DNP (low byte first). `LEN` counts the
//! control, destination and source octets (5) and the user data, not the CRCs.

use serde_json::{Value, json};

use super::hex;
use crate::crc::{CRC16_DNP, crc};

const START: [u8; 2] = [0x05, 0x64];
/// Start, length, control, destination and source: the header before its CRC.
const HEADER_LEN: usize = 8;
const CRC_LEN: usize = 2;
/// `LEN` counts the control, destination and source octets besides the user data.
const LEN_OVERHEAD: usize = 5;
const BLOCK: usize = 16;
const DIR: u8 = 0x80;
const PRM: u8 = 0x40;
const FUNCTION_MASK: u8 = 0x0F;

fn crc_matches(data: &[u8], tail: &[u8]) -> bool {
    crc(&CRC16_DNP, data) as u16 == u16::from_le_bytes([tail[0], tail[1]])
}

pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() < HEADER_LEN + CRC_LEN {
        return Err("too short".into());
    }
    if bytes[..2] != START {
        return Err("start".into());
    }
    let length = bytes[2] as usize;
    if length < LEN_OVERHEAD {
        return Err("length".into());
    }
    let (header, rest) = bytes.split_at(HEADER_LEN);
    if !crc_matches(header, rest) {
        return Err("crc".into());
    }
    let mut rest = &rest[CRC_LEN..];
    let data_len = length - LEN_OVERHEAD;
    let blocks = data_len.div_ceil(BLOCK);
    if rest.len() != data_len + blocks * CRC_LEN {
        return Err("length".into());
    }
    let mut data = Vec::with_capacity(data_len);
    let mut left = data_len;
    while left > 0 {
        let n = left.min(BLOCK);
        let (block, tail) = rest.split_at(n);
        if !crc_matches(block, tail) {
            return Err("crc".into());
        }
        data.extend_from_slice(block);
        rest = &tail[CRC_LEN..];
        left -= n;
    }
    let control = header[3];
    let function = control & FUNCTION_MASK;
    let destination = u16::from_le_bytes([header[4], header[5]]);
    let source = u16::from_le_bytes([header[6], header[7]]);
    Ok(json!({
        "length": length,
        "control": {"dir": control & DIR != 0, "prm": control & PRM != 0, "function": function},
        "destination": destination,
        "source": source,
        "user_data": hex(&data),
        "summary": format!("dnp3 {source} -> {destination} fn {function}"),
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::parse;
    use crate::parsers::vectors::{dnp3, dnp3_frame};

    #[test]
    fn dnp3_parses_header_and_blocks_and_rejects_a_bad_block_crc() {
        let f = parse(&dnp3()).unwrap();
        assert_eq!(f["length"], 11);
        assert_eq!(
            f["control"],
            json!({"dir": true, "prm": true, "function": 4})
        );
        assert_eq!(f["destination"], 1);
        assert_eq!(f["source"], 0);
        assert_eq!(f["user_data"], "c0c1013c0206");
        assert_eq!(f["summary"], "dnp3 0 -> 1 fn 4");

        // Two blocks: 16 bytes and 4.
        let data: Vec<u8> = (0..20).collect();
        let two = dnp3_frame(0x44, 7, 9, &data);
        let g = parse(&two).unwrap();
        assert_eq!(g["length"], 25);
        assert_eq!(g["user_data"].as_str().unwrap().len(), 40);

        let mut bad = two.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert_eq!(parse(&bad), Err("crc".to_string()));
        let mut bad = two.clone();
        bad[10 + 3] ^= 1; // inside the first block
        assert_eq!(parse(&bad), Err("crc".to_string()));
        let mut bad = two;
        bad[3] ^= 1; // the header
        assert_eq!(parse(&bad), Err("crc".to_string()));
    }

    #[test]
    fn a_length_that_disagrees_with_the_bytes_is_refused() {
        let mut v = dnp3();
        v.pop();
        assert!(parse(&v).is_err());
    }
}
