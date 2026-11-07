//! Modbus RTU: `address`, `function`, data, and a CRC-16/MODBUS (low byte first).

use serde_json::{Value, json};

use super::hex;
use crate::crc::{CRC16_MODBUS, crc};

/// Address, function and CRC: the shortest frame.
const MIN_LEN: usize = 4;
const EXCEPTION_BIT: u8 = 0x80;

pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() < MIN_LEN {
        return Err("too short".into());
    }
    let (body, tail) = bytes.split_at(bytes.len() - 2);
    if crc(&CRC16_MODBUS, body) as u16 != u16::from_le_bytes([tail[0], tail[1]]) {
        return Err("crc".into());
    }
    let (address, function) = (body[0], body[1]);
    Ok(json!({
        "address": address,
        "function": function,
        "data": hex(&body[2..]),
        "crc_ok": true,
        "exception": function & EXCEPTION_BIT != 0,
        "summary": format!("modbus {address} fn {function}"),
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::parse;
    use crate::crc::{CRC16_MODBUS, crc};
    use crate::parsers::vectors::modbus;

    #[test]
    fn modbus_parses_and_rejects_a_bad_crc() {
        let v = modbus();
        // The wire carries the CRC low byte first: C5 CD is 0xCDC5.
        assert_eq!(crc(&CRC16_MODBUS, &v[..6]), 0xCDC5);
        assert_eq!(
            parse(&v).unwrap(),
            json!({"address": 1, "function": 3, "data": "0000000a", "crc_ok": true,
                "exception": false, "summary": "modbus 1 fn 3"})
        );
        let mut bad = v.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(parse(&bad), Err("crc".to_string()));
        assert!(parse(&v[..3]).is_err());
    }

    #[test]
    fn an_exception_response_is_flagged() {
        let mut v = vec![0x01, 0x83, 0x02];
        v.extend_from_slice(&(crc(&CRC16_MODBUS, &v) as u16).to_le_bytes());
        let f = parse(&v).unwrap();
        assert_eq!(f["exception"], true);
        assert_eq!(f["function"], 0x83);
    }
}
