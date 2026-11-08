//! `nmea`: AIS sentences (`!AIVDM` received, `!AIVDO` own ship), as AIS decoders
//! such as AIS-catcher and rtl-ais print them:
//! `!AIVDM,<fragments>,<fragment>,<message id>,<channel>,<payload>,<fill bits>*<checksum>`.
//!
//! Each sentence is one event, `{channel, payload, fill_bits}` plus the AIS fields
//! [`AisMsg::fields`] gives (`mmsi`, `msg_type`, `repeat`, `lat`, `lon`, `sog_kt`,
//! `cog_deg`, `heading_deg`, `nav_status`, `name`, `callsign`, `destination`, `imo`,
//! `ship_type`, `aton_type`, `virtual_aton`), decoded by [`zeta_dsp::ais_msg`] as the
//! in-crate AIS stage decodes them. Each sentence is read alone: only a message's first
//! fragment is decoded, a field past its end is `null`, and every field is `null` for a
//! later fragment or a payload too short for an MMSI.

use serde_json::{Value, json};
use zeta_dsp::ais_msg::{self, AisMsg};

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
    let mut bits = ais_msg::unarmour(payload)?;
    bits.truncate(bits.len().saturating_sub(usize::from(fill_bits)));
    let channel = (!f[4].is_empty()).then_some(f[4]);
    // Only a message's first fragment starts with its header: a later one carries the
    // middle of a message, which read alone could pass for a vessel.
    let first = f[2] == "1";
    let fields = match ais_msg::decode(&bits).filter(|_| first) {
        Some(m) => m.fields(),
        None => AisMsg::default()
            .fields()
            .into_iter()
            .map(|(k, _)| (k, Value::Null))
            .collect(),
    };
    let mut v = json!({
        "channel": channel,
        "payload": payload,
        "fill_bits": fill_bits,
    });
    v.as_object_mut().expect("an object").extend(fields);
    Some(v)
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
        assert!(v["lat"].is_number(), "{v}");
        assert!(
            (v["lat"].as_f64().unwrap() - 47.582_833).abs() < 1e-6,
            "{v}"
        );
        assert_eq!(v["cog_deg"], 51.0);
        assert_eq!(v["name"], Value::Null);
    }

    #[test]
    fn a_later_fragment_is_not_decoded_as_a_message() {
        // The published type 1 payload sent as fragment 2 of 2: read alone it would decode
        // as a vessel, but a continuation carries the middle of another message.
        let body = "AIVDM,2,2,3,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0";
        let cs = body.bytes().fold(0u8, |a, b| a ^ b);
        let v = parse(&format!("!{body}*{cs:02X}")).unwrap();
        assert_eq!(v["payload"], "177KQJ5000G?tO`K>RA1wUbN0TKH");
        assert_eq!(v["mmsi"], Value::Null);
        assert_eq!(v["msg_type"], Value::Null);
        assert_eq!(v["lat"], Value::Null);
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
        assert_eq!(v["msg_type"], Value::Null);
        assert_eq!(v["lat"], Value::Null);
        assert_eq!(v["fill_bits"], 2);
    }
}
