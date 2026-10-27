//! A decoded event's envelope: `protocol`, `summary` and `t_ns` on top of the
//! decoder's own fields, the summary filled from the mode row's template.

use serde_json::Value;

use super::ModeRow;

/// What a template drops with an absent optional field: the separator run just before it.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '·' | ':' | '>' | ',')
}

fn present<'a>(event: &'a Value, field: &str) -> Option<&'a Value> {
    event.get(field).filter(|v| !v.is_null())
}

/// A value's text: strings unquoted, numbers as printed, booleans `true`/`false`,
/// `null` the literal `null`, nested values compact JSON.
fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// `template` filled from `event`'s fields: `{field}` is the field's text (`null` when
/// absent or null); `{field?}` its text, or nothing together with the separator run
/// (spaces, `·`, `:`, `>`, `,`) of the template just before it when it is absent or
/// null; `{a?b}` is `a` when present and non-null, else `b` as `{b}`. Trailing
/// whitespace is trimmed.
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
        match inner.split_once('?') {
            Some((a, "")) => match present(event, a) {
                Some(v) => out.push_str(&text(Some(v))),
                None => {
                    let kept = out[literal_from..].trim_end_matches(is_separator).len();
                    out.truncate(literal_from + kept);
                }
            },
            Some((a, b)) => match present(event, a) {
                Some(v) => out.push_str(&text(Some(v))),
                None => out.push_str(&text(event.get(b))),
            },
            None => out.push_str(&text(event.get(inner))),
        }
        literal_from = out.len();
    }
    out.push_str(rest);
    out.truncate(out.trim_end().len());
    out
}

/// `fields` with `protocol` (the row's), `summary` (its template filled from `fields`)
/// and `t_ns` (`start_ns`) set; every field is kept.
pub fn envelope(row: &ModeRow, start_ns: u64, mut fields: Value) -> Value {
    let summary = fill(&row.event.summary, &fields);
    if let Value::Object(m) = &mut fields {
        m.insert("protocol".into(), row.protocol.clone().into());
        m.insert("summary".into(), summary.into());
        m.insert("t_ns".into(), start_ns.into());
    }
    fields
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
    fn alternative_picks_the_present_one() {
        assert_eq!(fill("{a?b}", &json!({"a": "x", "b": "y"})), "x");
        assert_eq!(fill("{a?b}", &json!({"a": null, "b": "y"})), "y");
        assert_eq!(fill("{a?b}", &json!({})), "null");
    }
}
