//! IEC 60870-5-104 APCI and APDU: `68 LEN`, four control octets, then (I-format) an
//! ASDU whose header is type, variable structure qualifier, a two-octet cause of
//! transmission (the cause in its low six bits) and a two-octet common address.

use serde_json::{Value, json};

use super::hex;

const START: u8 = 0x68;
/// `68 LEN`.
const PREFIX: usize = 2;
const CONTROL_LEN: usize = 4;
/// Type, qualifier, two cause octets and two address octets.
const ASDU_HEADER: usize = 6;
const CAUSE_MASK: u8 = 0x3F;

pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() < PREFIX + CONTROL_LEN {
        return Err("too short".into());
    }
    if bytes[0] != START {
        return Err("start".into());
    }
    let len = bytes[1] as usize;
    if len < CONTROL_LEN || bytes.len() != PREFIX + len {
        return Err("length".into());
    }
    let c = &bytes[PREFIX..PREFIX + CONTROL_LEN];
    let seq = |lo: u8, hi: u8| u16::from_le_bytes([lo, hi]) >> 1;
    let asdu = &bytes[PREFIX + CONTROL_LEN..];
    if c[0] & 1 == 0 {
        if asdu.len() < ASDU_HEADER {
            return Err("asdu too short".into());
        }
        let type_id = asdu[0];
        Ok(json!({
            "format": "i",
            "send_seq": seq(c[0], c[1]),
            "recv_seq": seq(c[2], c[3]),
            "asdu": hex(asdu),
            "type_id": type_id,
            "cause": asdu[2] & CAUSE_MASK,
            "common_address": u16::from_le_bytes([asdu[4], asdu[5]]),
            "summary": format!("iec104 i type {type_id}"),
        }))
    } else if c[0] & 3 == 1 {
        if !asdu.is_empty() {
            return Err("length".into());
        }
        Ok(json!({
            "format": "s",
            "send_seq": null,
            "recv_seq": seq(c[2], c[3]),
            "asdu": "",
            "summary": "iec104 s type -",
        }))
    } else {
        if !asdu.is_empty() {
            return Err("length".into());
        }
        Ok(json!({
            "format": "u",
            "send_seq": null,
            "recv_seq": null,
            "asdu": "",
            "summary": "iec104 u type -",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::parsers::vectors::iec104;

    #[test]
    fn iec104_reads_the_apci_and_the_asdu_header() {
        let f = parse(&iec104()).unwrap();
        assert_eq!(f["format"], "i");
        assert_eq!(f["send_seq"], 1);
        assert_eq!(f["recv_seq"], 1);
        assert_eq!(f["type_id"], 100);
        assert_eq!(f["cause"], 6);
        assert_eq!(f["common_address"], 1);
        assert_eq!(f["asdu"], "64010600010000000014");
        assert_eq!(f["summary"], "iec104 i type 100");

        let s = parse(&[0x68, 0x04, 0x01, 0x00, 0x06, 0x00]).unwrap();
        assert_eq!(s["format"], "s");
        assert_eq!(s["recv_seq"], 3);
        let u = parse(&[0x68, 0x04, 0x07, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(u["format"], "u");
        assert!(parse(&[0x68, 0x05, 0x01, 0x00, 0x00, 0x00]).is_err());
        assert!(parse(&[0x69, 0x04, 0x01, 0x00, 0x00, 0x00]).is_err());
    }
}
