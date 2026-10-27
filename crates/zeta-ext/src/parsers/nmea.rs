//! `nmea`: AIS sentences (`!AIVDM` received, `!AIVDO` own ship), as AIS decoders
//! such as AIS-catcher and rtl-ais print them:
//! `!AIVDM,<fragments>,<fragment>,<message id>,<channel>,<payload>,<fill bits>*<checksum>`.
//!
//! Each sentence is one event, `{channel, payload, fill_bits, msg_type, mmsi}`:
//! the payload's 6-bit armouring is decoded far enough for the message type
//! (bits 0-5) and the MMSI (bits 8-37), `null` when the payload is too short
//! (a later fragment). Full AIS fields arrive in plan 17.

use serde_json::{Value, json};

/// The fields of one AIS sentence; `None` for any other line, or a sentence
/// whose checksum does not match.
pub fn parse(line: &str) -> Option<Value> {
    let line = line.trim();
    let body = line.strip_prefix('!')?;
    let (body, checksum) = match body.split_once('*') {
        Some((b, c)) => (b, Some(c)),
        None => (body, None),
    };
    if let Some(c) = checksum {
        let want = u8::from_str_radix(c.get(..2)?, 16).ok()?;
        if body.bytes().fold(0u8, |a, b| a ^ b) != want {
            return None;
        }
    }
    let f: Vec<&str> = body.split(',').collect();
    if !matches!(f.first(), Some(&"AIVDM" | &"AIVDO")) || f.len() != 7 {
        return None;
    }
    let payload = f[5];
    let fill_bits: u8 = f[6].parse().ok().filter(|n| *n <= 5)?;
    let bits = unarmour(payload)?;
    let field = |from: usize, len: usize| {
        (bits.len() >= from + len).then(|| {
            bits[from..from + len]
                .iter()
                .fold(0u64, |a, &b| (a << 1) | u64::from(b))
        })
    };
    let channel = (!f[4].is_empty()).then_some(f[4]);
    Some(json!({
        "channel": channel,
        "payload": payload,
        "fill_bits": fill_bits,
        "msg_type": field(0, 6),
        "mmsi": field(8, 30),
    }))
}

/// The payload's bits, six per character (`0`-`W` and `` ` ``-`w`); `None`
/// for a character outside the armouring.
fn unarmour(payload: &str) -> Option<Vec<u8>> {
    let mut bits = Vec::with_capacity(payload.len() * 6);
    for c in payload.bytes() {
        let v = match c {
            b'0'..=b'W' => c - b'0',
            b'`'..=b'w' => c - b'0' - 8,
            _ => return None,
        };
        bits.extend((0..6).rev().map(|i| (v >> i) & 1));
    }
    Some(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_report_gives_type_and_mmsi() {
        let v = parse("!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C").unwrap();
        assert_eq!(v["channel"], "B");
        assert_eq!(v["payload"], "177KQJ5000G?tO`K>RA1wUbN0TKH");
        assert_eq!(v["fill_bits"], 0);
        assert_eq!(v["msg_type"], 1);
        assert_eq!(v["mmsi"], 477_553_000);
    }

    #[test]
    fn bad_checksum_and_other_sentences_are_unread() {
        assert!(parse("!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5D").is_none());
        assert!(
            parse("$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47").is_none()
        );
        assert!(parse("hello").is_none());
        // A short later fragment: too few bits for an MMSI.
        let v = parse("!AIVDM,2,2,3,A,8888,2").unwrap();
        assert_eq!(v["mmsi"], Value::Null);
        assert_eq!(v["fill_bits"], 2);
    }
}
