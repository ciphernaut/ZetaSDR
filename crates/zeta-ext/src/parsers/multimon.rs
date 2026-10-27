//! `multimon`: multimon-ng's text output. A POCSAG line,
//! `POCSAG<baud>: Address: <n>  Function: <f>  Alpha:   <text>` (or `Numeric:`,
//! or neither for a tone-only page), becomes `{address, function, baud, kind,
//! text}` with `kind` `alpha`, `numeric` or `null` (and `text` `null` with it);
//! every other non-empty line (FLEX and the rest, for now) is `{"text": line}`.

use serde_json::{Value, json};

/// The fields of one multimon-ng line; `None` for an empty one.
pub fn parse(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    Some(pocsag(line).unwrap_or_else(|| json!({ "text": line })))
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
    fn other_lines_are_text() {
        let line = "FLEX|2026-10-28 12:00:00|1600/2/K/A|05.120|001234567|ALN|HELLO";
        assert_eq!(parse(line).unwrap(), json!({ "text": line }));
        assert_eq!(
            parse("POCSAG1200: garbled").unwrap(),
            json!({"text": "POCSAG1200: garbled"})
        );
        assert!(parse("").is_none());
    }
}
