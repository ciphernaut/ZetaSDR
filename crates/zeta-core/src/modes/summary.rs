//! A decoded event's envelope: `protocol`, `summary` and `t_ns` on top of the
//! decoder's own fields, the summary filled from the mode row's template, plus a
//! normalised `position` when the row maps one.

use serde_json::Value;

use super::{AltUnit, ModeRow};

/// Metres in a foot.
const FT_M: f64 = 0.3048;

/// What a template drops with an absent optional field: the separator run just before it.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '·' | ':' | '>' | ',')
}

fn present<'a>(event: &'a Value, field: &str) -> Option<&'a Value> {
    event.get(field).filter(|v| !v.is_null())
}

/// A summary placeholder's number format: `:.N` or `:+.N` (`N` 0-6 decimals, `+` a
/// sign on positive numbers too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NumberFormat {
    plus: bool,
    decimals: usize,
}

/// The highest `N` of a `:.N` format.
const MAX_DECIMALS: usize = 6;

/// A placeholder's inside split into its names (`a`, `a?`, `a?b`) and its number
/// format, when it ends in a well-formed `:.N` or `:+.N`; `None` otherwise.
pub(super) fn split_format(inner: &str) -> Option<(&str, NumberFormat)> {
    let (names, spec) = inner.rsplit_once(':')?;
    let (plus, spec) = match spec.strip_prefix('+') {
        Some(s) => (true, s),
        None => (false, spec),
    };
    let decimals = spec
        .strip_prefix('.')
        .filter(|d| d.len() == 1)
        .and_then(|d| d.parse::<usize>().ok())
        .filter(|&d| d <= MAX_DECIMALS)?;
    Some((names, NumberFormat { plus, decimals }))
}

/// A value's text: strings unquoted, numbers as printed (or under `format` when one is
/// given), booleans `true`/`false`, `null` the literal `null`, nested values compact JSON.
fn text(v: Option<&Value>, format: Option<NumberFormat>) -> String {
    match (v, format) {
        (None | Some(Value::Null), _) => "null".into(),
        (Some(Value::String(s)), _) => s.clone(),
        (Some(Value::Number(n)), Some(f)) => match (n.as_f64(), f.plus) {
            (Some(x), true) => format!("{x:+.*}", f.decimals),
            (Some(x), false) => format!("{x:.*}", f.decimals),
            (None, _) => n.to_string(),
        },
        (Some(v), _) => v.to_string(),
    }
}

/// `template` filled from `event`'s fields: `{field}` is the field's text (`null` when
/// absent or null); `{field?}` its text, or nothing together with the separator run
/// (spaces, `·`, `:`, `>`, `,`) of the template just before it when it is absent or
/// null; `{a?b}` is `a` when present and non-null, else `b` as `{b}`. Any of these may
/// end in a number format, `{field:.N}` or `{field:+.N}` (`N` 0-6; `{lat?:.4}`), which
/// prints a number with `N` decimals (and a sign with `+`); a value that is not a
/// number renders as without it. Leading and trailing whitespace is trimmed (a template
/// whose leading optional fields are absent starts at its first present one).
pub fn fill(template: &str, event: &Value) -> String {
    let mut out = String::new();
    // Where the literal text after the last substituted value begins: only that
    // text is separator material, never the end of a value.
    let mut literal_from = 0;
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(len) = rest[open..].find('}') else {
            break;
        };
        out.push_str(&rest[..open]);
        let inner = &rest[open + 1..open + len];
        rest = &rest[open + len + 1..];
        let (names, format) = match split_format(inner) {
            Some((names, f)) => (names, Some(f)),
            None => (inner, None),
        };
        match names.split_once('?') {
            Some((a, "")) => match present(event, a) {
                Some(v) => out.push_str(&text(Some(v), format)),
                None => {
                    let kept = out[literal_from..].trim_end_matches(is_separator).len();
                    out.truncate(literal_from + kept);
                }
            },
            Some((a, b)) => match present(event, a) {
                Some(v) => out.push_str(&text(Some(v), format)),
                None => out.push_str(&text(event.get(b), format)),
            },
            None => out.push_str(&text(event.get(names), format)),
        }
        literal_from = out.len();
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// `fields` with `protocol` (the row's), `summary` (its template filled from `fields`)
/// and `t_ns` set; every field is kept. `t_ns` is the time in the row's `time` field
/// when the event carries a well-formed one, else `start_ns`. With a row `position`
/// map and its `lat` and `lon` fields both numbers, `position` is
/// `{lat, lon, alt_m}` (`alt_m` `null` unless its field is a number, converted from feet
/// when the map's `alt_unit` is `ft`).
pub fn envelope(row: &ModeRow, start_ns: u64, mut fields: Value) -> Value {
    let summary = fill(&row.event.summary, &fields);
    let t_ns = row
        .time
        .as_deref()
        .and_then(|t| fields.get(t))
        .and_then(Value::as_str)
        .and_then(parse_utc_ns)
        .unwrap_or(start_ns);
    let number = |name: &str| fields.get(name).filter(|v| v.is_number()).cloned();
    let position = row.position.as_ref().and_then(|p| {
        let alt_m = p
            .alt_m
            .as_deref()
            .and_then(number)
            .map(|a| match (p.alt_unit, a.as_f64()) {
                (AltUnit::Ft, Some(ft)) => Value::from(ft * FT_M),
                _ => a,
            })
            .unwrap_or(Value::Null);
        Some(serde_json::json!({
            "lat": number(&p.lat)?,
            "lon": number(&p.lon)?,
            "alt_m": alt_m,
        }))
    });
    if let Value::Object(m) = &mut fields {
        m.insert("protocol".into(), row.protocol.clone().into());
        m.insert("summary".into(), summary.into());
        m.insert("t_ns".into(), t_ns.into());
        if let Some(p) = position {
            m.insert("position".into(), p);
        }
        if let Some(a) = row.access {
            m.insert("access".into(), a.word().into());
        }
    }
    fields
}

/// `YYYY-MM-DDTHH:MM:SS[.f]Z` (UTC, 1-9 fraction digits) as nanoseconds since the
/// UNIX epoch; `None` when malformed, out of range or before 1970.
fn parse_utc_ns(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let digits = |from: usize, len: usize| -> Option<u64> {
        let d = b.get(from..from + len)?;
        d.iter()
            .all(u8::is_ascii_digit)
            .then(|| d.iter().fold(0_u64, |n, c| n * 10 + u64::from(c - b'0')))
    };
    let punct = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if punct.iter().any(|&(i, c)| b.get(i) != Some(&c)) {
        return None;
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    let frac_ns = match &b[19..] {
        [b'Z'] => 0,
        [b'.', f @ .., b'Z'] if (1..=9).contains(&f.len()) => {
            digits(20, f.len())? * 10_u64.pow(9 - f.len() as u32)
        }
        _ => return None,
    };
    if year < 1970
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
    secs.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to a date on or after it (H. Hinnant's `days_from_civil`).
fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fills_fields() {
        let ev =
            json!({"src": "VK3ABC", "dst": "APRS", "n": 7, "ok": true, "o": {"a": 1}, "z": null});
        assert_eq!(
            fill("{src} > {dst}: {n} {ok}", &ev),
            "VK3ABC > APRS: 7 true"
        );
        assert_eq!(fill("{z} {absent} {o}", &ev), "null null {\"a\":1}");
    }

    #[test]
    fn optional_drops_its_separator() {
        // Absent leading optionals leave no leading separator (hfdl_decode's summary).
        let hfdl = "{icao?} {gs_id?} {gs?} {flight?} {pdu?} {text?}";
        assert_eq!(
            fill(hfdl, &json!({"pdu": "Logon confirm"})),
            "Logon confirm"
        );
        assert_eq!(fill(hfdl, &json!({"icao": null, "gs_id": 4})), "4");
        let ev = json!({"ps": "ZETA FM", "pi": "2AB1", "rt": null});
        assert_eq!(fill("{ps?pi} · {rt?}", &ev), "ZETA FM");
        let ev = json!({"pi": "2AB1", "rt": "hello"});
        assert_eq!(fill("{ps?pi} · {rt?}", &ev), "2AB1 · hello");
        let ev = json!({"icao": "7C6B2E", "lat": -37.8});
        assert_eq!(
            fill("{icao} {callsign?} {alt?} {lat?},{lon?}", &ev),
            "7C6B2E -37.8"
        );
        let ev = json!({"icao": "7C6B2E"});
        assert_eq!(
            fill("{icao} {callsign?} {alt?} {lat?},{lon?}", &ev),
            "7C6B2E"
        );
        let ev = json!({"address": 1234567, "text": "HELLO ZETA"});
        assert_eq!(fill("{address}: {text?}", &ev), "1234567: HELLO ZETA");
        // A value ending in a separator keeps it.
        let ev = json!({"a": "x:", "b": null});
        assert_eq!(fill("{a}{b?}", &ev), "x:");
    }

    #[test]
    fn summary_with_a_missing_field_renders_empty() {
        let ev = json!({"a": 1});
        assert_eq!(fill("{a} {b?}", &ev), "1");
        assert_eq!(fill("{a} {b}", &ev), "1 null");
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(
            fill(
                "{alt:.0} m {vel_v:+.1}",
                &json!({"alt": 12000.4, "vel_v": 5.04})
            ),
            "12000 m +5.0"
        );
        assert_eq!(
            fill("{id} {lat?:.4}", &json!({"id": "S1", "lat": -27.41234})),
            "S1 -27.4123"
        );
        // An absent optional drops with its separator, format or not.
        assert_eq!(fill("{id} {lat?:.4}", &json!({"id": "S1"})), "S1");
        // A non-number renders as text; an integer takes the format too.
        assert_eq!(fill("{id:.1}", &json!({"id": "S1"})), "S1");
        assert_eq!(fill("{n:+.2}", &json!({"n": 3})), "+3.00");
        assert_eq!(fill("{a?b:.1}", &json!({"b": 2.26})), "2.3");
        // A suffix that is not a format is part of the name, as today.
        assert_eq!(fill("{a:x}", &json!({"a": 1})), "null");
    }

    /// The embedded ADS-B row with `edit` applied.
    fn row(edit: impl FnOnce(&mut Value)) -> ModeRow {
        let (_, text) = super::super::ModeRegistry::embedded()
            .into_iter()
            .find(|(f, _)| f.ends_with("adsb_decode.json"))
            .unwrap();
        let mut v: Value = serde_json::from_str(text).unwrap();
        edit(&mut v);
        super::super::parse(&v.to_string()).unwrap()
    }

    #[test]
    fn a_restricted_row_marks_its_events() {
        let ev = envelope(
            &row(|v| v["access"] = "restricted".into()),
            7,
            json!({"icao": "7C6B28"}),
        );
        assert_eq!(ev["access"], "restricted");
    }

    #[test]
    fn a_plain_row_adds_no_marker() {
        let ev = envelope(&row(|_| {}), 7, json!({"icao": "7C6B28"}));
        assert!(ev.get("access").is_none());
    }

    fn positioned() -> ModeRow {
        row(|v| v["position"] = json!({"lat": "lat", "lon": "lon", "alt_m": "alt"}))
    }

    #[test]
    fn alt_unit_ft_converts_in_position() {
        let ev = envelope(
            &row(|_| {}),
            7,
            json!({"icao": "7C6B28", "lat": -27.4, "lon": 153.1, "altitude_ft": 21000}),
        );
        let alt = ev["position"]["alt_m"].as_f64().unwrap();
        assert!((alt - 6400.8).abs() < 0.001, "{alt}");
        assert_eq!(ev["position"]["lat"], -27.4);
        assert_eq!(ev["position"]["lon"], 153.1);
        assert_eq!(ev["altitude_ft"], 21000, "the event keeps its own unit");
    }

    #[test]
    fn a_fix_with_no_altitude_is_a_partial_position() {
        let ev = envelope(
            &row(|_| {}),
            7,
            json!({"icao": "S1", "lat": -27.4, "lon": 153.1, "altitude_ft": null}),
        );
        assert_eq!(ev["position"]["alt_m"], Value::Null);
        assert_eq!(ev["position"]["lat"], -27.4);
        let ev = envelope(
            &row(|_| {}),
            7,
            json!({"icao": "S1", "lat": null, "lon": 153.1, "altitude_ft": 100}),
        );
        assert!(ev.get("position").is_none(), "{ev}");
    }

    #[test]
    fn position_added_from_the_map() {
        let ev = envelope(
            &positioned(),
            7,
            json!({"icao": "S1", "lat": -27.4, "lon": 153.1, "alt": 12000.5}),
        );
        assert_eq!(
            ev["position"],
            json!({"lat": -27.4, "lon": 153.1, "alt_m": 12000.5})
        );
        assert_eq!(ev["lat"], -27.4, "the fields are kept");
        // A row without a map adds nothing.
        let ev = envelope(
            &row(|v| v["position"] = Value::Null),
            7,
            json!({"lat": -27.4, "lon": 153.1}),
        );
        assert!(ev.get("position").is_none(), "{ev}");
    }

    #[test]
    fn t_ns_comes_from_the_time_field() {
        let timed = row(|v| {
            v["time"] = "datetime".into();
            v["event"]["schema"]["properties"]["datetime"] = json!({"type": "string"});
        });
        let ev = envelope(&timed, 7, json!({"datetime": "2026-10-28T02:15:30.500Z"}));
        assert_eq!(ev["t_ns"], 1_793_153_730_500_000_000_u64);
        let ev = envelope(&timed, 7, json!({"datetime": "1970-01-01T00:00:01Z"}));
        assert_eq!(ev["t_ns"], 1_000_000_000_u64);
        let ev = envelope(
            &timed,
            7,
            json!({"datetime": "2024-02-29T00:00:00.123456789Z"}),
        );
        assert_eq!(ev["t_ns"], 1_709_164_800_123_456_789_u64);
        for bad in [
            json!("2026-10-28 02:15:30Z"),
            json!("2026-10-28T02:15:30"),
            json!("2026-13-28T02:15:30Z"),
            json!("2026-02-29T02:15:30Z"),
            json!("2026-10-28T24:00:00Z"),
            json!("2026-10-28T02:15:30.Z"),
            json!("1969-12-31T23:59:59Z"),
            json!(1_793_153_730),
            Value::Null,
        ] {
            let ev = envelope(&timed, 7, json!({ "datetime": bad }));
            assert_eq!(ev["t_ns"], 7, "{bad}");
        }
        let ev = envelope(&timed, 7, json!({}));
        assert_eq!(ev["t_ns"], 7);
    }

    #[test]
    fn an_event_without_a_fix_gets_no_position() {
        let row = positioned();
        let ev = envelope(&row, 7, json!({"lon": 153.1, "alt": 100.0}));
        assert!(ev.get("position").is_none(), "{ev}");
        let ev = envelope(&row, 7, json!({"lat": null, "lon": 153.1}));
        assert!(ev.get("position").is_none(), "{ev}");
        let ev = envelope(&row, 7, json!({"lat": "x", "lon": 153.1}));
        assert!(ev.get("position").is_none(), "{ev}");
        let ev = envelope(&row, 7, json!({"lat": -27.4, "lon": 153.1}));
        assert_eq!(
            ev["position"],
            json!({"lat": -27.4, "lon": 153.1, "alt_m": null})
        );
    }

    #[test]
    fn alternative_picks_the_present_one() {
        assert_eq!(fill("{a?b}", &json!({"a": "x", "b": "y"})), "x");
        assert_eq!(fill("{a?b}", &json!({"a": null, "b": "y"})), "y");
        assert_eq!(fill("{a?b}", &json!({})), "null");
    }
}
