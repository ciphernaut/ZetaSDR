//! `json_lines`: one JSON object per line, taken as the event's fields.

use serde_json::Value;

/// The object on `line`; `None` for anything that is not one JSON object.
pub fn parse(line: &str) -> Option<Value> {
    match serde_json::from_str(line.trim()) {
        Ok(v @ Value::Object(_)) => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_line_is_the_event() {
        let v = parse(r#"{"model":"Acurite-Tower","id":1234,"temperature_C":21.5}"#).unwrap();
        assert_eq!(v["model"], "Acurite-Tower");
        assert_eq!(v["id"], 1234);
        assert!(parse("[1,2]").is_none());
        assert!(parse("42").is_none());
        assert!(parse("garbage {").is_none());
    }
}
