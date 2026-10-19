//! `zeta ask` against a fake OpenAI-compatible model.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::{ServeProc, zeta_remote};
use serde_json::{Value, json};
use zeta_agent::fake::{FakeLlm, text_response, tool_call_response};
use zeta_source_file::fixtures::write_tone;

const SECRET: &str = "a-long-enough-secret";

/// Run `zeta <args>` (stdin piped from `stdin` when given) off the async runtime,
/// so the in-process fake keeps serving meanwhile.
async fn zeta(args: Vec<String>, stdin: Option<&'static str>) -> Output {
    tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_zeta"));
        for k in [
            "ZETA_NODE",
            "ZETA_SECRET",
            "ZETA_LLM_URL",
            "ZETA_LLM_MODEL",
            "ZETA_LLM_KEY",
        ] {
            cmd.env_remove(k);
        }
        cmd.args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn zeta");
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn local_args(dir: &Path, meta: &Path, fake: &FakeLlm, rest: &[&str]) -> Vec<String> {
    let mut a = s(&["--state-dir"]);
    a.push(dir.join("state").display().to_string());
    a.push("--source-file".into());
    a.push(format!("tone={}", meta.display()));
    a.extend(s(rest));
    a.extend(s(&["--llm-url", &fake.url, "--llm-model", "fake"]));
    a
}

fn json_out(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {:?}",
            String::from_utf8_lossy(&o.stdout)
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_local_lists_sources_via_fake_llm() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let fake = FakeLlm::start(
        vec![
            (200, tool_call_response(&[("source_list", json!({}))])),
            (200, text_response("tone", "stop")),
        ],
        vec![],
    )
    .await;
    let out = zeta(
        local_args(
            dir.path(),
            &meta,
            &fake,
            &["ask", "what sources are there?"],
        ),
        None,
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let t = json_out(&out);
    assert_eq!(t["calls"][0]["tool"], "source_list");
    assert_eq!(t["calls"][0]["result"]["value"][0]["id"], "tone");
    assert_eq!(t["answer"], "tone");
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_reads_request_from_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let fake = FakeLlm::start(
        vec![
            (200, tool_call_response(&[("source_list", json!({}))])),
            (200, text_response("tone", "stop")),
        ],
        vec![],
    )
    .await;
    let out = zeta(
        local_args(dir.path(), &meta, &fake, &["ask"]),
        Some("what sources are there?\n"),
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let reqs = fake.requests.lock().unwrap();
    assert_eq!(reqs[0]["messages"][1]["content"], "what sources are there?");
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_prose_on_command_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let fake = FakeLlm::start(vec![(200, text_response("ok", "stop"))], vec![]).await;
    let out = zeta(
        local_args(dir.path(), &meta, &fake, &["ask", "tune tone to 100 MHz"]),
        None,
    )
    .await;
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(json_out(&out)["error"], "no_tool_call");
}

#[tokio::test(flavor = "multi_thread")]
async fn system_prompt_and_tools_come_from_agents_md() {
    let dir = tempfile::tempdir().unwrap();
    let meta = write_tone(dir.path());
    let fake = FakeLlm::start(vec![(200, text_response("ok", "stop"))], vec![]).await;
    let out = zeta(
        local_args(
            dir.path(),
            &meta,
            &fake,
            &["ask", "what sources are there?"],
        ),
        None,
    )
    .await;
    assert!(out.status.code().is_some(), "{out:?}");
    let md =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../AGENTS.md")).unwrap();
    let reqs = fake.requests.lock().unwrap();
    assert_eq!(
        reqs[0]["messages"][0]["content"],
        zeta_client::system_prompt(&md)
    );
    let names: Vec<&str> = reqs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    let schema = zeta_core::ops::schema();
    let ops: Vec<&str> = schema["op"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["properties"]["op"]["const"].as_str().unwrap())
        .collect();
    assert_eq!(names, ops);
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_remote_starts_job_on_serving_node() {
    let dir = tempfile::tempdir().unwrap();
    let p = ServeProc::start(dir.path(), SECRET);
    assert_eq!(p.dir, dir.path());
    let fake = FakeLlm::start(
        vec![
            (
                200,
                tool_call_response(&[(
                    "job_start",
                    json!({"kind":"passthrough","source":"tone","center_hz":100000000,"bandwidth_hz":48000,"deviation_hz":null,"sideband":null}),
                )]),
            ),
            (200, text_response("Started.", "stop")),
        ],
        vec![],
    )
    .await;
    let mut a = s(&["--node", &p.url, "--secret", SECRET, "ask"]);
    a.push("start a passthrough job on tone at 100 MHz, 48 kHz wide".into());
    a.extend(s(&["--llm-url", &fake.url, "--llm-model", "fake"]));
    let out = zeta(a, None).await;
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let url = p.url.clone();
    let listed = tokio::task::spawn_blocking(move || {
        zeta_remote(&url, SECRET, &["job", "list"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    })
    .await
    .unwrap();
    let jobs: Value = serde_json::from_slice(&listed).unwrap();
    let arr = jobs["value"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "{jobs}");
    assert_eq!(arr[0]["state"]["status"], "running");
}
