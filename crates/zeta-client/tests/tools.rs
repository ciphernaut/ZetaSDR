use serde_json::{Value, json};
use std::collections::BTreeSet;
use zeta_client::{op_from_call, op_sections, op_tools, system_prompt};
use zeta_core::ops::{OpError, schema};

const MD: &str = include_str!("../../../AGENTS.md");

fn names(s: &Value) -> Vec<String> {
    s["op"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["properties"]["op"]["const"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_op_has_a_section_in_committed_agents_md() {
    let s = schema();
    let sections = op_sections(MD);
    for n in names(&s) {
        assert!(sections.contains_key(&n), "missing section {n}");
    }
}

#[test]
fn tool_count_matches_op_variants() {
    let s = schema();
    assert_eq!(
        op_tools(&s, MD).len(),
        s["op"]["oneOf"].as_array().unwrap().len()
    );
}

#[test]
fn tools_are_strict_and_ref_free() {
    for t in op_tools(&schema(), MD) {
        let sc = &t.input_schema;
        assert_eq!(sc["additionalProperties"], json!(false), "{}", t.name);
        let props: BTreeSet<&str> = sc["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let req: BTreeSet<&str> = sc["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(props, req, "{}", t.name);
        assert!(!props.contains("op"));
        assert!(!serde_json::to_string(sc).unwrap().contains("$ref"));
    }
}

#[test]
fn source_ensure_tool_shape() {
    let tools = op_tools(&schema(), MD);
    let t = tools.iter().find(|t| t.name == "source_ensure").unwrap();
    let props: BTreeSet<&str> = t.input_schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        props,
        BTreeSet::from(["source", "center_hz", "bandwidth_hz", "replace"])
    );
    assert_eq!(t.input_schema["properties"]["center_hz"]["type"], "integer");
    assert!(t.description.contains("Check a band"));
    assert!(t.description.contains("JSON: `{\"op\":\"source_ensure\""));
    assert!(!t.description.contains("CLI: "));
}

#[test]
fn job_start_kind_is_inlined_enum() {
    let tools = op_tools(&schema(), MD);
    let t = tools.iter().find(|t| t.name == "job_start").unwrap();
    let e = t.input_schema["properties"]["kind"]["enum"]
        .as_array()
        .unwrap();
    assert!(e.contains(&json!("passthrough")));
}

#[test]
fn system_prompt_keeps_rules_and_field_craft_only() {
    let p = system_prompt(MD);
    assert!(p.contains("# ZetaSDR agent guide"));
    assert!(p.contains("## Rules"));
    assert!(p.contains("Call operations, do not explain."));
    assert!(p.contains("## Field craft"));
    assert!(!p.contains("## Operations"));
    assert!(!p.contains("## Running the node"));
    assert!(!p.contains("### source_ensure"));
    assert!(p.ends_with("\n") && !p.ends_with("\n\n"));
}

#[test]
fn op_from_call_cases() {
    let tools = op_tools(&schema(), MD);
    let v = op_from_call(
        "source_ensure",
        json!({"source":"tone","center_hz":1,"bandwidth_hz":1}),
        &tools,
    )
    .unwrap();
    assert_eq!(v["op"], "source_ensure");
    assert_eq!(v["source"], "tone");
    match op_from_call("tune", json!({}), &tools) {
        Err(OpError::Unknown {
            field,
            value,
            known,
        }) => {
            assert_eq!(field, "tool");
            assert_eq!(value, "tune");
            assert!(known.contains(&"source_ensure".to_string()));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        op_from_call("job_list", json!("x"), &tools),
        Err(OpError::Other { .. })
    ));
}
