//! A row's `derive` step: event fields read out of another field's text by a named
//! extractor, run by [`envelope`](super::summary::envelope) before anything else it does.
//!
//! The one extractor, `acars_position`, finds the first position report in an ACARS
//! text (spec 4.3), scanning left to right and trying at each place, in order:
//!
//! 1. the compact degrees and minutes right after a `POS` token (H1 FMS reports,
//!    `POSS27231E153071,...`; label 80, `/POS S27231E153071/ALT 255`), minutes times ten;
//! 2. decimal degrees with hemisphere letters, the axes apart by spaces or one `/`
//!    (labels 16 and 3L, `S 27.384 E153.118`);
//! 3. degrees and minutes to a tenth, run together (QQ, `S2723.1E15307.1`).
//!
//! Anything else is not a fix: no regex, a byte scanner per form.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{Derive, Extractor};

/// Feet in a flight level.
pub const FT_PER_FL: i64 = 100;

/// `which`'s outputs found in `text`, by output name; an output not found is absent.
pub fn extract(which: Extractor, text: &str) -> BTreeMap<&'static str, Value> {
    match which {
        Extractor::AcarsPosition => acars_position(text),
    }
}

/// Runs each entry of `derive` on `fields`, in order (a later entry sees an earlier
/// one's fields). An entry runs when every `when` pair's field is present and equal,
/// `from` holds a string and none of the `into` targets holds a non-null value; it
/// writes only the outputs the extractor found.
pub fn apply(derive: &[Derive], fields: &mut Map<String, Value>) {
    for entry in derive {
        let holds = entry.when.iter().all(|(k, v)| fields.get(k) == Some(v));
        let taken = entry
            .into
            .values()
            .any(|f| fields.get(f).is_some_and(|v| !v.is_null()));
        if !holds || taken {
            continue;
        }
        let Some(Value::String(text)) = fields.get(&entry.from) else {
            continue;
        };
        let found = extract(entry.extract, text);
        for (output, field) in &entry.into {
            if let Some(v) = found.get(output.as_str()) {
                fields.insert(field.clone(), v.clone());
            }
        }
    }
}

/// A fix as read: signed degrees, and a flight level's feet when the form carries one.
struct Fix {
    lat: f64,
    lon: f64,
    alt_ft: Option<i64>,
}

fn acars_position(text: &str) -> BTreeMap<&'static str, Value> {
    let b = text.as_bytes();
    let fix = (0..b.len()).find_map(|i| {
        compact(b, i)
            .or_else(|| decimal(b, i))
            .or_else(|| run_together(b, i))
    });
    let mut out = BTreeMap::new();
    if let Some(f) = fix {
        out.insert("lat", Value::from(round5(f.lat)));
        out.insert("lon", Value::from(round5(f.lon)));
        if let Some(ft) = f.alt_ft {
            out.insert("alt_ft", Value::from(ft));
        }
    }
    out
}

/// `x` to five decimals, about a metre.
fn round5(x: f64) -> f64 {
    (x * 1e5).round() / 1e5
}

/// The value of exactly `n` ASCII digits at `at`.
fn digits(b: &[u8], at: usize, n: usize) -> Option<u32> {
    let d = b.get(at..at + n)?;
    d.iter()
        .all(u8::is_ascii_digit)
        .then(|| d.iter().fold(0, |v, c| v * 10 + u32::from(c - b'0')))
}

/// How many ASCII digits run from `at`.
fn run_of_digits(b: &[u8], at: usize) -> usize {
    b.get(at..)
        .map_or(0, |s| s.iter().take_while(|c| c.is_ascii_digit()).count())
}

/// `1.0` for the letter `pos` at `at`, `-1.0` for `neg`.
fn hemisphere(b: &[u8], at: usize, pos: u8, neg: u8) -> Option<f64> {
    match b.get(at) {
        Some(&c) if c == pos => Some(1.0),
        Some(&c) if c == neg => Some(-1.0),
        _ => None,
    }
}

/// The hemisphere letter at `at` starts a word: it is the first byte or follows a
/// byte that is not a letter (a digit or punctuation may run into it, as in QQ).
fn starts_word(b: &[u8], at: usize) -> bool {
    at == 0 || !b[at - 1].is_ascii_alphabetic()
}

/// `lat` and `lon` from unsigned degrees and their signs, when both are in range.
fn in_range(lat_sign: f64, lat: f64, lon_sign: f64, lon: f64) -> Option<(f64, f64)> {
    (lat < 90.0 && lon < 180.0).then_some((lat_sign * lat, lon_sign * lon))
}

/// Form 1 at `i`: `POS`, at most one space, `N|S` `dd` `mmm`, at most one space,
/// `E|W` `ddd` `mmm` (minutes times ten, under 600). The altitude is the H1 report's
/// third comma field after the fix when it is three digits, else, when the anchor is
/// `/POS`, the label 80 report's `/ALT <n>` (two or three digits) before the next
/// `POS`; both are flight levels.
fn compact(b: &[u8], i: usize) -> Option<Fix> {
    if !b[i..].starts_with(b"POS") {
        return None;
    }
    let skip_space = |j: usize| if b.get(j) == Some(&b' ') { j + 1 } else { j };
    let j = skip_space(i + 3);
    let lat_sign = hemisphere(b, j, b'N', b'S')?;
    let (lat_deg, lat_min) = (digits(b, j + 1, 2)?, digits(b, j + 3, 3)?);
    let j = skip_space(j + 6);
    let lon_sign = hemisphere(b, j, b'E', b'W')?;
    let (lon_deg, lon_min) = (digits(b, j + 1, 3)?, digits(b, j + 4, 3)?);
    if lat_min >= 600 || lon_min >= 600 {
        return None;
    }
    let deg = |d: u32, m: u32| f64::from(d) + f64::from(m) / 600.0;
    let (lat, lon) = in_range(
        lat_sign,
        deg(lat_deg, lat_min),
        lon_sign,
        deg(lon_deg, lon_min),
    )?;
    let rest = &b[j + 7..];
    let alt_ft = match rest.strip_prefix(b",") {
        Some(fields) => fields
            .split(|&c| c == b',')
            .nth(2)
            .filter(|f| f.len() == 3 && f.iter().all(u8::is_ascii_digit))
            .and_then(|f| digits(f, 0, 3)),
        None if i > 0 && b[i - 1] == b'/' => slash_alt(rest),
        None => None,
    }
    .map(|fl| i64::from(fl) * FT_PER_FL);
    Some(Fix { lat, lon, alt_ft })
}

/// The flight level of the first `/ALT <n>` in `rest` before its next `POS` token (a
/// later report's), `n` two or three digits.
fn slash_alt(rest: &[u8]) -> Option<u32> {
    let report = rest
        .windows(3)
        .position(|w| w == b"POS")
        .unwrap_or(rest.len());
    let rest = &rest[..report];
    let at = rest.windows(4).position(|w| w == b"/ALT")? + 4;
    let at = if rest.get(at) == Some(&b' ') {
        at + 1
    } else {
        at
    };
    let n = run_of_digits(rest, at);
    (2..=3).contains(&n).then(|| digits(rest, at, n)).flatten()
}

/// A decimal number at `at` with `int` integer digits and one to four decimals: its
/// value and the index after it.
fn decimal_number(
    b: &[u8],
    at: usize,
    int: std::ops::RangeInclusive<usize>,
) -> Option<(f64, usize)> {
    let n = run_of_digits(b, at);
    if !int.contains(&n) || b.get(at + n) != Some(&b'.') {
        return None;
    }
    let frac = run_of_digits(b, at + n + 1);
    if !(1..=4).contains(&frac) {
        return None;
    }
    let end = at + n + 1 + frac;
    let value = std::str::from_utf8(&b[at..end]).ok()?.parse().ok()?;
    Some((value, end))
}

/// Form 2 at `i`: `N|S`, an optional space, `d.d` (one or two integer digits), one or
/// more spaces or one `/`, `E|W`, an optional space, `d.d` (one to three integer digits).
fn decimal(b: &[u8], i: usize) -> Option<Fix> {
    if !starts_word(b, i) {
        return None;
    }
    let skip_space = |j: usize| if b.get(j) == Some(&b' ') { j + 1 } else { j };
    let lat_sign = hemisphere(b, i, b'N', b'S')?;
    let (lat, j) = decimal_number(b, skip_space(i + 1), 1..=2)?;
    let j = match b.get(j) {
        Some(b'/') => j + 1,
        Some(b' ') => j + b[j..].iter().take_while(|&&c| c == b' ').count(),
        _ => return None,
    };
    let lon_sign = hemisphere(b, j, b'E', b'W')?;
    let (lon, _) = decimal_number(b, skip_space(j + 1), 1..=3)?;
    let (lat, lon) = in_range(lat_sign, lat, lon_sign, lon)?;
    Some(Fix {
        lat,
        lon,
        alt_ft: None,
    })
}

/// Form 3 at `i`: `N|S` `dd` `mm.m` `E|W` `ddd` `mm.m`, run together (minutes under 60).
fn run_together(b: &[u8], i: usize) -> Option<Fix> {
    if !starts_word(b, i) {
        return None;
    }
    // Degrees of `n` digits, then minutes to a tenth, from `at`.
    let axis = |at: usize, n: usize| -> Option<f64> {
        let deg = digits(b, at, n)?;
        let min = digits(b, at + n, 2)?;
        let tenth = (b.get(at + n + 2) == Some(&b'.'))
            .then(|| digits(b, at + n + 3, 1))
            .flatten()?;
        (min < 60).then(|| f64::from(deg) + (f64::from(min) + f64::from(tenth) / 10.0) / 60.0)
    };
    let lat_sign = hemisphere(b, i, b'N', b'S')?;
    let lat = axis(i + 1, 2)?;
    let lon_sign = hemisphere(b, i + 7, b'E', b'W')?;
    let lon = axis(i + 8, 3)?;
    let (lat, lon) = in_range(lat_sign, lat, lon_sign, lon)?;
    Some(Fix {
        lat,
        lon,
        alt_ft: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `acars_position` outputs of `text`.
    fn pos(text: &str) -> BTreeMap<&'static str, Value> {
        extract(Extractor::AcarsPosition, text)
    }

    /// Asserts `text` yields the fix `lat`, `lon` (to 1e-5) and the altitude `alt_ft`
    /// (absent when `None`).
    #[track_caller]
    fn assert_fix(text: &str, lat: f64, lon: f64, alt_ft: Option<i64>) {
        let got = pos(text);
        let axis = |k: &str| got.get(k).and_then(Value::as_f64);
        let (Some(la), Some(lo)) = (axis("lat"), axis("lon")) else {
            panic!("no fix in {text:?}: {got:?}");
        };
        assert!((la - lat).abs() < 1e-5, "lat {la} != {lat} in {text:?}");
        assert!((lo - lon).abs() < 1e-5, "lon {lo} != {lon} in {text:?}");
        match alt_ft {
            Some(ft) => assert_eq!(got.get("alt_ft"), Some(&Value::from(ft)), "{text:?}"),
            None => assert!(!got.contains_key("alt_ft"), "{text:?}: {got:?}"),
        }
    }

    #[track_caller]
    fn assert_none(text: &str) {
        let got = pos(text);
        assert!(got.is_empty(), "{text:?}: {got:?}");
    }

    #[test]
    fn h1_fms_report_at_the_demo_station() {
        assert_fix(
            "#M1BPOSS27231E153071,ALPHA,025131,165,BRAVO,025351,CHARL,M9,252035,168",
            -27.385,
            153.11833,
            Some(16_500),
        );
    }

    #[test]
    fn h1_without_the_sublabel_prefix() {
        assert_fix(
            "POSS27231E153071,ALPHA,025131,165,BRAVO",
            -27.385,
            153.11833,
            Some(16_500),
        );
    }

    #[test]
    fn h1_north_west_variant() {
        assert_fix(
            "POSN27231W153071,ALPHA,025131,165",
            27.385,
            -153.11833,
            Some(16_500),
        );
    }

    #[test]
    fn h1_third_field_not_three_digits_has_no_alt() {
        assert_fix(
            "POSS27231E153071,ALPHA,025131,16,BRAVO",
            -27.385,
            153.11833,
            None,
        );
        assert_fix(
            "POSS27231E153071,ALPHA,025131,1650,BRAVO",
            -27.385,
            153.11833,
            None,
        );
    }

    #[test]
    fn label_80_slash_pos_and_alt() {
        assert_fix(
            "3N01 POSRPT 0054/09 YBBN/YSSY VH-ZET\r\n/POS S27231E153071/ALT 255/MCH 743/FOB 133.7",
            -27.385,
            153.11833,
            Some(25_500),
        );
    }

    #[test]
    fn two_reports_the_first_without_alt() {
        // The second report's `/ALT` is never the first fix's altitude.
        assert_fix(
            "POSS27231E153071 REPORT /POS N10000W020000/ALT 300",
            -27.385,
            153.11833,
            None,
        );
    }

    #[test]
    fn alt_without_the_slash_pos_anchor_is_ignored() {
        assert_fix("POSS27231E153071 TEXT /ALT 12", -27.385, 153.11833, None);
    }

    #[test]
    fn label_16_decimal_with_spaces() {
        assert_fix(
            "194606,28825,2106,  91,S 27.384 E153.118",
            -27.384,
            153.118,
            None,
        );
    }

    #[test]
    fn label_3l_decimal_with_a_slash() {
        assert_fix("S 27.384/E153.118", -27.384, 153.118, None);
    }

    #[test]
    fn qq_run_together() {
        assert_fix(
            "YBBNYSSY 2003\r\n001FE09200331S2723.1E15307.1017214",
            -27.385,
            153.11833,
            None,
        );
    }

    #[test]
    fn a_fix_in_the_middle_and_the_first_of_two_wins() {
        assert_fix(
            "REPORT POSS27231E153071,ALPHA,025131,165,BRAVO THEN POSN10000W020000,B,0,100",
            -27.385,
            153.11833,
            Some(16_500),
        );
        // The first in the text wins, whatever the form of either.
        assert_fix(
            "AT S 27.384 E153.118 ALSO POSN10000W020000,B,0,100",
            -27.384,
            153.118,
            None,
        );
    }

    #[test]
    fn an_empty_text_yields_nothing() {
        assert_none("");
    }

    #[test]
    fn rejects_the_compact_form_without_the_pos_token() {
        assert_none("S27231E153071 ALPHA");
    }

    #[test]
    fn rejects_what_is_not_a_fix() {
        for text in [
            "-24.0-49.7 369900.760",
            "FOB 133.7",
            "025131",
            "131550",
            "POSRPT 0054/09",
            // Minutes 650 and 650, each with its anchor.
            "POSS27650E153071",
            "POSS27231E153650",
            "LANDING123456E",
            "LAT LON\r\n-27.3842 153.1175",
            // The decimal axes apart by anything but spaces or one slash.
            "S 27.384 - E153.118",
            "S 27.384//E153.118",
            // Run-together minutes 63.
            "S2763.1E15307.1",
        ] {
            assert_none(text);
        }
    }

    #[test]
    fn range_is_checked() {
        assert_none("POSS90001E153071");
        assert_none("POSS27231E180001");
        assert_fix(
            "POSS89599E179599",
            -(89.0 + 59.9 / 60.0),
            179.0 + 59.9 / 60.0,
            None,
        );
    }
}
