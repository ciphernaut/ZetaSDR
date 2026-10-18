//! End-to-end tests of the `zeta` binary.

use std::path::Path;

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;
use zeta_source_file::fixtures::write_tone;

/// `zeta --state-dir <dir>/state --source-file tone=<fixture> --json <args…>`.
fn zeta(dir: &Path, args: &[&str]) -> Command {
    let meta = write_tone(dir);
    let mut cmd = Command::cargo_bin("zeta").unwrap();
    cmd.arg("--state-dir")
        .arg(dir.join("state"))
        .arg("--source-file")
        .arg(format!("tone={}", meta.display()))
        .arg("--json")
        .args(args);
    cmd
}

fn json(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("stdout is JSON")
}

#[test]
fn schema_prints_json_with_op_result_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(dir.path(), &["schema"]).assert().success().get_output().stdout.clone();
    let v = json(&out);
    assert_eq!(v["result"], "schema");
    for key in ["op", "result", "error"] {
        assert!(v["value"].get(key).is_some(), "schema missing {key}");
    }
}

#[test]
fn source_list_shows_file_source() {
    let dir = tempfile::tempdir().unwrap();
    let out = zeta(dir.path(), &["source", "list"]).assert().success().get_output().stdout.clone();
    let v = json(&out);
    let sources = v["value"].as_array().expect("sources array");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["id"], "tone");
}

#[test]
fn ensure_dropped_zeros_exit_2_with_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let args = ["source", "ensure", "--source", "tone", "--center-hz", "100000", "--bandwidth-hz", "48000"];
    let out = zeta(dir.path(), &args).assert().code(2).get_output().stdout.clone();
    let v = json(&out);
    assert_eq!(v["error"], "out_of_range");
    assert_eq!(v["field"], "center_hz");
    assert_eq!(v["suggested"], 100_000_000);
}

#[test]
fn job_start_persists_and_list_sees_it_in_new_process() {
    let dir = tempfile::tempdir().unwrap();
    let start = [
        "job", "start", "--kind", "passthrough", "--source", "tone", "--center-hz", "100000000", "--bandwidth-hz",
        "48000",
    ];
    let out = zeta(dir.path(), &start).assert().success().get_output().stdout.clone();
    let v = json(&out);
    assert_eq!(v["result"], "job");
    let id = v["value"].as_str().expect("job id").to_owned();

    let out = zeta(dir.path(), &["job", "list"]).assert().success().stdout(contains(id.as_str())).get_output().stdout.clone();
    let v = json(&out);
    let jobs = v["value"].as_array().expect("jobs array");
    let job = jobs.iter().find(|j| j["id"] == id.as_str()).expect("started job listed");
    assert_eq!(job["state"]["state"], "running");
}
