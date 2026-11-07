//! `multimon`: multimon-ng's text output. A POCSAG line,
//! `POCSAG<baud>: Address: <n>  Function: <f>  Alpha:   <text>` (or `Numeric:`,
//! or neither for a tone-only page), becomes `{address, function, baud, kind,
//! text}` with `kind` `alpha`, `numeric` or `null` (and `text` `null` with it).
//! A FLEX line becomes `{capcode, baud, levels, phase, cycle, frame, kind, text}`, in each
//! form multimon-ng 1.5.0 prints (pinned against its output in the tests):
//! - the FLEX_NEXT demodulator's (the `flex_decode` row's),
//!   `FLEX_NEXT|<baud>/<levels>|<cycle>.<frame>.<phase>|<capcode>|<address type>|<vector type>|<kind>|<text>`,
//!   an alphanumeric page with a fragment field (`3.0.K`) before the text;
//! - the FLEX demodulator's alphanumeric page,
//!   `FLEX|[<time>|]<baud>/<levels>/<K|C>/<phase>|<cycle>.<frame>|<capcode>|<kind>|<text>`
//!   (no time with `--flex-no-ts`);
//! - its other pages, `FLEX: <time> <baud>/<levels>/<phase> <cycle>.<frame> [<capcode>] <kind> <text>`,
//!   or with `--flex-no-ts` `FLEX|<baud>/<levels>/<phase>|<cycle>.<frame>|[<capcode>]|<kind> <text>`.
//!
//! `kind` is `alpha`, `numeric`, `tone`, `binary` or `null` (`text` `null` with `tone` and
//! `null`); multimon's timestamp, the host clock, is dropped. Every other non-empty line,
//! and a FLEX line that does not parse, is `{"text": line}`.

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

/// The `|`-separated fields of a FLEX_NEXT line after its tag, text last (so a `|` in
/// the text survives): rate, slot, capcode, address type, vector type, kind, text.
const FLEX_NEXT_FIELDS: usize = 7;
/// The `|`-separated fields of a FLEX line after its tag and time, text last: rate,
/// slot, capcode, kind, text.
const FLEX_FIELDS: usize = 5;

/// One FLEX page's fields, as the forms print them.
struct FlexLine<'a> {
    baud: &'a str,
    levels: &'a str,
    phase: &'a str,
    cycle: &'a str,
    frame: &'a str,
    capcode: &'a str,
    kind: &'a str,
    text: &'a str,
}

/// ALN, NUM, TON, BIN; anything else (UNK) is `None`.
fn flex_kind(s: &str) -> Option<&'static str> {
    match s {
        "ALN" => Some("alpha"),
        "NUM" => Some("numeric"),
        "TON" => Some("tone"),
        "BIN" => Some("binary"),
        _ => None,
    }
}

/// An alphanumeric page's fragment field, `<fragment>.<continued>.<letter>` (`3.0.K`).
fn is_fragment(s: &str) -> bool {
    let mut p = s.split('.');
    let digit =
        |f: Option<&str>| f.is_some_and(|f| f.len() == 1 && f.as_bytes()[0].is_ascii_digit());
    digit(p.next())
        && digit(p.next())
        && p.next()
            .is_some_and(|f| f.len() == 1 && f.as_bytes()[0].is_ascii_uppercase())
        && p.next().is_none()
}

/// `FLEX_NEXT|1600/2|01.000.A|0001234567|SS|5|ALN|3.0.K|HELLO ZETA`, after the tag.
fn flex_next(rest: &str) -> Option<FlexLine<'_>> {
    let mut f = rest.splitn(FLEX_NEXT_FIELDS, '|');
    let (baud, levels) = f.next()?.split_once('/')?;
    let mut slot = f.next()?.split('.');
    let (cycle, frame, phase) = (slot.next()?, slot.next()?, slot.next()?);
    if slot.next().is_some() {
        return None;
    }
    let capcode = f.next()?;
    let _address_type = f.next()?;
    let _vector_type = f.next()?;
    let kind = f.next()?;
    let mut text = f.next()?;
    if kind == "ALN"
        && let Some((fragment, rest)) = text.split_once('|')
        && is_fragment(fragment)
    {
        text = rest;
    }
    Some(FlexLine {
        baud,
        levels,
        phase,
        cycle,
        frame,
        capcode,
        kind,
        text,
    })
}

/// `<baud>/<levels>/<K|C>/<phase>` or `<baud>/<levels>/<phase>`.
fn flex_rate(s: &str) -> Option<(&str, &str, &str)> {
    let p: Vec<&str> = s.split('/').collect();
    match p[..] {
        [baud, levels, _, phase] | [baud, levels, phase] => Some((baud, levels, phase)),
        _ => None,
    }
}

/// `FLEX|[<time>|]1600/2/K/A|01.000|001234567|ALN|HELLO ZETA`, after the tag; also the
/// `--flex-no-ts` form of the other pages, `1600/2/A|01.000|[001234567]|NUM 0412555`.
fn flex_piped(rest: &str) -> Option<FlexLine<'_>> {
    // A time has no `/`; the rate always has.
    let rest = match rest.split_once('|') {
        Some((time, after)) if !time.contains('/') => after,
        _ => rest,
    };
    let mut f = rest.splitn(FLEX_FIELDS, '|');
    let (baud, levels, phase) = flex_rate(f.next()?)?;
    let (cycle, frame) = f.next()?.split_once('.')?;
    let capcode = f.next()?;
    let (kind, text) = match (f.next()?, f.next()) {
        (kind, Some(text)) => (kind, text),
        (kind_text, None) => kind_text.split_once(' ').unwrap_or((kind_text, "")),
    };
    Some(FlexLine {
        baud,
        levels,
        phase,
        cycle,
        frame,
        capcode,
        kind,
        text,
    })
}

/// `FLEX: 2026-11-07 21:37:01 1600/2/A 01.000 [001234567] NUM 0412555`, after the tag.
fn flex_spaced(rest: &str) -> Option<FlexLine<'_>> {
    // The time is a date and a clock, two words.
    let rest = rest.trim_start();
    let (_date, rest) = rest.split_once(char::is_whitespace)?;
    let (_clock, rest) = rest.trim_start().split_once(char::is_whitespace)?;
    let (rate, rest) = rest.trim_start().split_once(char::is_whitespace)?;
    let (baud, levels, phase) = flex_rate(rate)?;
    let (slot, rest) = rest.trim_start().split_once(char::is_whitespace)?;
    let (cycle, frame) = slot.split_once('.')?;
    let rest = rest.trim_start();
    let (capcode, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let rest = rest.trim_start();
    let (kind, text) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some(FlexLine {
        baud,
        levels,
        phase,
        cycle,
        frame,
        capcode,
        kind,
        text: text.trim_start(),
    })
}

fn flex(line: &str) -> Option<Value> {
    let l = if let Some(rest) = line.strip_prefix("FLEX_NEXT|") {
        flex_next(rest)?
    } else if let Some(rest) = line.strip_prefix("FLEX|") {
        flex_piped(rest)?
    } else {
        flex_spaced(line.strip_prefix("FLEX:")?)?
    };
    let baud: u32 = l.baud.parse().ok()?;
    let levels: u32 = l.levels.parse().ok()?;
    let cycle: u32 = l.cycle.parse().ok()?;
    let frame: u32 = l.frame.parse().ok()?;
    let capcode: u64 = l
        .capcode
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()?;
    let kind = flex_kind(l.kind);
    // A tone-only page, and one of a kind not named, has no text.
    let text = kind.filter(|k| *k != "tone").map(|_| l.text);
    Some(json!({
        "capcode": capcode,
        "baud": baud,
        "levels": levels,
        "phase": l.phase,
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

    // The FLEX lines below are multimon-ng 1.5.0's own output, captured from it decoding
    // the `flex` synthesiser's frames (and variants of them with other vector types).

    #[test]
    fn flex_next_lines_give_the_fields() {
        let v = parse("FLEX_NEXT|1600/2|01.000.A|0001234567|SS|5|ALN|3.0.K|HELLO ZETA").unwrap();
        assert_eq!(
            v,
            json!({"capcode": 1234567, "baud": 1600, "levels": 2, "phase": "A", "cycle": 1,
                   "frame": 0, "kind": "alpha", "text": "HELLO ZETA"})
        );
        // A fragment that continues: the fragment field is still not text.
        let v = parse("FLEX_NEXT|1600/2|01.000.A|0001234567|SS|5|ALN|0.0.C|HELLO ZETA").unwrap();
        assert_eq!(v["text"], "HELLO ZETA");
        let v = parse("FLEX_NEXT|1600/2|01.000.A|0001234567|SS|3|NUM|0412555[[[[[").unwrap();
        assert_eq!(v["kind"], "numeric");
        assert_eq!(v["text"], "0412555[[[[[");
        let v = parse(
            "FLEX_NEXT|1600/2|01.000.A|0001234567|SS|6|BIN|00116400 0013e64c 00116d20 000020d4",
        )
        .unwrap();
        assert_eq!(v["kind"], "binary");
        assert_eq!(v["text"], "00116400 0013e64c 00116d20 000020d4");
        let v = parse("FLEX_NEXT|3200/4|05.120.C|0001234567|SS|2|TON|").unwrap();
        assert_eq!(v["kind"], "tone");
        assert_eq!(v["text"], Value::Null);
        assert_eq!(v["baud"], 3200);
        assert_eq!(v["levels"], 4);
        assert_eq!(v["phase"], "C");
        assert_eq!(v["cycle"], 5);
        assert_eq!(v["frame"], 120);
    }

    #[test]
    fn flex_lines_give_the_fields() {
        // The FLEX demodulator's alphanumeric line, with its timestamp and without
        // (`--flex-no-ts`).
        for line in [
            "FLEX|2026-11-07 21:35:37|1600/2/K/A|01.000|001234567|ALN|HELLO ZETA",
            "FLEX|1600/2/K/A|01.000|001234567|ALN|HELLO ZETA",
        ] {
            assert_eq!(
                parse(line).unwrap(),
                json!({"capcode": 1234567, "baud": 1600, "levels": 2, "phase": "A", "cycle": 1,
                       "frame": 0, "kind": "alpha", "text": "HELLO ZETA"}),
                "{line}"
            );
        }
        // Its other kinds: a spaced line, and with `--flex-no-ts` a pipe line whose
        // capcode is bracketed and whose kind and text share a field.
        for line in [
            "FLEX: 2026-11-07 21:37:01 1600/2/A 01.000 [001234567] NUM 0412555[[[[[",
            "FLEX|1600/2/A|01.000|[001234567]|NUM 0412555[[[[[",
        ] {
            assert_eq!(
                parse(line).unwrap(),
                json!({"capcode": 1234567, "baud": 1600, "levels": 2, "phase": "A", "cycle": 1,
                       "frame": 0, "kind": "numeric", "text": "0412555[[[[["}),
                "{line}"
            );
        }
        let v =
            parse("FLEX: 2026-11-07 21:37:01 1600/2/A 01.000 [001234567] UNK 00001800 00116400")
                .unwrap();
        assert_eq!(v["kind"], Value::Null);
        assert_eq!(v["text"], Value::Null);
        assert_eq!(v["capcode"], 1234567);
    }

    #[test]
    fn text_with_a_pipe_survives() {
        for line in [
            "FLEX_NEXT|1600/2|01.000.A|0001234567|SS|5|ALN|3.0.K|A|B C",
            "FLEX|2026-11-07 21:37:01|1600/2/K/A|01.000|001234567|ALN|A|B C",
        ] {
            assert_eq!(parse(line).unwrap()["text"], "A|B C", "{line}");
        }
    }

    #[test]
    fn a_flex_line_that_does_not_parse_stays_text() {
        for line in [
            "FLEX|garbled",
            "FLEX: garbled",
            "FLEX_NEXT|garbled",
            "FLEX_NEXT: Locked",
            "FLEX|2026-11-07 21:35:37|1600/2/K/A|01.000|notanumber|ALN|X",
            "FLEX_NEXT|1600/2|01.000.A|notanumber|SS|5|ALN|3.0.K|X",
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
