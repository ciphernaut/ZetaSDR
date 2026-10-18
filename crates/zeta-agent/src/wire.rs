//! OpenAI-compatible chat message and response shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One chat message; unknown fields from the server are ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn system(s: impl Into<String>) -> Self {
        Self::new("system", s)
    }

    pub fn user(s: impl Into<String>) -> Self {
        Self::new("user", s)
    }

    pub fn tool(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(id.into()),
            ..Self::new("tool", content)
        }
    }
}

fn function_kind() -> String {
    "function".into()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: Value,
}

impl FunctionCall {
    /// The arguments as a JSON object: a string is parsed (llama-server sends
    /// one), an object is taken as is.
    pub fn args(&self) -> Result<Value, String> {
        let v = match &self.arguments {
            Value::String(s) => serde_json::from_str(s)
                .map_err(|e| format!("arguments are not a JSON object: {e}"))?,
            other => other.clone(),
        };
        if v.is_object() {
            Ok(v)
        } else {
            Err(format!("arguments are not a JSON object: {v}"))
        }
    }
}

/// The response body; only `choices` is read.
#[derive(Debug, Deserialize)]
pub(crate) struct ChatResponse {
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Choice {
    pub message: ResponseMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponseMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arguments_as_string_or_object() {
        let call = |arguments| FunctionCall {
            name: "f".into(),
            arguments,
        };
        assert_eq!(call(json!("{\"a\":1}")).args(), Ok(json!({"a":1})));
        assert_eq!(call(json!({"a":1})).args(), Ok(json!({"a":1})));
        assert!(call(json!("not json")).args().is_err());
    }
}
