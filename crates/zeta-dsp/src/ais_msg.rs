//! AIS messages: the payload's bit fields read into [`AisMsg`], and the payload written
//! back out as `!AIVDM` sentences and on-air HDLC bytes.
//!
//! Sources: Eric S. Raymond, "AIVDM/AIVDO protocol decoding" (the gpsd project's public
//! description of the sentence armouring and of each message type's bit layout), and
//! ITU-R M.1371 (the message tables and the "not available" values). The published
//! sentence in the tests is the one that description uses; pyais read it as the oracle.
//!
//! The payload is a string of bits read most significant bit first. On air it rides in an
//! HDLC frame whose bytes go least significant bit first ([`payload_to_hdlc_bytes`],
//! [`hdlc_bytes_to_payload`]); in a sentence it is armoured six bits to a character
//! ([`armour`], [`unarmour`]).

use serde_json::{Map, Value, json};

/// Payload characters in one sentence before the payload is split into fragments.
pub const AIS_SENTENCE_CHARS: usize = 60;
/// Bits before the first field past the MMSI: a frame shorter is not decoded.
const AIS_HEADER_BITS: usize = 38;
/// Latitude and longitude are in 1/10 000 minute.
const AIS_POS_SCALE: f64 = 600_000.0;
/// The largest latitude and longitude, in degrees; "not available" is 91 and 181.
const AIS_LAT_MAX: f64 = 90.0;
const AIS_LON_MAX: f64 = 180.0;
/// Speed over ground in 0.1 kt; 1023 is not available.
const AIS_SOG_NA: u64 = 1023;
/// Course over ground in 0.1 degree; 3600 is not available.
const AIS_COG_NA: u64 = 3600;
/// True heading in degrees; 511 is not available.
const AIS_HEADING_NA: u64 = 511;
/// Headings run 0-359.
const AIS_HEADING_MAX: u64 = 359;
/// Rate of turn -128: no turn information.
const AIS_TURN_NA: i64 = -128;
/// The UTC second field: 60 is not available.
const AIS_SECOND_NA: u64 = 60;
/// Navigation status 0: under way using engine.
const AIS_STATUS_UNDER_WAY: u64 = 0;
/// Length of a type 1-3 position report and of a type 5 static report, in bits.
const AIS_POSITION_BITS: usize = 168;
const AIS_STATIC_BITS: usize = 424;
/// The six-bit text fields' lengths in characters.
const AIS_NAME_CHARS: usize = 20;
const AIS_CALLSIGN_CHARS: usize = 7;
/// Where a type 21 name extension starts, and its most characters.
const AIS_ATON_EXT_BIT: usize = 272;
const AIS_ATON_EXT_CHARS: usize = 14;
/// ETA "not available": hour 24, minute 60.
const AIS_ETA_HOUR_NA: u64 = 24;
const AIS_ETA_MINUTE_NA: u64 = 60;

/// One decoded AIS message; a field the type does not carry, or one marked not available,
/// is `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AisMsg {
    pub msg_type: u8,
    pub repeat: u8,
    pub mmsi: u32,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub sog_kt: Option<f64>,
    pub cog_deg: Option<f64>,
    pub heading_deg: Option<u16>,
    pub nav_status: Option<u8>,
    pub name: Option<String>,
    pub callsign: Option<String>,
    pub destination: Option<String>,
    pub imo: Option<u32>,
    pub ship_type: Option<u8>,
    pub aton_type: Option<u8>,
    pub virtual_aton: Option<bool>,
}

impl AisMsg {
    /// The event fields this message gives (every field of the AIS event but `protocol`,
    /// `channel` and `nmea`, which the receiver adds); a missing value is `null`.
    pub fn fields(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("mmsi".into(), json!(self.mmsi));
        m.insert("msg_type".into(), json!(self.msg_type));
        m.insert("repeat".into(), json!(self.repeat));
        m.insert("lat".into(), json!(self.lat));
        m.insert("lon".into(), json!(self.lon));
        m.insert("sog_kt".into(), json!(self.sog_kt));
        m.insert("cog_deg".into(), json!(self.cog_deg));
        m.insert("heading_deg".into(), json!(self.heading_deg));
        m.insert("nav_status".into(), json!(self.nav_status));
        m.insert("name".into(), json!(self.name));
        m.insert("callsign".into(), json!(self.callsign));
        m.insert("destination".into(), json!(self.destination));
        m.insert("imo".into(), json!(self.imo));
        m.insert("ship_type".into(), json!(self.ship_type));
        m.insert("aton_type".into(), json!(self.aton_type));
        m.insert("virtual_aton".into(), json!(self.virtual_aton));
        m
    }
}

/// Field reader over payload bits; every read is `None` past the end.
struct Fields<'a>(&'a [u8]);

impl Fields<'_> {
    fn uint(&self, from: usize, len: usize) -> Option<u64> {
        let b = self.0.get(from..from + len)?;
        Some(b.iter().fold(0u64, |a, &x| (a << 1) | u64::from(x & 1)))
    }

    fn int(&self, from: usize, len: usize) -> Option<i64> {
        let u = self.uint(from, len)?;
        let shift = 64 - len as u32;
        Some(((u << shift) as i64) >> shift)
    }

    /// Six-bit text of `chars` characters, trailing `@` and spaces trimmed; `None` when
    /// nothing is left.
    fn text(&self, from: usize, chars: usize) -> Option<String> {
        let s: String = (0..chars)
            .map(|i| self.uint(from + i * 6, 6).map(|v| six_bit_char(v as u8)))
            .collect::<Option<_>>()?;
        let t = s.trim_end_matches(['@', ' ']);
        (!t.is_empty()).then(|| t.to_string())
    }

    /// Latitude (27 bits); 91 (not available) or out of range is `None`.
    fn lat(&self, from: usize) -> Option<f64> {
        let v = self.int(from, 27)? as f64 / AIS_POS_SCALE;
        (v.abs() <= AIS_LAT_MAX).then_some(v)
    }

    /// Longitude (28 bits); 181 (not available) or out of range is `None`.
    fn lon(&self, from: usize) -> Option<f64> {
        let v = self.int(from, 28)? as f64 / AIS_POS_SCALE;
        (v.abs() <= AIS_LON_MAX).then_some(v)
    }

    fn sog(&self, from: usize) -> Option<f64> {
        let v = self.uint(from, 10)?;
        (v != AIS_SOG_NA).then(|| v as f64 / 10.0)
    }

    fn cog(&self, from: usize) -> Option<f64> {
        let v = self.uint(from, 12)?;
        (v < AIS_COG_NA).then(|| v as f64 / 10.0)
    }

    fn heading(&self, from: usize) -> Option<u16> {
        let v = self.uint(from, 9)?;
        (v <= AIS_HEADING_MAX).then_some(v as u16)
    }
}

/// The AIS six-bit character of `v`: 0-31 are `@`-`_`, 32-63 are space-`?`.
fn six_bit_char(v: u8) -> char {
    let v = v & 0x3F;
    if v < 32 { (v + 64) as char } else { v as char }
}

/// The six-bit value of `c` (upper-cased); a character outside the set is a space.
fn six_bit_value(c: char) -> u8 {
    match c.to_ascii_uppercase() {
        c @ '@'..='_' => c as u8 - 64,
        c @ ' '..='?' => c as u8,
        _ => b' ',
    }
}

/// Whether `c` is in the AIS six-bit character set as a name may use it: upper-case
/// letters, digits, space and `!"#$%&'()*+,-./:;<=>?`.
pub fn is_six_bit_text(c: char) -> bool {
    c.is_ascii_uppercase() || (' '..='?').contains(&c)
}

/// Decode an AIS payload (bits, most significant first). `None` under 38 bits; types 1-5,
/// 18, 21 and 24 (parts A and B) are read, any other type gives its type, repeat and MMSI
/// with the rest `None`. A field past the end of a short payload is `None`.
pub fn decode(bits: &[u8]) -> Option<AisMsg> {
    if bits.len() < AIS_HEADER_BITS {
        return None;
    }
    let f = Fields(bits);
    let mut m = AisMsg {
        msg_type: f.uint(0, 6)? as u8,
        repeat: f.uint(6, 2)? as u8,
        mmsi: f.uint(8, 30)? as u32,
        ..AisMsg::default()
    };
    match m.msg_type {
        1..=3 => {
            m.nav_status = f.uint(38, 4).map(|v| v as u8);
            m.sog_kt = f.sog(50);
            m.lon = f.lon(61);
            m.lat = f.lat(89);
            m.cog_deg = f.cog(116);
            m.heading_deg = f.heading(128);
        }
        4 => {
            m.lon = f.lon(79);
            m.lat = f.lat(107);
        }
        5 => {
            m.imo = f.uint(40, 30).map(|v| v as u32);
            m.callsign = f.text(70, AIS_CALLSIGN_CHARS);
            m.name = f.text(112, AIS_NAME_CHARS);
            m.ship_type = f.uint(232, 8).map(|v| v as u8);
            m.destination = f.text(302, AIS_NAME_CHARS);
        }
        18 => {
            m.sog_kt = f.sog(46);
            m.lon = f.lon(57);
            m.lat = f.lat(85);
            m.cog_deg = f.cog(112);
            m.heading_deg = f.heading(124);
        }
        21 => {
            m.aton_type = f.uint(38, 5).map(|v| v as u8);
            // A full 20-character name runs on into the extension after the fixed part.
            let ext = (bits.len().saturating_sub(AIS_ATON_EXT_BIT) / 6).min(AIS_ATON_EXT_CHARS);
            m.name = match (f.text(43, AIS_NAME_CHARS), f.text(AIS_ATON_EXT_BIT, ext)) {
                (Some(b), Some(x)) if b.len() == AIS_NAME_CHARS => Some(b + &x),
                (b, _) => b,
            };
            m.lon = f.lon(164);
            m.lat = f.lat(192);
            m.virtual_aton = f.uint(269, 1).map(|v| v == 1);
        }
        24 => match f.uint(38, 2) {
            Some(0) => m.name = f.text(40, AIS_NAME_CHARS),
            Some(1) => {
                m.ship_type = f.uint(40, 8).map(|v| v as u8);
                m.callsign = f.text(90, AIS_CALLSIGN_CHARS);
            }
            _ => {}
        },
        _ => {}
    }
    Some(m)
}

/// The payload's bits, six per character (`0`-`W` and `` ` ``-`w`); `None` for a
/// character outside the armouring.
pub fn unarmour(payload: &str) -> Option<Vec<u8>> {
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

/// The payload text of `bits` and its fill bits (the zeros added to make the last
/// character whole, 0-5).
pub fn armour(bits: &[u8]) -> (String, u8) {
    let fill = (6 - bits.len() % 6) % 6;
    let text = bits
        .chunks(6)
        .map(|c| {
            let v = (0..6).fold(0u8, |a, i| (a << 1) | (c.get(i).copied().unwrap_or(0) & 1));
            (if v < 40 { v + 48 } else { v + 56 }) as char
        })
        .collect();
    (text, fill as u8)
}

/// The `!AIVDM` sentence(s) of a payload as receivers print them:
/// `!AIVDM,<n>,<i>,<seq>,<channel>,<payload>,<fill>*<checksum>`, the payload split every
/// [`AIS_SENTENCE_CHARS`] characters, the sequence id empty for one sentence and `0` for
/// several, the fill bits on the last (0 on the others), the checksum the XOR of the
/// characters between `!` and `*` as two upper-case hex digits.
pub fn encode_sentences(bits: &[u8], channel: Option<char>) -> Vec<String> {
    let (payload, fill) = armour(bits);
    let chunks: Vec<&str> = if payload.is_empty() {
        vec![""]
    } else {
        payload
            .as_bytes()
            .chunks(AIS_SENTENCE_CHARS)
            .map(|c| std::str::from_utf8(c).expect("armour is ASCII"))
            .collect()
    };
    let n = chunks.len();
    let seq = if n == 1 { "" } else { "0" };
    let ch = channel.map(String::from).unwrap_or_default();
    chunks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let f = if i + 1 == n { fill } else { 0 };
            let body = format!("AIVDM,{n},{},{seq},{ch},{c},{f}", i + 1);
            let cs = body.bytes().fold(0u8, |a, b| a ^ b);
            format!("!{body}*{cs:02X}")
        })
        .collect()
}

/// The payload's bytes as HDLC sends them: each byte's bits reversed, so the first
/// payload bit is the least significant (the last byte padded with zeros to eight bits).
pub fn payload_to_hdlc_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8)
        .map(|c| {
            c.iter()
                .enumerate()
                .fold(0u8, |a, (i, &b)| a | ((b & 1) << i))
        })
        .collect()
}

/// The inverse of [`payload_to_hdlc_bytes`]: a deframed frame's bytes as payload bits,
/// eight per byte, least significant bit first.
pub fn hdlc_bytes_to_payload(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .flat_map(|b| (0..8).map(move |i| (b >> i) & 1))
        .collect()
}

fn push_uint(bits: &mut Vec<u8>, v: u64, len: usize) {
    bits.extend((0..len).rev().map(|i| ((v >> i) & 1) as u8));
}

fn push_int(bits: &mut Vec<u8>, v: i64, len: usize) {
    push_uint(bits, v as u64, len);
}

/// `s` as `chars` six-bit characters, `@`-filled.
fn push_text(bits: &mut Vec<u8>, s: &str, chars: usize) {
    let mut it = s.chars();
    for _ in 0..chars {
        push_uint(bits, u64::from(it.next().map_or(0, six_bit_value)), 6);
    }
}

/// A type 1 position report (168 bits): status under way using engine, no turn
/// information, heading and second not available, `sog_kt` in 0.1 kt (1023 at most),
/// `cog_deg` in 0.1 degree (3600 at most), the position in 1/10 000 minute.
pub fn position_report_bits(mmsi: u32, lat: f64, lon: f64, sog_kt: f64, cog_deg: f64) -> Vec<u8> {
    let mut b = Vec::with_capacity(AIS_POSITION_BITS);
    push_uint(&mut b, 1, 6);
    push_uint(&mut b, 0, 2);
    push_uint(&mut b, u64::from(mmsi), 30);
    push_uint(&mut b, AIS_STATUS_UNDER_WAY, 4);
    push_int(&mut b, AIS_TURN_NA, 8);
    push_uint(&mut b, ((sog_kt * 10.0).round() as u64).min(AIS_SOG_NA), 10);
    push_uint(&mut b, 0, 1);
    push_int(&mut b, (lon * AIS_POS_SCALE).round() as i64, 28);
    push_int(&mut b, (lat * AIS_POS_SCALE).round() as i64, 27);
    push_uint(
        &mut b,
        ((cog_deg * 10.0).round() as u64).min(AIS_COG_NA),
        12,
    );
    push_uint(&mut b, AIS_HEADING_NA, 9);
    push_uint(&mut b, AIS_SECOND_NA, 6);
    b.resize(AIS_POSITION_BITS, 0);
    b
}

/// A type 5 static and voyage report (424 bits) carrying `name` (six-bit, at most 20
/// characters, `@`-filled); the callsign and destination are all `@`, IMO, ship type and
/// dimensions 0, the ETA not available.
pub fn static_report_bits(mmsi: u32, name: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(AIS_STATIC_BITS);
    push_uint(&mut b, 5, 6);
    push_uint(&mut b, 0, 2);
    push_uint(&mut b, u64::from(mmsi), 30);
    push_uint(&mut b, 0, 2); // AIS version
    push_uint(&mut b, 0, 30); // IMO
    push_text(&mut b, "", AIS_CALLSIGN_CHARS);
    push_text(&mut b, name, AIS_NAME_CHARS);
    push_uint(&mut b, 0, 8); // ship type
    push_uint(&mut b, 0, 30); // dimensions
    push_uint(&mut b, 0, 4); // position fix type
    push_uint(&mut b, 0, 4); // ETA month
    push_uint(&mut b, 0, 5); // ETA day
    push_uint(&mut b, AIS_ETA_HOUR_NA, 5);
    push_uint(&mut b, AIS_ETA_MINUTE_NA, 6);
    push_uint(&mut b, 0, 8); // draught
    push_text(&mut b, "", AIS_NAME_CHARS);
    push_uint(&mut b, 1, 1); // DTE not ready
    b.resize(AIS_STATIC_BITS, 0);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload bits of `sentences` (fragments in order) joined.
    fn joined_bits(sentences: &[String]) -> Vec<u8> {
        let mut bits = Vec::new();
        for s in sentences {
            let f: Vec<&str> = s.split(',').collect();
            let fill: usize = f[6][..1].parse().unwrap();
            let mut b = unarmour(f[5]).unwrap();
            b.truncate(b.len() - fill);
            bits.extend(b);
        }
        bits
    }

    /// The NMEA checksum of the text between `!` and `*`.
    fn checksum_ok(s: &str) -> bool {
        let (body, cs) = s[1..].split_once('*').unwrap();
        format!("{:02X}", body.bytes().fold(0u8, |a, b| a ^ b)) == cs
    }

    #[test]
    fn the_published_sentence_decodes_to_the_full_fields() {
        // gpsd's AIVDM/AIVDO protocol description uses this sentence
        // (`!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C`); pyais 2.x reads it as
        // type 1, MMSI 477553000, status 5 (moored), speed 0.0, lon -122.345833,
        // lat 47.582833, course 51.0, heading 181.
        let m = decode(&unarmour("177KQJ5000G?tO`K>RA1wUbN0TKH").unwrap()).unwrap();
        assert_eq!(m.msg_type, 1);
        assert_eq!(m.repeat, 0);
        assert_eq!(m.mmsi, 477_553_000);
        assert!((m.lat.unwrap() - 47.582_833).abs() < 1e-6, "{:?}", m.lat);
        assert!((m.lon.unwrap() - -122.345_833).abs() < 1e-6, "{:?}", m.lon);
        assert_eq!(m.sog_kt, Some(0.0));
        assert_eq!(m.cog_deg, Some(51.0));
        assert_eq!(m.heading_deg, Some(181));
        assert_eq!(m.nav_status, Some(5));
        assert_eq!(m.name, None);
        let f = m.fields();
        assert_eq!(f["mmsi"], 477_553_000);
        assert_eq!(f["msg_type"], 1);
        assert_eq!(f["name"], serde_json::Value::Null);
        for k in ["protocol", "channel", "nmea"] {
            assert!(!f.contains_key(k), "{k}");
        }
    }

    #[test]
    fn a_type_5_payload_spans_two_sentences() {
        let bits = static_report_bits(503_000_001, "ZETA TEST");
        assert_eq!(bits.len(), 424);
        let s = encode_sentences(&bits, Some('A'));
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(s[0].starts_with("!AIVDM,2,1,0,A,"), "{}", s[0]);
        let f0: Vec<&str> = s[0].split(',').collect();
        assert_eq!(f0[5].len(), AIS_SENTENCE_CHARS);
        assert!(f0[6].starts_with("0*"), "{}", s[0]);
        assert!(s[1].starts_with("!AIVDM,2,2,0,A,"), "{}", s[1]);
        let f1: Vec<&str> = s[1].split(',').collect();
        assert_eq!(f1[5].len(), 11);
        assert!(f1[6].starts_with("2*"), "{}", s[1]);
        assert!(s.iter().all(|x| checksum_ok(x)), "{s:?}");
        let m = decode(&joined_bits(&s)).unwrap();
        assert_eq!(m.msg_type, 5);
        assert_eq!(m.mmsi, 503_000_001);
        assert_eq!(m.name.as_deref(), Some("ZETA TEST"));
        assert_eq!(m.callsign, None);
        assert_eq!(m.destination, None);
    }

    #[test]
    fn a_position_report_round_trips_through_sentences() {
        let bits = position_report_bits(503_000_001, -27.3842, 153.1175, 12.3, 45.0);
        assert_eq!(bits.len(), 168);
        let s = encode_sentences(&bits, Some('A'));
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(s[0].starts_with("!AIVDM,1,1,,A,"), "{}", s[0]);
        assert!(s[0].contains(",0*"), "{}", s[0]);
        assert!(checksum_ok(&s[0]), "{}", s[0]);
        let m = decode(&joined_bits(&s)).unwrap();
        assert_eq!((m.msg_type, m.mmsi), (1, 503_000_001));
        assert!((m.lat.unwrap() - -27.3842).abs() < 1e-4, "{:?}", m.lat);
        assert!((m.lon.unwrap() - 153.1175).abs() < 1e-4, "{:?}", m.lon);
        assert_eq!(m.sog_kt, Some(12.3));
        assert_eq!(m.cog_deg, Some(45.0));
        // No channel: the field is empty.
        assert!(encode_sentences(&bits, None)[0].starts_with("!AIVDM,1,1,,,"));
    }

    #[test]
    fn not_available_values_are_null() {
        let m = decode(&position_report_bits(
            503_000_001,
            91.0,
            181.0,
            102.3,
            360.0,
        ))
        .unwrap();
        assert_eq!(m.lat, None);
        assert_eq!(m.lon, None);
        assert_eq!(m.sog_kt, None);
        assert_eq!(m.cog_deg, None);
        assert_eq!(m.heading_deg, None);
        let f = m.fields();
        for k in ["lat", "lon", "sog_kt", "cog_deg", "heading_deg"] {
            assert_eq!(f[k], serde_json::Value::Null, "{k}");
        }
    }

    #[test]
    fn an_unread_type_keeps_mmsi_and_type() {
        let mut bits = Vec::new();
        push_uint(&mut bits, 8, 6);
        push_uint(&mut bits, 1, 2);
        push_uint(&mut bits, 123_456_789, 30);
        bits.extend(std::iter::repeat_n(1, 100));
        let m = decode(&bits).unwrap();
        assert_eq!((m.msg_type, m.repeat, m.mmsi), (8, 1, 123_456_789));
        assert_eq!(m.lat, None);
        assert_eq!(m.name, None);
        assert!(decode(&bits[..37]).is_none());
    }

    #[test]
    fn hdlc_byte_order_round_trips() {
        let bits: Vec<u8> = (0..30u32)
            .map(|i| u8::from(i % 3 == 0 || i % 7 == 1))
            .collect();
        let bytes = payload_to_hdlc_bytes(&bits);
        assert_eq!(bytes.len(), 4);
        // The first payload bit is the first sent: the byte's least significant bit.
        assert_eq!(bytes[0] & 1, bits[0]);
        let back: Vec<u8> = bytes
            .iter()
            .flat_map(|b| (0..8).map(move |i| (b >> i) & 1))
            .collect();
        let mut padded = bits.clone();
        padded.resize(32, 0);
        assert_eq!(back, padded);
        assert_eq!(hdlc_bytes_to_payload(&bytes), padded);
    }

    #[test]
    fn class_b_and_aton_messages_are_read() {
        // Type 24 part A: the name; part B: ship type and callsign.
        let mut a = Vec::new();
        push_uint(&mut a, 24, 6);
        push_uint(&mut a, 0, 2);
        push_uint(&mut a, 503_000_002, 30);
        push_uint(&mut a, 0, 2);
        push_text(&mut a, "SMALL BOAT", 20);
        let m = decode(&a).unwrap();
        assert_eq!(m.name.as_deref(), Some("SMALL BOAT"));
        let mut b = Vec::new();
        push_uint(&mut b, 24, 6);
        push_uint(&mut b, 0, 2);
        push_uint(&mut b, 503_000_002, 30);
        push_uint(&mut b, 1, 2);
        push_uint(&mut b, 37, 8);
        push_text(&mut b, "", 7);
        push_text(&mut b, "VK4ABC", 7);
        b.resize(168, 0);
        let m = decode(&b).unwrap();
        assert_eq!(
            (m.ship_type, m.callsign.as_deref()),
            (Some(37), Some("VK4ABC"))
        );
        assert_eq!(m.name, None);
        // Type 18: class B position.
        let mut p = Vec::new();
        push_uint(&mut p, 18, 6);
        push_uint(&mut p, 0, 2);
        push_uint(&mut p, 503_000_002, 30);
        push_uint(&mut p, 0, 8);
        push_uint(&mut p, 55, 10);
        push_uint(&mut p, 0, 1);
        push_int(&mut p, (153.1175f64 * 600_000.0).round() as i64, 28);
        push_int(&mut p, (-27.3842f64 * 600_000.0).round() as i64, 27);
        push_uint(&mut p, 1800, 12);
        push_uint(&mut p, 511, 9);
        p.resize(168, 0);
        let m = decode(&p).unwrap();
        assert_eq!(
            (m.msg_type, m.sog_kt, m.cog_deg),
            (18, Some(5.5), Some(180.0))
        );
        assert!((m.lat.unwrap() - -27.3842).abs() < 1e-6);
        assert_eq!(m.heading_deg, None);
        // Type 21: an aid to navigation, virtual.
        let mut t = Vec::new();
        push_uint(&mut t, 21, 6);
        push_uint(&mut t, 0, 2);
        push_uint(&mut t, 995_031_001, 30);
        push_uint(&mut t, 30, 5);
        push_text(&mut t, "NE SPIT", 20);
        push_uint(&mut t, 0, 1);
        push_int(&mut t, (153.1175f64 * 600_000.0).round() as i64, 28);
        push_int(&mut t, (-27.3842f64 * 600_000.0).round() as i64, 27);
        push_uint(&mut t, 0, 30 + 4 + 6 + 1 + 8 + 1);
        push_uint(&mut t, 1, 1);
        t.resize(272, 0);
        let m = decode(&t).unwrap();
        assert_eq!(m.aton_type, Some(30));
        assert_eq!(m.virtual_aton, Some(true));
        assert_eq!(m.name.as_deref(), Some("NE SPIT"));
        assert!((m.lon.unwrap() - 153.1175).abs() < 1e-6);
        // Type 4: a base station's position.
        let mut s = Vec::new();
        push_uint(&mut s, 4, 6);
        push_uint(&mut s, 0, 2);
        push_uint(&mut s, 3_503_000, 30);
        push_uint(&mut s, 0, 14 + 4 + 5 + 5 + 6 + 6 + 1);
        push_int(&mut s, (153.1175f64 * 600_000.0).round() as i64, 28);
        push_int(&mut s, (-27.3842f64 * 600_000.0).round() as i64, 27);
        s.resize(168, 0);
        let m = decode(&s).unwrap();
        assert!((m.lat.unwrap() - -27.3842).abs() < 1e-6);
        assert_eq!(m.sog_kt, None);
    }

    #[test]
    fn armour_pads_to_six_bits() {
        let (text, fill) = armour(&[0, 0, 0, 0, 0, 1, 1, 1]);
        assert_eq!((text.as_str(), fill), ("1h", 4));
        assert_eq!(unarmour("1h").unwrap()[..8], [0, 0, 0, 0, 0, 1, 1, 1]);
        assert!(unarmour("1x").is_none());
    }
}
