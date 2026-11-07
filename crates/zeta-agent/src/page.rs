//! Page-only tools: offered only to a run the web page asked for, answered by the
//! agent itself and applied by the page when the answer arrives. They never reach
//! the executor, so no job, lease or allowance has a say in them.

use serde_json::{Value, json};
use zeta_client::ToolDef;

/// The page-only tool that changes one of the page's view settings.
pub const VIEW_SET: &str = "view_set";

/// The view settings `view_set` may change: presentation only, each one changing what this
/// tab shows and nothing else (no operation that acts, no job, no operator data). The page's
/// keys that act (`zoom_out`, `scanner` and `auto_scan_stop` drive its scans; `home` moves
/// where distances count from) are typed by the operator, never set by the agent.
pub const VIEW_KEYS: &[&str] = &[
    "theme",
    "side",
    "dock",
    "main",
    "map",
    "pane",
    "subjects",
    "finished_rows_s",
    "subject_dim_s",
    "subject_drop_s",
    "plan",
    "follow",
    "origin_fade_s",
];

/// What a run with page tools knows about the page it serves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageTools {
    /// The node's ready theme ids, the only values `view_set theme` takes.
    pub themes: Vec<String>,
}

/// The `view_set` tool as the model sees it.
pub fn view_set_tool() -> ToolDef {
    ToolDef {
        name: VIEW_SET.to_string(),
        description: format!(
            "Change one of the web page's view settings for the tab that asked (the same as typing `view set <key> <value>`), e.g. key `theme` with a ready id from `themes_list`. Keys: {} (presentation only; the page's scan and home settings are the operator's to type). It changes only that page; it never touches the radio or any job, so no job or lease can block it. Available only when the request came from the page.",
            VIEW_KEYS.join(", ")
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "key": {"type": "string", "enum": VIEW_KEYS, "description": "The view setting, one of the presentation keys listed."},
                "value": {"type": "string", "description": "Its new value; for `theme` a ready id from `themes_list`."},
            },
            "required": ["key", "value"],
            "additionalProperties": false,
        }),
    }
}

/// Answer a `view_set` call: `Ok` with what the page will apply, `Err` naming the
/// offending field. The key must be one of [`VIEW_KEYS`]; a `theme` value must be a ready
/// theme id; the page checks the other keys' values.
pub fn view_set(page: &PageTools, args: &Value) -> Result<Value, Value> {
    let Some(o) = args.as_object() else {
        return Err(json!({"error": "other", "message": "arguments must be a JSON object"}));
    };
    if let Some(extra) = o.keys().find(|k| *k != "key" && *k != "value") {
        return Err(json!({
            "error": "other",
            "field": extra,
            "message": format!("view_set: unknown field `{extra}`, expected `key` and `value`"),
        }));
    }
    let field = |name: &str| match o.get(name) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(json!({
            "error": "other",
            "field": name,
            "message": format!("view_set: `{name}` must be a string"),
        })),
        None => Err(json!({
            "error": "other",
            "field": name,
            "message": format!("view_set: missing field `{name}`"),
        })),
    };
    let key = field("key")?;
    let value = field("value")?;
    if !VIEW_KEYS.contains(&key.as_str()) {
        return Err(json!({
            "error": "unknown",
            "field": "key",
            "value": key,
            "known": VIEW_KEYS,
        }));
    }
    if key == "theme" && !page.themes.contains(&value) {
        return Err(json!({
            "error": "unknown",
            "field": "value",
            "value": value,
            "known": page.themes,
        }));
    }
    Ok(json!({
        "page": true,
        "key": key,
        "value": value,
        "applied": "by the page when this answer arrives",
    }))
}
