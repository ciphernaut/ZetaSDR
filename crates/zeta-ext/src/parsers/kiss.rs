//! `kiss`: TNC frames, one KISS frame per line written as hex (case-insensitive,
//! spaces allowed): `C0 00 <AX.25 frame> C0`, the `C0` delimiters optional, the
//! command byte a data frame (low nibble 0, any port), `DB DC` and `DB DD`
//! unescaped to `C0` and `DB`. The AX.25 frame comes from the TNC without its
//! FCS (a TNC passes only CRC-good frames); a trailing FCS that checks is
//! stripped too. The frame is read as `aprs_decode` reads one:
//! `{src, dst, path, info, fcs_ok}` and the fields `aprs_info` reads from the info
//! (`lat`, `lon`, `alt_m`, `course_deg`, `speed_kt`, `symbol`, `comment`, `data_type`, `object`).

use serde_json::{Value, json};
use zeta_dsp::aprs_info::parse_info;
use zeta_dsp::ax25::parse_ax25_ui;

const FEND: u8 = 0xC0;
const FESC: u8 = 0xDB;
const TFEND: u8 = 0xDC;
const TFESC: u8 = 0xDD;

/// The fields of the UI frame on `line`; `None` for a line that is not a hex
/// KISS data frame holding one.
pub fn parse(line: &str) -> Option<Value> {
    let raw = hex(line)?;
    let raw = raw.strip_prefix(&[FEND]).unwrap_or(&raw);
    let raw = raw.strip_suffix(&[FEND]).unwrap_or(raw);
    let (&cmd, escaped) = raw.split_first()?;
    if cmd & 0x0F != 0 {
        return None;
    }
    let frame = unescape(escaped)?;
    let body = match frame.split_last_chunk::<2>() {
        Some((body, fcs)) if u16::from_le_bytes(*fcs) == zeta_dsp::crc16_x25(body) => body,
        _ => &frame[..],
    };
    let f = parse_ax25_ui(body)?;
    let mut v = json!({
        "src": f.src,
        "dst": f.dst,
        "path": f.path,
        "info": String::from_utf8_lossy(&f.info),
        "fcs_ok": true,
    });
    if let Some(m) = v.as_object_mut() {
        m.extend(parse_info(&f.dst, &f.info).fields());
    }
    Some(v)
}

/// The bytes of a hex line, whitespace ignored; `None` for anything else.
fn hex(line: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = line.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return None;
    }
    digits
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

/// KISS transparency undone; `None` for a stray `C0` or a bad escape.
fn unescape(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len());
    let mut it = b.iter();
    while let Some(&c) = it.next() {
        match c {
            FEND => return None,
            FESC => match *it.next()? {
                TFEND => out.push(FEND),
                TFESC => out.push(FESC),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeta_dsp::fixtures::ax25_ui_frame;

    /// `frame` as a KISS data frame on port 0, in hex.
    fn kiss_hex(frame: &[u8]) -> String {
        let mut k = vec![FEND, 0x00];
        for &b in frame {
            match b {
                FEND => k.extend([FESC, TFEND]),
                FESC => k.extend([FESC, TFESC]),
                b => k.push(b),
            }
        }
        k.push(FEND);
        k.iter().map(|b| format!("{b:02X} ")).collect()
    }

    #[test]
    fn a_tnc_frame_reads_as_aprs() {
        let with_fcs = ax25_ui_frame(
            "VK3ZET-9",
            "APZETA",
            &["WIDE1-1"],
            b"!3745.00S/14458.00E>zeta\xC0",
        );
        // As a TNC sends it: without the FCS.
        let frame = &with_fcs[..with_fcs.len() - 2];
        let v = parse(&kiss_hex(frame)).unwrap();
        assert_eq!(v["src"], "VK3ZET-9");
        assert_eq!(v["dst"], "APZETA");
        assert_eq!(v["path"], json!(["WIDE1-1"]));
        assert_eq!(v["fcs_ok"], true);
        assert!(
            v["info"]
                .as_str()
                .unwrap()
                .starts_with("!3745.00S/14458.00E>zeta")
        );
        // The position read from the info, as `aprs_decode` reads it.
        assert_eq!(v["data_type"], "position");
        assert!((v["lat"].as_f64().unwrap() + 37.75).abs() < 1e-9, "{v}");
        assert!(
            (v["lon"].as_f64().unwrap() - (144.0 + 58.0 / 60.0)).abs() < 1e-9,
            "{v}"
        );
        assert_eq!(v["symbol"], "/>");
        assert_eq!(v["alt_m"], Value::Null);
        assert_eq!(v["object"], Value::Null);
        // With the FCS still on, lower case and without spaces.
        let line = kiss_hex(&with_fcs).replace(' ', "").to_lowercase();
        assert_eq!(parse(&line).unwrap()["src"], "VK3ZET-9");
    }

    #[test]
    fn a_frame_without_a_position_reads_its_type() {
        let with_fcs = ax25_ui_frame("VK3ZET", "APZETA", &[], b">on air");
        let v = parse(&kiss_hex(&with_fcs[..with_fcs.len() - 2])).unwrap();
        assert_eq!(v["data_type"], "status");
        assert_eq!(v["lat"], Value::Null);
        assert_eq!(v["info"], ">on air");
    }

    #[test]
    fn other_lines_are_unread() {
        assert!(parse("hello").is_none());
        assert!(parse("C0 01 02 C0").is_none());
        assert!(parse("C0 00 DB 00 C0").is_none());
        assert!(parse("").is_none());
    }
}
