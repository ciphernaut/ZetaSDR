//! IEC 60870-5-101 FT1.2 variable-length frame:
//! `68 L L 68 C A ASDU... CS 16`, one-octet link address, `L` counting `C`, `A` and
//! the ASDU, `CS` their sum modulo 256.

use serde_json::{Value, json};

use super::hex;

const START: u8 = 0x68;
const END: u8 = 0x16;
/// `68 L L 68` before the user data.
const PREFIX: usize = 4;
/// The checksum and the end byte after it.
const SUFFIX: usize = 2;
/// Control and address: the least `L` can be.
const MIN_L: usize = 2;
const PRM: u8 = 0x40;
const FCB_ACD: u8 = 0x20;
const FCV_DFC: u8 = 0x10;
const FUNCTION_MASK: u8 = 0x0F;

pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() < PREFIX + MIN_L + SUFFIX {
        return Err("too short".into());
    }
    if bytes[0] != START || bytes[3] != START {
        return Err("start".into());
    }
    if bytes[1] != bytes[2] {
        return Err("length".into());
    }
    let l = bytes[1] as usize;
    if l < MIN_L || bytes.len() != PREFIX + l + SUFFIX {
        return Err("length".into());
    }
    let body = &bytes[PREFIX..PREFIX + l];
    let sum = body.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    if sum != bytes[PREFIX + l] {
        return Err("checksum".into());
    }
    if bytes[PREFIX + l + 1] != END {
        return Err("end".into());
    }
    let (control, address) = (body[0], body[1]);
    let function = control & FUNCTION_MASK;
    Ok(json!({
        "control": {
            "prm": control & PRM != 0,
            "fcb_acd": control & FCB_ACD != 0,
            "fcv_dfc": control & FCV_DFC != 0,
            "function": function,
        },
        "address": address,
        "asdu": hex(&body[2..]),
        "summary": format!("iec101 link {address} fn {function}"),
    }))
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::parsers::vectors::iec101;

    #[test]
    fn iec101_checks_the_sum_and_the_end_byte() {
        let v = iec101();
        let f = parse(&v).unwrap();
        assert_eq!(f["control"]["prm"], true);
        assert_eq!(f["control"]["fcb_acd"], true);
        assert_eq!(f["control"]["fcv_dfc"], true);
        assert_eq!(f["control"]["function"], 3);
        assert_eq!(f["address"], 1);
        assert_eq!(f["asdu"], "64010601000014");
        assert_eq!(f["summary"], "iec101 link 1 fn 3");

        let n = v.len();
        let mut bad = v.clone();
        bad[n - 2] ^= 1;
        assert_eq!(parse(&bad), Err("checksum".to_string()));
        let mut bad = v.clone();
        bad[n - 1] = 0x17;
        assert_eq!(parse(&bad), Err("end".to_string()));
        let mut bad = v.clone();
        bad[2] += 1;
        assert!(parse(&bad).is_err());
        let mut bad = v;
        bad[3] = 0x69;
        assert!(parse(&bad).is_err());
    }
}
