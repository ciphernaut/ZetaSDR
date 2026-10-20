//! `zeta serve --listen` and `--node`, against a real serving process.

mod common;

use assert_cmd::Command;
use common::{ServeProc, zeta_remote};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;

const SECRET: &str = "a-long-enough-secret";

fn json(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("stdout is JSON")
}

fn plain() -> Command {
    let mut c = Command::cargo_bin("zeta").unwrap();
    c.env_remove("ZETA_NODE").env_remove("ZETA_SECRET");
    c
}

#[test]
fn remote_job_start_then_list_sees_running_job() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    assert_eq!(p.dir, dir.path());
    let out = zeta_remote(
        &p.url,
        SECRET,
        &[
            "job",
            "start",
            "--kind",
            "passthrough",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
        ],
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let started = json(&out);
    let id = started["value"]
        .as_str()
        .unwrap_or_else(|| panic!("no job id in {started}"))
        .to_string();

    let out = zeta_remote(&p.url, SECRET, &["job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let jobs = json(&out);
    let found = jobs["value"].as_array().is_some_and(|a| {
        a.iter()
            .any(|j| j["id"] == id.as_str() && j["state"]["status"] == "running")
    });
    assert!(found, "running job {id} not in {jobs}");

    let out = zeta_remote(&p.url, SECRET, &["stream", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let streams = json(&out);
    let owned = streams["value"]
        .as_array()
        .is_some_and(|a| a.iter().any(|s| s["provenance"]["job"] == id.as_str()));
    assert!(owned, "no stream from job {id} in {streams}");
}

#[test]
fn remote_op_error_exits_2_with_json() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    let out = zeta_remote(
        &p.url,
        SECRET,
        &[
            "source",
            "ensure",
            "--source",
            "tone",
            "--center-hz",
            "100000",
            "--bandwidth-hz",
            "48000",
        ],
    )
    .assert()
    .code(2)
    .get_output()
    .stdout
    .clone();
    let e = json(&out);
    assert_eq!(e["suggested"], 100_000_000u64, "{e}");
}

#[test]
fn remote_wrong_secret_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    zeta_remote(
        &p.url,
        "wrong",
        &[
            "job",
            "start",
            "--kind",
            "passthrough",
            "--source",
            "tone",
            "--center-hz",
            "100000000",
            "--bandwidth-hz",
            "48000",
        ],
    )
    .assert()
    .code(1)
    .stderr(contains("rejected the secret"));
    let out = zeta_remote(&p.url, SECRET, &["job", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["value"].as_array().map(Vec::len), Some(0));
}

#[test]
fn remote_unreachable_exits_1_naming_url() {
    zeta_remote("http://127.0.0.1:9", "s", &["job", "list"])
        .assert()
        .code(1)
        .stderr(contains("unreachable"))
        .stderr(contains("http://127.0.0.1:9"));
}

#[test]
fn node_flag_rejects_local_flags() {
    plain()
        .args(["--node", "http://127.0.0.1:9", "--secret", "s"])
        .args(["--source-file", "x=y", "source", "list"])
        .assert()
        .code(1);
    plain()
        .args(["--node", "http://127.0.0.1:9", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("requires --secret"));
}

#[test]
fn listen_requires_secret_and_lan() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    plain()
        .arg("--state-dir")
        .arg(&state)
        .args(["serve", "--listen", "127.0.0.1:0"])
        .assert()
        .code(1)
        .stderr(contains("--listen requires --secret"));
    plain()
        .arg("--state-dir")
        .arg(&state)
        .args(["serve", "--listen", "8.8.8.8:7979", "--secret", "s"])
        .assert()
        .code(1)
        .stderr(contains("LAN only"));
    assert!(!state.join("serve.lock").exists());
}

fn stdout_of(args: &[&str]) -> String {
    let out = plain()
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn committed_skill_matches_rendering() {
    plain()
        .args(["agents-md", "--skill", "--check"])
        .assert()
        .success();
}

#[test]
fn skill_has_frontmatter_and_full_guide() {
    let skill = stdout_of(&["agents-md", "--skill"]);
    let guide = stdout_of(&["agents-md"]);
    assert!(skill.starts_with("---\nname: zetasdr\n"));
    assert!(skill.ends_with(&guide));
}

#[test]
fn guide_documents_transports() {
    let guide = stdout_of(&["agents-md"]);
    for needle in [
        "--node URL",
        "ZETA_SECRET",
        "zeta serve --listen",
        "zeta-mcp --node URL",
        "zeta ask",
    ] {
        assert!(guide.contains(needle), "guide lacks {needle}");
    }
}

#[tokio::test]
async fn serve_serves_committed_agents_md() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    let client = zeta_client::NodeClient::new(&p.url, SECRET).unwrap();
    let served = client.agents_md().await.unwrap();
    let committed =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../AGENTS.md")).unwrap();
    assert_eq!(served, committed);
}

#[test]
fn stt_socket_and_stt_node_are_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    plain()
        .env_remove("ZETA_STT_SOCKET")
        .env("ZETA_STT_NODE_SECRET", SECRET)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["serve", "--listen", "127.0.0.1:0", "--secret", SECRET])
        .args(["--stt-socket", "/nonexistent/voiced.sock"])
        .args(["--stt-node", "http://127.0.0.1:9"])
        .assert()
        .code(1)
        .stderr(contains("--stt-socket and --stt-node are exclusive"));
}

#[test]
fn stt_socket_and_stt_node_are_exclusive_for_every_command() {
    let dir = tempfile::tempdir().unwrap();
    plain()
        .env_remove("ZETA_STT_SOCKET")
        .env_remove("ZETA_STT_NODE")
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["--stt-socket", "/nonexistent/voiced.sock"])
        .args(["--stt-node", "http://127.0.0.1:9", "source", "list"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains("--stt-socket and --stt-node are exclusive"));
    plain()
        .env_remove("ZETA_STT_SOCKET")
        .env_remove("ZETA_STT_NODE")
        .args(["--node", "http://127.0.0.1:9", "--secret", SECRET])
        .args(["--stt-socket", "/nonexistent/voiced.sock"])
        .args(["--stt-node", "http://127.0.0.1:9", "source", "list"])
        .assert()
        .code(1)
        .stderr(contains("--stt-socket and --stt-node are exclusive"));
}

#[test]
fn node_flag_ignores_speech_flags() {
    // Nothing listens on the node, so the request itself fails: the flags were let through.
    for flag in ["--stt-socket", "--stt-node"] {
        plain()
            .env_remove("ZETA_STT_SOCKET")
            .env_remove("ZETA_STT_NODE")
            .args(["--node", "http://127.0.0.1:9", "--secret", SECRET])
            .args([flag, "x", "source", "list"])
            .assert()
            .code(1)
            .stdout("")
            .stderr(contains("unreachable"))
            .stderr(contains("with --node").not());
    }
}

#[test]
fn stt_node_requires_its_secret() {
    let dir = tempfile::tempdir().unwrap();
    plain()
        .env_remove("ZETA_STT_SOCKET")
        .env_remove("ZETA_STT_NODE_SECRET")
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .args(["serve", "--listen", "127.0.0.1:0", "--secret", SECRET])
        .args(["--stt-node", "http://127.0.0.1:9"])
        .assert()
        .code(1)
        .stderr(contains("ZETA_STT_NODE_SECRET"));
}

#[test]
fn then_tail_with_node_is_usage_error() {
    // Rejected before any request, so no node needs to be listening.
    plain()
        .args([
            "--node",
            "http://127.0.0.1:9",
            "--secret",
            SECRET,
            "job",
            "start",
            "--kind",
            "aprs_decode",
            "--source",
            "aprs",
            "--center-hz",
            "144800000",
            "--bandwidth-hz",
            "12500",
            "--then-tail",
            "1,1000",
        ])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains("--then-tail is not supported with --node yet"));
}
