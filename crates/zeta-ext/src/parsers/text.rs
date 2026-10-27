//! `text`: every non-empty line is one event, `{"text": line}`.

use serde_json::{Value, json};

/// `{"text": line}`; `None` for an empty line.
pub fn parse(line: &str) -> Option<Value> {
    (!line.trim().is_empty()).then(|| json!({ "text": line }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_its_text() {
        assert_eq!(
            parse("EAS: ZCZC-WXR-TOR-029095+0030-1051700-KEAX/NWS-").unwrap(),
            json!({"text": "EAS: ZCZC-WXR-TOR-029095+0030-1051700-KEAX/NWS-"})
        );
        assert!(parse("  ").is_none());
    }
}
