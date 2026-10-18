//! `zeta agents-md`: render AGENTS.md from the operation schema plus the hand-written field craft.

use std::process::ExitCode;

use serde_json::Value;

const FIELD_CRAFT: &str = include_str!("../../../docs/agents/field-craft.md");

const INTRO: &str = "ZetaSDR is an agentic SDR runtime. You drive it through a small typed operation set. \
The CLI form and the JSON form of every operation are equivalent: `zeta <group> <verb> --flag value` \
and `{\"op\": \"<name>\", ...}` do the same thing.";

const RULES: &[&str] = &[
    "One unit per field: frequencies and bandwidths are integers in hertz in fields ending `_hz`, rates are in baud in fields ending `_baud`.",
    "Every field is required and unknown fields are rejected.",
    "Call operations, do not explain.",
    "Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it.",
    "Unknown signals are reported as unknown, never guessed.",
];

/// Hand-written sentence appended to an operation's section.
fn note(op: &str) -> Option<&'static str> {
    match op {
        "graph_put" => Some(
            "Fetch the current graph with `graph get`, edit it, and put it back; stage kinds and their params are listed by `schema`.",
        ),
        _ => None,
    }
}

/// Unit label of a named integer type, stated explicitly.
fn unit(name: &str) -> Option<&'static str> {
    match name {
        "Hz" => Some("Hz"),
        "Baud" => Some("baud"),
        "Timestamp" => Some("ns since epoch"),
        _ => None,
    }
}

/// Render the guide from `schema()` and the field-craft text.
pub fn render(schema: &Value, field_craft: &str) -> String {
    let op = &schema["op"];
    let defs = &op["$defs"];
    let mut out = format!("# ZetaSDR agent guide\n\n{INTRO}\n\n## Rules\n\n");
    for r in RULES {
        out.push_str(&format!("- {r}\n"));
    }
    out.push_str("\n## Operations\n");
    for variant in op["oneOf"].as_array().into_iter().flatten() {
        let name = variant["properties"]["op"]["const"].as_str().unwrap_or("?");
        let fields: Vec<(&str, &Value)> = variant["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|f| *f != "op")
            .map(|f| (f, &variant["properties"][f]))
            .collect();
        out.push_str(&format!("\n### {name}\n\n"));
        for (f, s) in &fields {
            out.push_str(&format!("- {f}: {}\n", type_of(s, defs)));
        }
        if !fields.is_empty() {
            out.push('\n');
        }
        let mut json = format!("{{\"op\":\"{name}\"");
        for (f, s) in &fields {
            json.push_str(&format!(",\"{f}\":{}", example_json(f, s, defs)));
        }
        json.push('}');
        out.push_str(&format!(
            "JSON: `{json}`\n\nCLI: `{}`\n",
            cli_form(name, &fields, defs)
        ));
        if let Some(n) = note(name) {
            out.push_str(&format!("\n{n}\n"));
        }
    }
    out.push_str("\n## Field craft\n\n");
    out.push_str(field_craft);
    out
}

/// The `$defs` entry a `$ref` points at, with its name.
fn resolve<'a>(s: &'a Value, defs: &'a Value) -> (Option<&'a str>, &'a Value) {
    match s["$ref"].as_str().and_then(|r| r.strip_prefix("#/$defs/")) {
        Some(n) => (Some(n), &defs[n]),
        None => (None, s),
    }
}

fn required<'a>(def: &'a Value) -> Vec<&'a str> {
    def["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn type_of(s: &Value, defs: &Value) -> String {
    let (name, def) = resolve(s, defs);
    let ty = def["type"].as_str().unwrap_or("any");
    match (ty, name) {
        ("integer", Some(n)) => format!("integer ({})", unit(n).unwrap_or(n)),
        ("string", Some(n)) if def["enum"].is_array() => {
            let vals: Vec<&str> = def["enum"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            format!("string ({n}: {})", vals.join("|"))
        }
        ("object", Some(n)) => {
            let parts: Vec<String> = required(def)
                .into_iter()
                .map(|p| {
                    let (_, pd) = resolve(&def["properties"][p], defs);
                    let (item_name, item) = resolve(&pd["items"], defs);
                    if pd["type"] == "object" {
                        let subs = required(pd);
                        let units: Vec<Option<&str>> = subs
                            .iter()
                            .map(|q| resolve(&pd["properties"][*q], defs).0.and_then(unit))
                            .collect();
                        let tail = match units.first() {
                            Some(Some(u)) if units.iter().all(|x| *x == Some(*u)) => {
                                format!(": integer ({u})")
                            }
                            _ => String::new(),
                        };
                        format!("{p}{{{}{tail}}}", subs.join(","))
                    } else if let (Some(n), "object") =
                        (item_name, item["type"].as_str().unwrap_or(""))
                    {
                        format!("{p}[{n}: {}]", required(item).join(", "))
                    } else {
                        p.to_string()
                    }
                })
                .collect();
            format!("object ({n}: {})", parts.join(", "))
        }
        _ => ty.to_string(),
    }
}

fn example(field: &str, s: &Value, defs: &Value) -> Value {
    let (name, def) = resolve(s, defs);
    match def["type"].as_str() {
        _ if name == Some("NodeId") => "<node-id>".into(),
        Some("integer") => match (field, name) {
            ("bandwidth_hz", _) => 12500.into(),
            (_, Some("Hz")) => 144800000u64.into(),
            ("start", _) => 1760000000000000000u64.into(),
            ("end", _) => 1760000001000000000u64.into(),
            _ => 0.into(),
        },
        Some("string") => match def["enum"].as_array().and_then(|a| a.first()) {
            Some(first) => first.clone(),
            None if field == "region" => "AU".into(),
            None if field == "kind" => "passthrough".into(),
            None => "<id>".into(),
        },
        Some("object") => {
            let mut m = serde_json::Map::new();
            for p in required(def) {
                m.insert(p.into(), example(p, &def["properties"][p], defs));
            }
            Value::Object(m)
        }
        Some("array") if field == "nodes" => Value::Array(vec![example("node", &s["items"], defs)]),
        Some("array") => Value::Array(vec![]),
        _ if field == "params" => serde_json::json!({"kind": "iq"}),
        _ => Value::Null,
    }
}

/// Example value as JSON text, object keys in schema (required) order.
fn example_json(field: &str, s: &Value, defs: &Value) -> String {
    let (_, def) = resolve(s, defs);
    if def["type"] != "object" {
        return example(field, s, defs).to_string();
    }
    let parts: Vec<String> = required(def)
        .into_iter()
        .map(|p| format!("\"{p}\":{}", example_json(p, &def["properties"][p], defs)))
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// Flag name and shell-ready value for each leaf the CLI takes.
fn cli_flags(field: &str, s: &Value, defs: &Value, out: &mut Vec<(String, String)>) {
    let (name, def) = resolve(s, defs);
    match (field, name) {
        ("selection", _) | ("span", _) => {
            for p in required(def) {
                let flag = match (field, p) {
                    ("span", "start") => "start_ns",
                    ("span", "end") => "end_ns",
                    _ => p,
                };
                if p == "span" {
                    cli_flags(p, &def["properties"][p], defs, out);
                } else {
                    out.push((
                        flag.replace('_', "-"),
                        value(p, &def["properties"][p], defs),
                    ));
                }
            }
        }
        ("graph", _) => out.push(("file".into(), "graph.json".into())),
        _ => out.push((field.replace('_', "-"), value(field, s, defs))),
    }
}

fn value(field: &str, s: &Value, defs: &Value) -> String {
    match example(field, s, defs) {
        Value::String(t) => t,
        v => v.to_string(),
    }
}

fn cli_form(op: &str, fields: &[(&str, &Value)], defs: &Value) -> String {
    let mut line = String::from("zeta ");
    line.push_str(&match op.split_once('_') {
        Some((g, v)) => format!("{g} {v}"),
        None => op.to_string(),
    });
    let mut flags = vec![];
    for (f, s) in fields {
        cli_flags(f, s, defs, &mut flags);
    }
    for (flag, v) in flags {
        line.push_str(&format!(" --{flag} {v}"));
    }
    line
}

/// Print the rendering, or with `check` compare it to the committed `AGENTS.md`.
pub fn run(check: bool) -> ExitCode {
    let text = render(&zeta_core::ops::schema(), FIELD_CRAFT);
    if !check {
        print!("{text}");
        return ExitCode::SUCCESS;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../AGENTS.md");
    match std::fs::read_to_string(path) {
        Ok(on_disk) if on_disk == text => ExitCode::SUCCESS,
        Ok(on_disk) => {
            let line = text.lines().zip(on_disk.lines()).position(|(a, b)| a != b);
            let at = line.unwrap_or_else(|| text.lines().count().min(on_disk.lines().count())) + 1;
            eprintln!(
                "zeta: AGENTS.md differs from the rendering (first difference at line {at}); regenerate with `zeta agents-md > AGENTS.md`"
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("zeta: reading {path}: {e}");
            ExitCode::FAILURE
        }
    }
}
