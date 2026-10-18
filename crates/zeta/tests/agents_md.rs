//! `zeta agents-md`: the rendering lists every operation and the committed file matches it.

use assert_cmd::Command;
use serde_json::Value;

fn zeta() -> Command {
    Command::cargo_bin("zeta").unwrap()
}

#[test]
fn rendering_lists_every_op_variant() {
    let out = zeta()
        .args(["--json", "schema"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: Value = serde_json::from_slice(&out).unwrap();
    let variants = v["value"]["op"]["oneOf"].as_array().unwrap();
    assert_eq!(variants.len(), 13);
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    for variant in variants {
        let name = variant["properties"]["op"]["const"].as_str().unwrap();
        assert!(
            md.contains(&format!("## {name}\n")),
            "missing section for {name}"
        );
    }
}

#[test]
fn committed_agents_md_matches_rendering() {
    zeta().args(["agents-md", "--check"]).assert().success();
}
