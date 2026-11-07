//! The information field of an APRS packet, read for what the map and the Subjects table use.
//!
//! Sources: the APRS Protocol Reference, version 1.0.1 (TAPR, 2000), chapters 5 (data types),
//! 6 (time and position formats, ambiguity), 7 (data extensions: course and speed), 8 (the
//! `/A=` altitude in the comment), 9 (compressed positions) and 10 (Mic-E), with their examples
//! as the unit tests.
//!
//! [`parse_info`] never fails: a packet whose position cannot be read keeps its `data_type` with
//! the fix fields `None`, so the event still carries `src`, `dst` and `info`. Objects and items
//! are named but not placed: the subject key is the sender, and an object's fix on the sender's
//! track would be wrong.

use serde::Serialize;
use serde_json::{Map, Value};

/// Feet to metres.
const FEET_TO_M: f64 = 0.3048;
/// `/`, `@`: the timestamp's length (`ddhhmmz`, `ddhhmm/`, `hhmmssh`).
const TIMESTAMP_LEN: usize = 7;
/// `ddmm.hhN`.
const LAT_LEN: usize = 8;
/// `dddmm.hhW`.
const LON_LEN: usize = 9;
/// `ccc/sss`, right after the symbol code.
const COURSE_SPEED_LEN: usize = 7;
/// `/A=` then six characters of feet.
const ALT_TAG: &[u8] = b"/A=";
const ALT_DIGITS: usize = 6;
/// Minutes in hundredths: a degree, and the ambiguous cell's size by the blanked digits.
const HUNDREDTHS_PER_DEGREE: u32 = 6000;
const HUNDREDTHS_PER_MINUTE: u32 = 100;
const AMBIGUITY_CELL: [u32; 5] = [1, 10, 100, 1000, HUNDREDTHS_PER_DEGREE];
/// Base-91 digits are printable bytes from `!`.
const BASE91_ZERO: u8 = b'!';
const BASE91: u32 = 91;
/// Compressed: the table byte, `YYYY`, `XXXX`, the code, `cs` and `T`.
const COMPRESSED_LEN: usize = 13;
/// Compressed latitude `90 - y / 380926`, longitude `-180 + x / 190463`.
const COMPRESSED_LAT_DIV: f64 = 380_926.0;
const COMPRESSED_LON_DIV: f64 = 190_463.0;
/// Compressed `c` values 0-89 are a course in 4-degree steps; `s` a speed `1.08^s - 1` knots.
const COMPRESSED_COURSE_MAX: u8 = 89;
const COMPRESSED_COURSE_STEP: f64 = 4.0;
const COMPRESSED_SPEED_BASE: f64 = 1.08;
/// Compressed altitude `1.002^cs` feet.
const COMPRESSED_ALT_BASE: f64 = 1.002;
/// The compression type's NMEA source (bits 3-4): `10` is a GGA fix, whose `cs` is altitude.
const COMPRESSION_NMEA_SHIFT: u8 = 3;
const COMPRESSION_NMEA_MASK: u8 = 0b11;
const COMPRESSION_NMEA_GGA: u8 = 0b10;
/// Mic-E: the destination's six latitude characters and the info's nine fixed bytes (the
/// identifier, longitude, speed and course, symbol).
const MIC_E_DST_LEN: usize = 6;
const MIC_E_INFO_LEN: usize = 9;
/// Mic-E info bytes carry values offset by 28.
const MIC_E_OFFSET: u8 = 28;
/// Mic-E longitude degrees: +100 when flagged, then 180-189 is 100-109 and 190-199 is 0-9.
const MIC_E_LON_OFFSET: u32 = 100;
/// Mic-E minutes 60-69 mean 0-9; speed 800+ and course 400+ wrap.
const MIC_E_MIN_WRAP: u32 = 60;
const MIC_E_SPEED_WRAP: u32 = 800;
const MIC_E_COURSE_WRAP: u32 = 400;
/// Mic-E altitude: three base-91 digits and `}`, metres above 10 km below sea level.
const MIC_E_ALT_LEN: usize = 4;
const MIC_E_ALT_DATUM_M: f64 = 10_000.0;
/// An object's name is nine characters; an item's three to nine.
const OBJECT_NAME_LEN: usize = 9;
const ITEM_NAME_MAX: usize = 9;

/// What a packet was, by its data type identifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Position,
    MicE,
    Object,
    Item,
    Message,
    Status,
    Telemetry,
    Weather,
    #[default]
    Other,
}

/// The fields read from one information field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AprsInfo {
    pub data_type: DataType,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt_m: Option<f64>,
    pub course_deg: Option<f64>,
    pub speed_kt: Option<f64>,
    /// The symbol table (or overlay) and code, two characters.
    pub symbol: Option<String>,
    pub comment: Option<String>,
    /// An object's or item's name, trimmed.
    pub object: Option<String>,
    /// How many trailing position digits were blanked (0-4).
    pub ambiguity: u8,
}

impl AprsInfo {
    /// The event fields: `lat`, `lon`, `alt_m`, `course_deg`, `speed_kt`, `symbol`, `comment`,
    /// `data_type`, `object`, each null when unknown.
    pub fn fields(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("lat".into(), self.lat.into());
        m.insert("lon".into(), self.lon.into());
        m.insert("alt_m".into(), self.alt_m.into());
        m.insert("course_deg".into(), self.course_deg.into());
        m.insert("speed_kt".into(), self.speed_kt.into());
        m.insert("symbol".into(), self.symbol.clone().into());
        m.insert("comment".into(), self.comment.clone().into());
        m.insert(
            "data_type".into(),
            serde_json::to_value(self.data_type).unwrap_or(Value::Null),
        );
        m.insert("object".into(), self.object.clone().into());
        m
    }
}

/// Reads `info` (with the packet's destination `dst`, which Mic-E needs). Never fails.
pub fn parse_info(dst: &str, info: &[u8]) -> AprsInfo {
    let mut out = AprsInfo::default();
    let Some((&id, rest)) = info.split_first() else {
        return out;
    };
    match id {
        b'!' | b'=' => {
            out.data_type = DataType::Position;
            position(rest, &mut out);
        }
        b'/' | b'@' => {
            out.data_type = DataType::Position;
            if let Some(body) = rest.get(TIMESTAMP_LEN..) {
                position(body, &mut out);
            }
        }
        b'`' | b'\'' | 0x1c | 0x1d => {
            out.data_type = DataType::MicE;
            mic_e(dst, info, &mut out);
        }
        b';' => {
            out.data_type = DataType::Object;
            out.object = name_of(&rest[..rest.len().min(OBJECT_NAME_LEN)]);
        }
        b')' => {
            out.data_type = DataType::Item;
            let end = rest
                .iter()
                .take(ITEM_NAME_MAX + 1)
                .position(|&b| b == b'!' || b == b'_');
            out.object = end.and_then(|e| name_of(&rest[..e]));
        }
        b':' => out.data_type = DataType::Message,
        b'>' => out.data_type = DataType::Status,
        b'T' if rest.first() == Some(&b'#') => out.data_type = DataType::Telemetry,
        b'_' => out.data_type = DataType::Weather,
        _ => {}
    }
    out
}

fn name_of(b: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(b).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// A position body (after the identifier and any timestamp): uncompressed when it starts with
/// a digit, compressed otherwise. Leaves `out` without a fix when it does not read.
fn position(b: &[u8], out: &mut AprsInfo) {
    match b.first() {
        Some(c) if c.is_ascii_digit() => uncompressed(b, out),
        Some(_) => compressed(b, out),
        None => {}
    }
}

/// The fix of an uncompressed body: latitude, longitude, ambiguity, symbol.
fn uncompressed_fix(b: &[u8]) -> Option<(f64, f64, u8, [u8; 2])> {
    let lat = b.get(..LAT_LEN)?;
    let table = *b.get(LAT_LEN)?;
    let lon = b.get(LAT_LEN + 1..LAT_LEN + 1 + LON_LEN)?;
    let code = *b.get(LAT_LEN + 1 + LON_LEN)?;
    if !table.is_ascii_graphic() || !code.is_ascii_graphic() {
        return None;
    }
    let (lat_deg, lat_h, ambiguity) = coordinate(&lat[..LAT_LEN - 1], 2)?;
    let sign_lat = match lat[LAT_LEN - 1] {
        b'N' => 1.0,
        b'S' => -1.0,
        _ => return None,
    };
    // The longitude takes the latitude's ambiguity; its own blanks read as zero.
    let (lon_deg, lon_h, _) = coordinate(&lon[..LON_LEN - 1], 3)?;
    let sign_lon = match lon[LON_LEN - 1] {
        b'E' => 1.0,
        b'W' => -1.0,
        _ => return None,
    };
    let lat = sign_lat * degrees(lat_deg, lat_h, ambiguity, 90)?;
    let lon = sign_lon * degrees(lon_deg, lon_h, ambiguity, 180)?;
    Some((lat, lon, ambiguity, [table, code]))
}

fn uncompressed(b: &[u8], out: &mut AprsInfo) {
    let Some((lat, lon, ambiguity, symbol)) = uncompressed_fix(b) else {
        return;
    };
    out.lat = Some(lat);
    out.lon = Some(lon);
    out.ambiguity = ambiguity;
    out.symbol = Some(String::from_utf8_lossy(&symbol).into_owned());
    let mut comment = &b[LAT_LEN + 1 + LON_LEN + 1..];
    if let Some((course, speed)) = course_speed(comment) {
        out.course_deg = course;
        out.speed_kt = speed;
        comment = &comment[COURSE_SPEED_LEN..];
    }
    finish_comment(comment, out);
}

/// `dd[d]mm.hh` (no hemisphere) with spaces for blanked digits: the degrees, the minutes in
/// hundredths with blanks read as zero, and how many digits were blanked. `None` unless the
/// blanks run from the right and every other place is a digit.
fn coordinate(b: &[u8], deg_digits: usize) -> Option<(u32, u32, u8)> {
    let digit = |c: u8| c.is_ascii_digit().then(|| u32::from(c - b'0'));
    if b.len() != deg_digits + 5 || b[deg_digits + 2] != b'.' {
        return None;
    }
    let mut deg = 0;
    for &c in &b[..deg_digits] {
        deg = deg * 10 + digit(c)?;
    }
    // The minute places, most significant first.
    let places = [
        b[deg_digits],
        b[deg_digits + 1],
        b[deg_digits + 3],
        b[deg_digits + 4],
    ];
    let mut hundredths = 0;
    let mut blanks = 0u8;
    for &c in &places {
        let d = match c {
            b' ' => {
                blanks += 1;
                0
            }
            _ if blanks > 0 => return None,
            _ => digit(c)?,
        };
        hundredths = hundredths * 10 + d;
    }
    Some((deg, hundredths, blanks))
}

/// Degrees from whole degrees and minutes in hundredths, at the centre of the cell the
/// ambiguity leaves; `None` past `max` degrees or 60 minutes.
fn degrees(deg: u32, hundredths: u32, ambiguity: u8, max: u32) -> Option<f64> {
    let cell = *AMBIGUITY_CELL.get(usize::from(ambiguity))?;
    if hundredths >= HUNDREDTHS_PER_DEGREE || deg > max || (deg == max && hundredths > 0) {
        return None;
    }
    let h = f64::from(hundredths - hundredths % cell) + f64::from(cell / 2);
    Some(f64::from(deg) + h / f64::from(HUNDREDTHS_PER_DEGREE))
}

/// `ccc/sss` at the start of `b`: the course (000 and `...` unknown) and speed (`...` unknown).
fn course_speed(b: &[u8]) -> Option<(Option<f64>, Option<f64>)> {
    let ext = b.get(..COURSE_SPEED_LEN)?;
    if ext[3] != b'/' {
        return None;
    }
    let field = |f: &[u8]| -> Option<Option<f64>> {
        if f.iter().all(|&c| c == b'.' || c == b' ') {
            return Some(None);
        }
        if !f.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(std::str::from_utf8(f).ok()?.parse::<f64>().ok())
    };
    let course = field(&ext[..3])?.filter(|&c| c > 0.0);
    let speed = field(&ext[4..])?;
    Some((course, speed))
}

/// The comment, and `/A=aaaaaa` from it when no altitude is known yet.
fn finish_comment(b: &[u8], out: &mut AprsInfo) {
    if out.alt_m.is_none() {
        out.alt_m = b
            .windows(ALT_TAG.len() + ALT_DIGITS)
            .filter(|w| w.starts_with(ALT_TAG))
            .find_map(|w| {
                let s = std::str::from_utf8(&w[ALT_TAG.len()..]).ok()?;
                s.parse::<i32>().ok().map(|ft| f64::from(ft) * FEET_TO_M)
            });
    }
    let s = String::from_utf8_lossy(b).trim_end().to_string();
    out.comment = (!s.is_empty()).then_some(s);
}

fn base91(b: &[u8]) -> Option<u32> {
    b.iter().try_fold(0u32, |acc, &c| {
        let d = c
            .checked_sub(BASE91_ZERO)
            .filter(|&d| u32::from(d) < BASE91)?;
        Some(acc * BASE91 + u32::from(d))
    })
}

fn compressed(b: &[u8], out: &mut AprsInfo) {
    let Some(p) = b.get(..COMPRESSED_LEN) else {
        return;
    };
    let (table, code) = (p[0], p[9]);
    let symbol_ok = matches!(table, b'/' | b'\\' | b'A'..=b'Z' | b'a'..=b'j');
    let (Some(y), Some(x)) = (base91(&p[1..5]), base91(&p[5..9])) else {
        return;
    };
    let lat = 90.0 - f64::from(y) / COMPRESSED_LAT_DIV;
    let lon = -180.0 + f64::from(x) / COMPRESSED_LON_DIV;
    if !symbol_ok || !code.is_ascii_graphic() || lat.abs() > 90.0 || lon.abs() > 180.0 {
        return;
    }
    out.lat = Some(lat);
    out.lon = Some(lon);
    out.symbol = Some(String::from_utf8_lossy(&[table, code]).into_owned());
    let (c, s, t) = (p[10], p[11], p[12]);
    // A space for `c`: the `cs` bytes say nothing.
    if c != b' ' {
        let gga = t.checked_sub(BASE91_ZERO).is_some_and(|t| {
            (t >> COMPRESSION_NMEA_SHIFT) & COMPRESSION_NMEA_MASK == COMPRESSION_NMEA_GGA
        });
        if gga {
            if let Some(cs) = base91(&[c, s]) {
                out.alt_m = Some(COMPRESSED_ALT_BASE.powf(f64::from(cs)) * FEET_TO_M);
            }
        } else if let (Some(cv), Some(sv)) = (c.checked_sub(BASE91_ZERO), base91(&[s])) {
            // `{` (90) is a radio range, which is no event field.
            if cv <= COMPRESSED_COURSE_MAX {
                out.course_deg = Some(f64::from(cv) * COMPRESSED_COURSE_STEP);
                out.speed_kt = Some(COMPRESSED_SPEED_BASE.powf(f64::from(sv)) - 1.0);
            }
        }
    }
    finish_comment(&b[COMPRESSED_LEN..], out);
}

/// One Mic-E destination character: its latitude digit (`None` for a blank) and its flag
/// (`A-K` and `P-Z` set it: on characters 4-6 it is north, the +100 offset and west).
fn mic_e_dst_char(c: u8) -> Option<(Option<u8>, bool)> {
    Some(match c {
        b'0'..=b'9' => (Some(c - b'0'), false),
        b'A'..=b'J' => (Some(c - b'A'), true),
        b'P'..=b'Y' => (Some(c - b'P'), true),
        b'K' | b'Z' => (None, true),
        b'L' => (None, false),
        _ => return None,
    })
}

/// The Mic-E fix: latitude, longitude, ambiguity, speed, course and symbol.
struct MicEFix {
    lat: f64,
    lon: f64,
    ambiguity: u8,
    speed_kt: u32,
    course_deg: u32,
    symbol: [u8; 2],
}

fn mic_e_fix(dst: &str, info: &[u8]) -> Option<MicEFix> {
    let call = dst.split('-').next().unwrap_or(dst).as_bytes();
    if call.len() != MIC_E_DST_LEN {
        return None;
    }
    let i = info.get(..MIC_E_INFO_LEN)?;
    let chars = call
        .iter()
        .map(|&c| mic_e_dst_char(c))
        .collect::<Option<Vec<_>>>()?;
    // The latitude digits as an uncompressed `ddmm.hh`, blanks as spaces.
    let mut lat = [b'.'; LAT_LEN - 1];
    for (k, (digit, _)) in chars.iter().enumerate() {
        let at = if k < 4 { k } else { k + 1 };
        lat[at] = digit.map_or(b' ', |v| b'0' + v);
    }
    let (lat_deg, lat_h, ambiguity) = coordinate(&lat, 2)?;
    let (north, lon_offset, west) = (chars[3].1, chars[4].1, chars[5].1);
    let v = |b: u8| b.checked_sub(MIC_E_OFFSET).map(u32::from);
    let (mut lon_deg, mut lon_min, lon_hh) = (v(i[1])?, v(i[2])?, v(i[3])?);
    if lon_offset {
        lon_deg += MIC_E_LON_OFFSET;
    }
    lon_deg = match lon_deg {
        180..=189 => lon_deg - 80,
        190..=199 => lon_deg - 190,
        d => d,
    };
    if lon_min >= MIC_E_MIN_WRAP {
        lon_min -= MIC_E_MIN_WRAP;
    }
    if lon_hh >= HUNDREDTHS_PER_MINUTE {
        return None;
    }
    let lat = degrees(lat_deg, lat_h, ambiguity, 90)?;
    let lon = degrees(
        lon_deg,
        lon_min * HUNDREDTHS_PER_MINUTE + lon_hh,
        ambiguity,
        180,
    )?;
    let (sp, dc, se) = (v(i[4])?, v(i[5])?, v(i[6])?);
    let mut speed_kt = sp * 10 + dc / 10;
    if speed_kt >= MIC_E_SPEED_WRAP {
        speed_kt -= MIC_E_SPEED_WRAP;
    }
    let mut course_deg = (dc % 10) * 100 + se;
    if course_deg >= MIC_E_COURSE_WRAP {
        course_deg -= MIC_E_COURSE_WRAP;
    }
    let (code, table) = (i[7], i[8]);
    if !code.is_ascii_graphic() || !table.is_ascii_graphic() {
        return None;
    }
    Some(MicEFix {
        lat: if north { lat } else { -lat },
        lon: if west { -lon } else { lon },
        ambiguity,
        speed_kt,
        course_deg,
        symbol: [table, code],
    })
}

fn mic_e(dst: &str, info: &[u8], out: &mut AprsInfo) {
    let Some(f) = mic_e_fix(dst, info) else {
        return;
    };
    out.lat = Some(f.lat);
    out.lon = Some(f.lon);
    out.ambiguity = f.ambiguity;
    out.speed_kt = Some(f64::from(f.speed_kt));
    // Course 0 is unknown.
    out.course_deg = (f.course_deg > 0).then(|| f64::from(f.course_deg));
    out.symbol = Some(String::from_utf8_lossy(&f.symbol).into_owned());
    let mut rest = &info[MIC_E_INFO_LEN..];
    if let Some(alt) = rest.get(..MIC_E_ALT_LEN).filter(|a| a[3] == b'}')
        && let Some(m) = base91(&alt[..3])
    {
        out.alt_m = Some(f64::from(m) - MIC_E_ALT_DATUM_M);
        rest = &rest[MIC_E_ALT_LEN..];
    }
    finish_comment(rest, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(got: Option<f64>, want: f64, within: f64) {
        let g = got.unwrap_or_else(|| panic!("expected {want}, got None"));
        assert!((g - want).abs() <= within, "got {g}, want {want} ±{within}");
    }

    fn no_fix(i: &AprsInfo) {
        assert_eq!((i.lat, i.lon, i.alt_m), (None, None, None), "{i:?}");
        assert_eq!((i.course_deg, i.speed_kt), (None, None), "{i:?}");
    }

    #[test]
    fn uncompressed_position_with_symbol_and_comment() {
        let i = parse_info("APRS", b"!4903.50N/07201.75W-Test 001234");
        assert_eq!(i.data_type, DataType::Position);
        near(i.lat, 49.058333, 1e-6);
        near(i.lon, -72.029167, 1e-6);
        assert_eq!(i.symbol.as_deref(), Some("/-"));
        assert_eq!(i.comment.as_deref(), Some("Test 001234"));
        assert_eq!((i.course_deg, i.speed_kt, i.alt_m), (None, None, None));
        assert_eq!(i.ambiguity, 0);
    }

    #[test]
    fn uncompressed_with_timestamp_course_speed_and_altitude() {
        let i = parse_info("APRS", b"/092345z4903.50N/07201.75W>088/036/A=001234");
        assert_eq!(i.data_type, DataType::Position);
        near(i.lat, 49.058333, 1e-6);
        near(i.lon, -72.029167, 1e-6);
        assert_eq!(i.symbol.as_deref(), Some("/>"));
        assert_eq!(i.course_deg, Some(88.0));
        assert_eq!(i.speed_kt, Some(36.0));
        near(i.alt_m, 376.1, 0.1);
        // `@` (messaging) with a timestamp reads the same.
        let j = parse_info("APRS", b"@092345/4903.50N/07201.75W>088/036");
        near(j.lat, 49.058333, 1e-6);
        assert_eq!(j.course_deg, Some(88.0));
        // An unknown course and speed ("..."): none.
        let k = parse_info("APRS", b"!4903.50N/07201.75W>.../...");
        assert_eq!((k.course_deg, k.speed_kt), (None, None));
    }

    #[test]
    fn ambiguity_centres_the_cell() {
        let i = parse_info("APRS", b"!4903.5 N/07201.7 W-");
        assert_eq!(i.data_type, DataType::Position);
        assert_eq!(i.ambiguity, 1);
        // The cell 49 03.50'-03.59' centred at 03.55'; the longitude takes the same ambiguity.
        near(i.lat, 49.058333 + 0.000833, 1e-5);
        near(i.lon, -(72.0 + 1.75 / 60.0), 1e-5);
        // Four spaces: the nearest degree, centred at 30'.
        let j = parse_info("APRS", b"!49  .  N/072  .  W-");
        assert_eq!(j.ambiguity, 4);
        near(j.lat, 49.5, 1e-6);
        near(j.lon, -72.5, 1e-6);
    }

    #[test]
    fn brisbane_airport_round_trips() {
        let i = parse_info("APRS", b"!2723.05S/15307.05E>ZetaSDR");
        assert_eq!(i.data_type, DataType::Position);
        near(i.lat, -27.3842, 0.0001);
        near(i.lon, 153.1175, 0.0001);
        assert_eq!(i.symbol.as_deref(), Some("/>"));
        assert_eq!(i.comment.as_deref(), Some("ZetaSDR"));
    }

    #[test]
    fn compressed_position() {
        let i = parse_info("APRS", b"=/5L!!<*e7>7P[");
        assert_eq!(i.data_type, DataType::Position);
        near(i.lat, 49.5, 0.0001);
        near(i.lon, -72.75, 0.0001);
        assert_eq!(i.symbol.as_deref(), Some("/>"));
        // cs "7P" with T "[" (an RMC fix): course 88, speed 1.08^47 - 1 = 36.2 kt.
        assert_eq!(i.course_deg, Some(88.0));
        near(i.speed_kt, 36.2, 0.05);
        assert_eq!(i.alt_m, None);
        assert_eq!(i.comment, None);
    }

    #[test]
    fn compressed_altitude_and_no_cs() {
        // T "S" (0x53 - 33 = 0b110010): a GGA fix, so cs "S]" is altitude 1.002^cs feet.
        let i = parse_info("APRS", b"!/5L!!<*e7OS]S");
        near(i.lat, 49.5, 0.0001);
        let cs = f64::from(u32::from(b'S' - 33) * 91 + u32::from(b']' - 33));
        near(i.alt_m, 1.002f64.powf(cs) * 0.3048, 0.01);
        assert_eq!((i.course_deg, i.speed_kt), (None, None));
        // A space for c: the cs bytes are ignored.
        let j = parse_info("APRS", b"!/5L!!<*e7>  !hello");
        near(j.lon, -72.75, 0.0001);
        assert_eq!((j.course_deg, j.speed_kt, j.alt_m), (None, None, None));
        assert_eq!(j.comment.as_deref(), Some("hello"));
    }

    #[test]
    fn mic_e_from_the_destination_and_info() {
        // Chapter 10's info bytes with the destination's longitude offset set (char 5 'V', the
        // digit 6 with +100): 33 25.64 N, 112 07.74 W, 20 kt, 251 degrees, a jeep.
        let i = parse_info("S32UVT", b"`(_fn\"Oj/");
        assert_eq!(i.data_type, DataType::MicE);
        near(i.lat, 33.427333, 1e-4);
        near(i.lon, -(112.0 + 7.74 / 60.0), 1e-4);
        near(i.speed_kt, 20.0, 0.5);
        near(i.course_deg, 251.0, 1.0);
        assert_eq!(i.symbol.as_deref(), Some("/j"));
        // The chapter's S32U6T has no offset (char 5 is a digit): 12 degrees, not 112.
        let j = parse_info("S32U6T-2", b"`(_fn\"Oj/");
        near(j.lat, 33.427333, 1e-4);
        near(j.lon, -(12.0 + 7.74 / 60.0), 1e-4);
        // A destination that is no Mic-E latitude: the type, no fix.
        let k = parse_info("APRS", b"`(_fn\"Oj/");
        assert_eq!(k.data_type, DataType::MicE);
        no_fix(&k);
    }

    #[test]
    fn mic_e_altitude_and_comment() {
        // "\"4T}" after the symbol: base-91 metres above 10 km below sea level.
        let i = parse_info("S32UVT", b"`(_fn\"Oj/\"4T}hello");
        let v = f64::from(
            u32::from(b'"' - 33) * 91 * 91 + u32::from(b'4' - 33) * 91 + u32::from(b'T' - 33),
        );
        near(i.alt_m, v - 10_000.0, 1e-9);
        assert_eq!(i.comment.as_deref(), Some("hello"));
    }

    #[test]
    fn objects_and_items_are_named_not_placed() {
        let o = parse_info("APRS", b";LEADER   *092345z4903.50N/07201.75W>088/036");
        assert_eq!(o.data_type, DataType::Object);
        assert_eq!(o.object.as_deref(), Some("LEADER"));
        no_fix(&o);
        let it = parse_info("APRS", b")AID #2!4903.50N/07201.75WA");
        assert_eq!(it.data_type, DataType::Item);
        assert_eq!(it.object.as_deref(), Some("AID #2"));
        no_fix(&it);
    }

    #[test]
    fn messages_status_telemetry_weather_set_the_type_only() {
        for (info, want) in [
            (&b":WU2Z     :Testing{003"[..], DataType::Message),
            (b">status", DataType::Status),
            (b"T#005,199,000,255,073,123,01101001", DataType::Telemetry),
            (b"_10090556c220s004g005t077", DataType::Weather),
            (b"<IGATE,MSG_CNT=1", DataType::Other),
            (b"", DataType::Other),
        ] {
            let i = parse_info("APRS", info);
            assert_eq!(i.data_type, want, "{}", String::from_utf8_lossy(info));
            no_fix(&i);
            assert_eq!((i.symbol.as_ref(), i.object.as_ref()), (None, None));
        }
    }

    #[test]
    fn a_malformed_position_gives_no_fix_and_keeps_the_packet() {
        for info in [
            &b"!garbage"[..],
            b"=4903.5",
            b"!",
            b"/092345z",
            b"!9903.50N/07201.75W-",
            b"!4963.50N/07201.75W-",
            b"!4903.50N/18101.75W-",
            b"!4903.50X/07201.75W-",
            b"!4903. 0N/07201.75W-",
            b"=/5L!",
            b"!\xff\xfe\xfd",
        ] {
            let i = parse_info("APRS", info);
            assert_eq!(
                i.data_type,
                DataType::Position,
                "{}",
                String::from_utf8_lossy(info)
            );
            no_fix(&i);
        }
    }

    #[test]
    fn fields_are_the_nine_event_fields() {
        let m = parse_info("APRS", b"!2723.05S/15307.05E>ZetaSDR").fields();
        let mut keys: Vec<&str> = m.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut want = [
            "lat",
            "lon",
            "alt_m",
            "course_deg",
            "speed_kt",
            "symbol",
            "comment",
            "data_type",
            "object",
        ];
        want.sort_unstable();
        assert_eq!(keys, want);
        assert_eq!(m["data_type"], "position");
        assert_eq!(m["symbol"], "/>");
        assert_eq!(m["alt_m"], serde_json::Value::Null);
        let s = parse_info("APRS", b">hi").fields();
        assert_eq!(s["data_type"], "status");
        assert_eq!(s["lat"], serde_json::Value::Null);
        assert_eq!(parse_info("X", b"`").fields()["data_type"], "mic_e");
    }
}
