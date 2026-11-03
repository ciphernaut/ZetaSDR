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
    assert_eq!(variants.len(), 34);
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

#[test]
fn plan_start_cli_line_is_the_real_command() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    assert!(
        md.contains("CLI: `zeta plan start --plan uhf_cb_au --kind fm_demod --source <id>"),
        "plan_start CLI line missing"
    );
    assert!(md.contains("never `job_stop`"), "rule missing");
    assert!(md.contains("use `plan_start`"), "rule missing");
}

#[test]
fn jobs_stop_cli_line_and_rule_are_in_the_guide() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    assert!(md.contains("CLI: `zeta jobs stop [--source ID] [--kind KIND] [--plan ID]`"));
    assert!(md.contains("use `jobs_stop`, never one `job_stop` per job"));
}

#[test]
fn mix_start_is_in_the_guide() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    for needle in [
        "CLI: `zeta mix start`",
        "\"op\":\"mix_start\",\"sources\":null",
        "### Listening to many channels",
        "attach it there only when asked",
        "### mix\n",
    ] {
        assert!(md.contains(needle), "missing {needle}");
    }
}

#[test]
fn the_guide_has_the_plan_rule_and_the_own_jobs_rule() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    assert!(md.contains("Plan ids come from `channel_plans`: read it before naming a plan. A band with no plan takes one `job_start` per measured station; never invent a plan."));
    assert!(md.contains("started it in this run"));
    assert!(md.contains("never `job_stop`"));
}

#[test]
fn knowledge_acma_is_in_the_running_section() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    for needle in [
        "--knowledge-acma URL[,reach_km=N]",
        "ZETA_KNOWLEDGE_ACMA",
        "`acma`",
        "`station set`",
    ] {
        assert!(md.contains(needle), "missing {needle}");
    }
    assert!(!md.contains("Neither has allocations"));
}

#[test]
fn station_set_is_in_the_guide() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    for needle in ["CLI: `zeta station set --lat", "\"op\":\"station_set\""] {
        assert!(md.contains(needle), "missing {needle}");
    }
}

#[test]
fn capture_ops_are_in_the_guide() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    for needle in [
        "CLI: `zeta capture list",
        "CLI: `zeta capture show --capture",
        "CLI: `zeta capture classify --capture",
        "CLI: `zeta capture delete --capture",
        "\"op\":\"capture_delete\"",
        "allow.delete_captures",
        "GET /captures/<id>.sigmf-meta",
        "newest first",
    ] {
        assert!(md.contains(needle), "missing {needle}");
    }
}

#[test]
fn bursts_keep_flags_are_in_the_running_section() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    for needle in [
        "--bursts-keep-mb N",
        "ZETA_BURSTS_KEEP_MB",
        "--bursts-keep-days N",
        "ZETA_BURSTS_KEEP_DAYS",
    ] {
        assert!(md.contains(needle), "missing {needle}");
    }
}

#[test]
fn exploring_an_unknown_signal_is_in_the_guide() {
    let md = zeta()
        .arg("agents-md")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let md = String::from_utf8(md).unwrap();
    for needle in [
        "### Exploring an unknown signal",
        "on_unknown",
        "capture_classify",
        "register says",
        "legal line",
        "measured over the bursts",
    ] {
        assert!(md.contains(needle), "missing {needle}");
    }
}
