//! `multimon`: multimon-ng's text output. A POCSAG line,
//! `POCSAG<baud>: Address: <n>  Function: <f>  Alpha:   <text>` (or `Numeric:`,
//! or neither for a tone-only page), becomes `{address, function, baud, kind,
//! text}` with `kind` `alpha`, `numeric` or `null` (and `text` `null` with it);
//! A FLEX line, in either form multimon-ng prints (FLEX_NEXT,
//! `FLEX|<time>|<baud>/<levels>/<K|C>/<phase>|<cycle>.<frame>|<capcode>|<kind>|<text>`, or the
//! older `FLEX: <time> <baud>/<levels>/<K> <cycle>.<frame> [<capcode>] <kind> <text>`), becomes
//! `{capcode, baud, levels, phase, cycle, frame, kind, text}` (`phase` `null` in the older form;
//! `kind` `alpha`, `numeric`, `tone`, `binary` or `null`; multimon's timestamp, the host clock,
//! is dropped). Every other non-empty line, and a FLEX line that does not parse, is
//! `{"text": line}`.

use serde_json::{Value, json};

/// The fields of one multimon-ng line; `None` for an empty one.
pub fn parse(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    Some(
        pocsag(line)
            .or_else(|| flex(line))
            .unwrap_or_else(|| json!({ "text": line })),
    )
}

/// The `|`-separated fields of a FLEX_NEXT line after the `FLEX` tag, text last (so a `|` in
/// the text survives): time, rate, slot, capcode, kind, text.
const FLEX_NEXT_FIELDS: usize = 6;

/// `<baud>/<levels>/<K|C>[/<phase>]` into `(baud, levels, phase)`.
fn flex_rate(s: &str) -> Option<(u32, u32, Option<&str>)> {
    let mut p = s.split('/');
    let baud = p.next()?.parse().ok()?;
    let levels = p.next()?.parse().ok()?;
    p.next()?; // the K / C mode letter
    let phase = p.next();
    if p.next().is_some() {
        return None;
    }
    Some((baud, levels, phase))
}

/// `<cycle>.<frame>`.
fn flex_slot(s: &str) -> Option<(u32, u32)> {
    let (cycle, frame) = s.split_once('.')?;
    Some((cycle.parse().ok()?, frame.parse().ok()?))
}

/// ALN, NUM, TON, BIN.
fn flex_kind(s: &str) -> Option<&'static str> {
    match s {
        "ALN" => Some("alpha"),
        "NUM" => Some("numeric"),
        "TON" => Some("tone"),
        "BIN" => Some("binary"),
        _ => None,
    }
}

fn flex(line: &str) -> Option<Value> {
    let (rate, slot, capcode, kind, text) = if let Some(rest) = line.strip_prefix("FLEX|") {
        let mut f = rest.splitn(FLEX_NEXT_FIELDS, '|');
        let _time = f.next()?;
        let rate = f.next()?;
        let slot = f.next()?;
        let capcode = f.next()?;
        let kind = f.next()?;
        let text = f.next()?;
        (rate, slot, capcode, kind, text)
    } else {
        // The older form: the time is a date and a clock, two words.
        let rest = line.strip_prefix("FLEX:")?.trim_start();
        let (_date, rest) = rest.split_once(char::is_whitespace)?;
        let (_clock, rest) = rest.trim_start().split_once(char::is_whitespace)?;
        let (rate, rest) = rest.trim_start().split_once(char::is_whitespace)?;
        let (slot, rest) = rest.trim_start().split_once(char::is_whitespace)?;
        let rest = rest.trim_start().strip_prefix('[')?;
        let (capcode, rest) = rest.split_once(']')?;
        let rest = rest.trim_start();
        let (kind, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        (rate, slot, capcode, kind, text.trim_start())
    };
    let (baud, levels, phase) = flex_rate(rate)?;
    let (cycle, frame) = flex_slot(slot)?;
    let capcode: u64 = capcode.trim().parse().ok()?;
    let kind = flex_kind(kind);
    // A tone-only page has no text.
    let text = kind.filter(|k| *k != "tone").map(|_| text);
    Some(json!({
        "capcode": capcode,
        "baud": baud,
        "levels": levels,
        "phase": phase,
        "cycle": cycle,
        "frame": frame,
        "kind": kind,
        "text": text,
    }))
}

fn pocsag(line: &str) -> Option<Value> {
    let rest = line.strip_prefix("POCSAG")?;
    let (baud, rest) = rest.split_once(':')?;
    let baud: u32 = baud.parse().ok()?;
    let rest = rest.trim_start().strip_prefix("Address:")?.trim_start();
    let (address, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let address: u32 = address.parse().ok()?;
    let rest = rest.trim_start().strip_prefix("Function:")?.trim_start();
    let (function, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let function: u8 = function.parse().ok()?;
    let rest = rest.trim_start();
    let (kind, text) = if let Some(t) = rest.strip_prefix("Alpha:") {
        (Some("alpha"), Some(t.trim_start()))
    } else if let Some(t) = rest.strip_prefix("Numeric:") {
        (Some("numeric"), Some(t.trim_start()))
    } else {
        (None, None)
    };
    Some(json!({
        "address": address,
        "function": function,
        "baud": baud,
        "kind": kind,
        "text": text,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pocsag_alpha_and_numeric_lines() {
        let v =
            parse("POCSAG1200: Address: 1234567  Function: 3  Alpha:   HELLO ZETA<EOT>").unwrap();
        assert_eq!(
            v,
            json!({"address": 1234567, "function": 3, "baud": 1200, "kind": "alpha", "text": "HELLO ZETA<EOT>"})
        );
        let v = parse("POCSAG512: Address:   20017  Function: 0  Numeric:  0412 555 1234").unwrap();
        assert_eq!(v["baud"], 512);
        assert_eq!(v["address"], 20017);
        assert_eq!(v["kind"], "numeric");
        assert_eq!(v["text"], "0412 555 1234");
        let v = parse("POCSAG2400: Address: 99  Function: 1 ").unwrap();
        assert_eq!(v["kind"], Value::Null);
        assert_eq!(v["text"], Value::Null);
    }

    #[test]
    fn flex_next_lines_give_the_fields() {
        let v =
            parse("FLEX|2026-10-28 12:00:00|1600/2/K/A|05.120|001234567|ALN|HELLO ZETA").unwrap();
        assert_eq!(
            v,
            json!({"capcode": 1234567, "baud": 1600, "levels": 2, "phase": "A", "cycle": 5,
                   "frame": 120, "kind": "alpha", "text": "HELLO ZETA"})
        );
        let v = parse("FLEX|2026-10-28 12:00:00|3200/4/C/B|05.120|001234567|NUM|0412 555").unwrap();
        assert_eq!(v["kind"], "numeric");
        assert_eq!(v["text"], "0412 555");
        assert_eq!(v["baud"], 3200);
        assert_eq!(v["levels"], 4);
        assert_eq!(v["phase"], "B");
        let v = parse("FLEX|2026-10-28 12:00:00|1600/2/K/A|05.120|001234567|TON|").unwrap();
        assert_eq!(v["kind"], "tone");
        assert_eq!(v["text"], Value::Null);
        let v = parse("FLEX|2026-10-28 12:00:00|1600/2/K/A|05.120|001234567|BIN|0102").unwrap();
        assert_eq!(v["kind"], "binary");
    }

    #[test]
    fn older_flex_lines_give_the_fields() {
        let v =
            parse("FLEX: 2026-10-28 12:00:00 1600/2/K 05.120 [001234567] ALN HELLO ZETA").unwrap();
        assert_eq!(
            v,
            json!({"capcode": 1234567, "baud": 1600, "levels": 2, "phase": null, "cycle": 5,
                   "frame": 120, "kind": "alpha", "text": "HELLO ZETA"})
        );
    }

    #[test]
    fn text_with_a_pipe_survives() {
        let v = parse("FLEX|2026-10-28 12:00:00|1600/2/K/A|05.120|001234567|ALN|A|B").unwrap();
        assert_eq!(v["text"], "A|B");
    }

    #[test]
    fn a_flex_line_that_does_not_parse_stays_text() {
        for line in [
            "FLEX|garbled",
            "FLEX: garbled",
            "FLEX|2026-10-28 12:00:00|1600/2/K/A|05.120|notanumber|ALN|X",
        ] {
            assert_eq!(parse(line).unwrap(), json!({ "text": line }));
        }
    }

    #[test]
    fn other_lines_are_text() {
        assert_eq!(
            parse("POCSAG1200: garbled").unwrap(),
            json!({"text": "POCSAG1200: garbled"})
        );
        assert_eq!(parse("hello").unwrap(), json!({"text": "hello"}));
        assert!(parse("").is_none());
    }
}
