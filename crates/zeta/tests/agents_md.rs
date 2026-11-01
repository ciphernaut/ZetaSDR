//! `zeta agents-md`: the rendering lists every operation and the committed file matches it.

use assert_cmd::Command;
use serde_json::Value;

/// The `zeta` binary without `ZETA_NODE`/`ZETA_SECRET`, so `schema` is never sent remote.
fn zeta() -> Command {
    let mut cmd = Command::cargo_bin("zeta").unwrap();
    cmd.env_remove("ZETA_NODE").env_remove("ZETA_SECRET");
    cmd
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
    assert_eq!(variants.len(), 24);
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

#[test]
fn guide_covers_running_the_node_stages_and_descriptions() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    let running = md.find("## Running the node\n").expect("running section");
    let ops = md.find("## Operations\n").expect("operations section");
    assert!(running < ops);
    for needle in [
        "--state-dir",
        "--source-file",
        "zeta serve",
        "serve.lock",
        "## Stage kinds\n",
        "### passthrough\n",
        "writes the whole ring bandwidth over the span as a SigMF pair",
        "restarts a running job under the same id with the new graph",
        "`source list`",
        "`job list`",
        "`AU`",
        "no provider data",
        // A param with a default is listed too, as optional.
        "- params.squelch: boolean (optional, default true)\n",
        "- params.squelch_db: integer or null (optional, default null)\n",
    ] {
        assert!(md.contains(needle), "guide lacks {needle:?}");
    }
}

#[test]
fn channel_plans_cli_line_is_the_real_command() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    let section = &md[md.find("## channel_plans\n").expect("section")..];
    let line = section
        .lines()
        .find(|l| l.starts_with("CLI: "))
        .expect("CLI line");
    assert!(line.starts_with("CLI: `zeta plans"), "{line}");
}
