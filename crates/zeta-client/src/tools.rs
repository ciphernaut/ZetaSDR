//! MCP-style tool definitions generated from the operation schema and AGENTS.md.

use crate::guide::op_sections;
use serde::Serialize;
use serde_json::{Map, Value};
use zeta_core::ops::OpError;

const MAX_DEPTH: usize = 16;

/// One tool per operation; `input_schema` is strict and `$ref`-free.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Build one tool per `schema["op"]["oneOf"]` variant, in order.
pub fn op_tools(schema: &Value, md: &str) -> Vec<ToolDef> {
    let defs = &schema["op"]["$defs"];
    let sections = op_sections(md);
    let variants = schema["op"]["oneOf"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    variants
        .iter()
        .filter_map(|variant| {
            let name = variant["properties"]["op"]["const"].as_str()?.to_string();
            let mut v = variant.clone();
            let desc = v.as_object_mut().and_then(|o| o.remove("description"));
            if let Some(p) = v.get_mut("properties").and_then(Value::as_object_mut) {
                p.remove("op");
            }
            if let Some(r) = v.get_mut("required").and_then(Value::as_array_mut) {
                r.retain(|x| x != "op");
            }
            let mut hit_limit = false;
            let mut input_schema = inline(v, defs, 0, &mut hit_limit);
            if hit_limit && let Some(o) = input_schema.as_object_mut() {
                o.insert("$defs".into(), defs.clone());
            }
            let description = sections.get(&name).cloned().unwrap_or_else(|| {
                desc.as_ref()
                    .and_then(Value::as_str)
                    .map(|s| s.replace('\n', " "))
                    .unwrap_or_else(|| name.clone())
            });
            Some(ToolDef {
                name,
                description,
                input_schema,
            })
        })
        .collect()
}

fn inline(v: Value, defs: &Value, depth: usize, hit_limit: &mut bool) -> Value {
    match v {
        Value::Array(a) => Value::Array(
            a.into_iter()
                .map(|x| inline(x, defs, depth, hit_limit))
                .collect(),
        ),
        Value::Object(o) => {
            let target = o
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .and_then(|n| defs.get(n));
            match target {
                Some(def) if depth < MAX_DEPTH => {
                    let mut merged = def.as_object().cloned().unwrap_or_default();
                    for (k, x) in o {
                        if k != "$ref" {
                            merged.insert(k, x);
                        }
                    }
                    inline(Value::Object(merged), defs, depth + 1, hit_limit)
                }
                Some(_) => {
                    *hit_limit = true;
                    Value::Object(o)
                }
                None => Value::Object(
                    o.into_iter()
                        .map(|(k, x)| (k, inline(x, defs, depth, hit_limit)))
                        .collect::<Map<_, _>>(),
                ),
            }
        }
        other => other,
    }
}

/// Turn a tool call into an operation JSON by inserting `"op": name`.
pub fn op_from_call(name: &str, args: Value, tools: &[ToolDef]) -> Result<Value, OpError> {
    if !tools.iter().any(|t| t.name == name) {
        return Err(OpError::Unknown {
            field: "tool".into(),
            value: name.into(),
            known: tools.iter().map(|t| t.name.clone()).collect(),
        });
    }
    let Value::Object(mut o) = args else {
        return Err(OpError::Other {
            message: "arguments must be a JSON object".into(),
        });
    };
    o.insert("op".into(), Value::String(name.into()));
    Ok(Value::Object(o))
}
