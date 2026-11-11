//! The agent loop driven over the fake chat server.
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use zeta_agent::agent::{Allow, GATED_REPLACE_OPS, OwnJobs, gate};
use zeta_agent::fake::{
    FakeLlm, RecordingExecutor, text_response, tool_call_response, tool_call_response_with,
};
use zeta_agent::{
    Agent, AgentConfig, AgentError, AgentFailure, Caller, Executor, FocusFrom, LlmClient,
    LlmConfig, LocalExecutor, OffDefault, RadioContext, RadioPolicy, is_question, resolve,
};
use zeta_client::op_tools;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Hz, SourceId};
use zeta_source_file::fixtures::write_tone;
use zeta_source_file::{FileOpts, FileSource};

const SYSTEM: &str = "Call operations, do not explain.";

fn llm(fake: &FakeLlm) -> LlmClient {
    let mut c = LlmConfig::new(fake.url.clone(), "floor-model");
    c.backoff_initial = Duration::from_millis(10);
    c.backoff_max = Duration::from_millis(10);
    c.ready_timeout = Duration::from_secs(5);
    c.request_timeout = Duration::from_secs(5);
    LlmClient::new(c).unwrap()
}

fn cfg() -> AgentConfig {
    AgentConfig::new(SYSTEM, op_tools(&zeta_core::ops::schema(), ""))
}

async fn fake(chat: Vec<Value>) -> FakeLlm {
    FakeLlm::start(chat.into_iter().map(|b| (200, b)).collect(), vec![]).await
}

fn requests(fake: &FakeLlm) -> Vec<Value> {
    fake.requests.lock().unwrap().clone()
}

/// The `source_list` every run applies before its first completion (the pre-read).
fn pre_read() -> Value {
    json!({"op": "source_list"})
}

/// The system message of a run whose pre-read gave no source list (a `RecordingExecutor`
/// answers `{"result": "ok"}`): the prompt and the no-context paragraph.
fn system_without_sources(request: &str) -> String {
    let section = resolve(request, &RadioContext::default(), &[], Caller::cli()).prompt_section();
    format!("{SYSTEM}\n{section}")
}

/// A node on `dir/state` with a looping `tone` source (as zeta-ws's test harness opens it).
async fn tone_node(dir: &std::path::Path) -> Arc<Node> {
    let meta = write_tone(dir);
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let source =
        Arc::new(FileSource::open(SourceId("tone".to_string()), &meta, opts).expect("open tone"));
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    let cfg = NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 10.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: false,
        bank_factory: None,
        modes_dir: None,
    };
    let ports = vec![source.clone() as Arc<dyn Port>];
    let node = Arc::new(Node::open(cfg, registry, ports).await.expect("open node"));
    source.start();
    node
}

#[test]
fn is_question_cases() {
    assert!(is_question("What radios do I have connected?"));
    assert!(!is_question("can you hear any weather balloons right now"));
    assert!(is_question("can I see the job list?"));
    assert!(!is_question("could you start a passthrough job on tone"));
    assert!(!is_question("do a scan of 433 MHz"));
    assert!(is_question("does tone have a job?"));
    assert!(is_question("  how many jobs"));
    assert!(!is_question(
        "Tune hackrf0 to 101.5 MHz with 200 kHz bandwidth."
    ));
    assert!(!is_question("start decoding ADS-B on hackrf0."));
}

#[tokio::test]
async fn tool_call_then_prose_is_answer() {
    let fake = fake(vec![
        tool_call_response(&[("source_list", json!({}))]),
        text_response("One source: tone.", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), exec, cfg());

    let t = agent.run("list the sources").await.unwrap();
    assert_eq!(t.answer.as_deref(), Some("One source: tone."));
    assert_eq!(t.calls.len(), 1);
    assert_eq!(t.calls[0].tool, "source_list");
    assert!(t.calls[0].ok);
    assert_eq!(t.turns, 2);
    // The pre-read's answer is no source list: no selection.
    assert!(t.selection.is_none());

    let reqs = requests(&fake);
    assert_eq!(reqs.len(), 2);
    let messages = &reqs[1]["messages"];
    assert_eq!(
        messages,
        &json!([
            {"role": "system", "content": system_without_sources("list the sources")},
            {"role": "user", "content": "list the sources"},
            {"role": "assistant", "content": "", "tool_calls": [{
                "id": "call_0", "type": "function",
                "function": {"name": "source_list", "arguments": "{}"},
            }]},
            {"role": "tool", "content": json!({"result": "ok"}).to_string(), "tool_call_id": "call_0"},
        ])
    );
    for req in &reqs {
        assert!(!req.to_string().contains("reasoning_content"));
    }
}

#[tokio::test]
async fn op_error_is_fed_back_so_model_can_correct() {
    let dir = tempfile::tempdir().unwrap();
    let node = tone_node(dir.path()).await;
    let fake = fake(vec![
        tool_call_response(&[(
            "source_ensure",
            json!({"source": "tone", "center_hz": 100000, "bandwidth_hz": 48000, "replace": []}),
        )]),
        tool_call_response(&[(
            "source_ensure",
            json!({"source": "tone", "center_hz": 100000000, "bandwidth_hz": 48000, "replace": []}),
        )]),
        text_response("Tuned.", "stop"),
    ])
    .await;
    let agent = Agent::new(llm(&fake), LocalExecutor::new(node), cfg());

    let t = agent.run("tune tone to 100 MHz").await.unwrap();
    assert_eq!(t.calls.len(), 2);
    assert!(!t.calls[0].ok);
    assert_eq!(t.calls[0].result["suggested"], json!(100000000));
    assert!(t.calls[1].ok, "{:?}", t.calls[1]);
    assert_eq!(t.answer.as_deref(), Some("Tuned."));

    let reqs = requests(&fake);
    let last = reqs[1]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "tool");
    assert!(
        last["content"]
            .as_str()
            .unwrap()
            .contains("\"suggested\":100000000"),
        "{last}"
    );
}

#[tokio::test]
async fn unknown_tool_is_fed_back_with_known_tools() {
    let fake = fake(vec![
        tool_call_response(&[(
            "tune",
            json!({"source": "tone", "center_hz": 1, "bandwidth_hz": 1}),
        )]),
        tool_call_response(&[("source_list", json!({}))]),
        text_response("Done.", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, cfg());

    let t = agent.run("tune tone").await.unwrap();
    assert_eq!(
        *exec.calls.lock().unwrap(),
        [pre_read(), json!({"op": "source_list"})],
        "tune reached the node"
    );
    assert!(!t.calls[0].ok);
    let r = &t.calls[0].result;
    assert_eq!(r["error"], "unknown");
    assert_eq!(r["field"], "tool");
    assert_eq!(r["value"], "tune");
    let known: Vec<&str> = r["known"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(known.contains(&"source_ensure"), "{known:?}");
    assert_eq!(t.answer.as_deref(), Some("Done."));
}

#[tokio::test]
async fn prose_on_command_is_no_tool_call_error() {
    let fake = fake(vec![text_response(
        "hackrf0.set_frequency(101.5e6)",
        "stop",
    )])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, cfg());

    let err: AgentFailure = agent
        .run("Tune hackrf0 to 101.5 MHz with 200 kHz bandwidth.")
        .await
        .unwrap_err();
    match &err.error {
        AgentError::NoToolCall { content } => assert!(content.contains("set_frequency")),
        other => panic!("expected NoToolCall, got {other:?}"),
    }
    assert!(err.transcript.calls.is_empty());
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
}

#[tokio::test]
async fn prose_on_question_is_answer() {
    let fake = fake(vec![text_response("None that I can see.", "stop")]).await;
    let agent = Agent::new(llm(&fake), RecordingExecutor::new(vec![]), cfg());

    let t = agent.run("What radios do I have connected?").await.unwrap();
    assert_eq!(t.answer.as_deref(), Some("None that I can see."));
    assert!(t.calls.is_empty());
}

#[tokio::test]
async fn malformed_arguments_are_fed_back() {
    let mut bad = tool_call_response(&[("source_list", json!({}))]);
    bad["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] = json!("{not json");
    let fake = fake(vec![
        bad,
        tool_call_response(&[("source_list", json!({}))]),
        text_response("Listed.", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, cfg());

    let t = agent.run("list the sources").await.unwrap();
    assert_eq!(t.calls.len(), 2);
    assert!(!t.calls[0].ok);
    assert_eq!(t.calls[0].result["error"], "other");
    assert!(
        t.calls[0].result["message"]
            .as_str()
            .unwrap()
            .contains("arguments")
    );
    assert!(t.calls[1].ok);
    assert_eq!(
        *exec.calls.lock().unwrap(),
        vec![pre_read(), json!({"op": "source_list"})]
    );
}

#[tokio::test]
async fn turn_limit_stops_loop() {
    let call = tool_call_response(&[("job_list", json!({}))]);
    let fake = fake(vec![call.clone(), call.clone(), call]).await;
    let mut cfg = cfg();
    cfg.max_turns = 3;
    let agent = Agent::new(llm(&fake), RecordingExecutor::new(vec![]), cfg);

    let err = agent.run("keep listing jobs").await.unwrap_err();
    assert!(
        matches!(err.error, AgentError::TurnLimit { turns: 3 }),
        "{:?}",
        err.error
    );
    assert_eq!(err.transcript.turns, 3);
    assert_eq!(err.transcript.calls.len(), 3);
    assert_eq!(requests(&fake).len(), 3);
}

#[test]
fn max_turns_defaults_to_eight() {
    assert_eq!(cfg().max_turns, 8);
}

/// A `job_list` result reaches the model without each record's `graph`
/// (hundreds of bytes per job the model never needs); the transcript shows
/// the same trimmed result.
#[tokio::test]
async fn job_list_reaches_the_model_without_graphs() {
    let fake = fake(vec![
        tool_call_response(&[("job_list", json!({}))]),
        text_response("Two jobs.", "stop"),
    ])
    .await;
    let record = |id: &str| {
        json!({
            "id": id, "kind": "spectrum", "source": "tone", "format": 1,
            "graph": {"id": "g", "nodes": [{"id": "n", "kind": "fft", "params": {"size": 1024}}], "edges": []},
            "tuning": {"center_hz": 100000000, "bandwidth_hz": 48000},
            "state": {"status": "stopped"}, "context": {}, "created_ns": 1
        })
    };
    let listed = json!({"result": "jobs", "value": [record("a"), record("b")]});
    let exec = RecordingExecutor::new(vec![("job_list", Ok(listed))]);
    let agent = Agent::new(llm(&fake), exec, cfg());

    let t = agent.run("list the jobs").await.unwrap();
    assert_eq!(t.calls.len(), 1);
    let value = t.calls[0].result["value"].as_array().unwrap();
    assert_eq!(value.len(), 2);
    for r in value {
        assert!(r.get("graph").is_none(), "{r}");
        assert_eq!(r["kind"], "spectrum");
        assert_eq!(r["state"]["status"], "stopped");
    }
    let reqs = requests(&fake);
    let tool_msg = reqs[1]["messages"][3]["content"].as_str().unwrap();
    assert!(!tool_msg.contains("graph"), "{tool_msg}");
    assert!(tool_msg.contains("\"kind\":\"spectrum\""), "{tool_msg}");
}

fn start_with_replace(replace: Value) -> Value {
    tool_call_response(&[(
        "job_start",
        json!({"kind": "passthrough", "source": "tone", "center_hz": 1, "bandwidth_hz": 1,
               "deviation_hz": null, "sideband": null, "elastic": false, "replace": replace}),
    )])
}

#[tokio::test]
async fn job_stop_is_refused_until_stops_are_allowed() {
    let fake = fake(vec![
        tool_call_response(&[("job_stop", json!({"job": "01X"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("stop it").await.unwrap();
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
    assert!(!t.calls[0].ok);
    let r = &t.calls[0].result;
    assert_eq!(r["error"], "not_allowed");
    assert_eq!(r["field"], "op");
    assert_eq!(r["op"], "job_stop");
    let reqs = requests(&fake);
    assert!(reqs[1]["messages"].to_string().contains("not_allowed"));
}

#[tokio::test]
async fn job_stop_is_applied_when_stops_are_allowed() {
    let fake = fake(vec![
        tool_call_response(&[("job_stop", json!({"job": "01X"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let mut c = cfg();
    c.allow = Allow {
        stop_jobs: true,
        replace: false,
        delete_captures: false,
        accept_signatures: false,
    };
    let agent = Agent::new(llm(&fake), &exec, c);
    let t = agent.run("stop it").await.unwrap();
    assert_eq!(applied_ops(&exec), ["source_list", "job_stop"]);
    assert!(t.calls[0].ok);
}

#[tokio::test]
async fn a_non_empty_replace_is_refused_until_replace_is_allowed() {
    let fake1 = fake(vec![
        start_with_replace(json!(["01X"])),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake1), &exec, cfg());
    let t = agent.run("start").await.unwrap();
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
    let r = &t.calls[0].result;
    assert!(!t.calls[0].ok);
    assert_eq!(r["error"], "not_allowed");
    assert_eq!(r["field"], "replace");
    assert_eq!(r["op"], "job_start");

    let fake2 = fake(vec![
        start_with_replace(json!(["01X"])),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let mut c = cfg();
    c.allow = Allow {
        replace: true,
        ..Allow::default()
    };
    let agent = Agent::new(llm(&fake2), &exec, c);
    let t = agent.run("start").await.unwrap();
    assert_eq!(applied_ops(&exec), ["source_list", "job_start"]);
    assert!(t.calls[0].ok);
}

#[tokio::test]
async fn an_empty_replace_passes_the_gate() {
    let fake = fake(vec![
        start_with_replace(json!([])),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    agent.run("start").await.unwrap();
    assert_eq!(applied_ops(&exec), ["source_list", "job_start"]);
}

#[test]
fn jobs_stop_needs_stops_allowed() {
    let op = json!({"op": "jobs_stop", "source": null, "kind": null, "plan": null});
    let refused = gate(&op, Allow::default()).expect("refused");
    assert_eq!(refused["error"], "not_allowed");
    assert_eq!(refused["op"], "jobs_stop");
    let allowed = Allow {
        stop_jobs: true,
        replace: false,
        delete_captures: false,
        accept_signatures: false,
    };
    assert!(gate(&op, allowed).is_none());
}

#[test]
fn gate_covers_every_replace_taking_op() {
    // Every op whose schema takes a `replace` stops jobs with it: each must be gated, so
    // a new one cannot reach the agent ungated.
    let schema = zeta_core::ops::schema();
    let variants = schema["op"]["oneOf"]
        .as_array()
        .or_else(|| schema["op"]["anyOf"].as_array())
        .expect("op variants");
    let takes_replace: Vec<&str> = variants
        .iter()
        .filter(|v| v["properties"].get("replace").is_some())
        .filter_map(|v| v["properties"]["op"]["const"].as_str())
        .collect();
    assert!(
        takes_replace.contains(&"source_measure"),
        "{takes_replace:?}"
    );
    for name in &takes_replace {
        assert!(GATED_REPLACE_OPS.contains(name), "{name} is not gated");
        assert!(
            gate(&json!({"op": name, "replace": ["a"]}), Allow::default()).is_some(),
            "{name}"
        );
    }
    assert!(
        gate(
            &json!({"op": "source_unpin", "source": "s"}),
            Allow::default()
        )
        .is_none()
    );
}

#[test]
fn set_start_with_a_replace_needs_replace_allowed() {
    let op = |replace: Value| json!({"op": "set_start", "set": "airband", "radios": {}, "members": null, "replace": replace});
    let refused = gate(&op(json!(["01X"])), Allow::default()).expect("refused");
    assert_eq!(refused["error"], "not_allowed");
    assert_eq!(refused["field"], "replace");
    let allowed = Allow {
        replace: true,
        ..Allow::default()
    };
    assert!(gate(&op(json!(["01X"])), allowed).is_none());
    assert!(gate(&op(json!([])), Allow::default()).is_none());
}

#[test]
fn set_stop_needs_stops_allowed_unless_the_run_started_the_set() {
    let stop = json!({"op": "set_stop", "set": "airband"});
    assert_eq!(
        gate(&stop, Allow::default()).expect("refused")["error"],
        "not_allowed"
    );
    let allowed = Allow {
        stop_jobs: true,
        ..Allow::default()
    };
    assert!(gate(&stop, allowed).is_none());

    let mut own = OwnJobs::default();
    assert!(!own.allows_stop(&stop));
    own.record(
        "set_start",
        &json!({"set": "airband"}),
        &json!({"result": "set_start", "value": {"set": "airband", "started": [
            {"job": "01A", "attached": null},
            {"job": "01B", "attached": "01C"}
        ], "existing": []}}),
    );
    assert!(own.allows_stop(&stop));
    assert!(own.allows_stop(&json!({"op": "job_stop", "job": "01A"})));
    assert!(own.allows_stop(&json!({"op": "job_stop", "job": "01C"})));
    assert!(!own.allows_stop(&json!({"op": "job_stop", "job": "01OLD"})));
    assert!(!own.allows_stop(&json!({"op": "set_stop", "set": "pagers_au"})));
}

#[test]
fn a_set_with_existing_members_is_not_the_runs_to_stop() {
    let stop = json!({"op": "set_stop", "set": "airband"});
    let mut own = OwnJobs::default();
    own.record(
        "set_start",
        &json!({"set": "airband"}),
        &json!({"result": "set_start", "value": {"set": "airband",
            "started": [{"job": "01A", "attached": null}],
            "existing": [{"job": "01OLD"}]}}),
    );
    assert!(!own.allows_stop(&stop));
    assert!(own.allows_stop(&json!({"op": "job_stop", "job": "01A"})));
    assert!(!own.allows_stop(&json!({"op": "job_stop", "job": "01OLD"})));
}

#[test]
fn a_narrowed_set_start_is_not_the_runs_set_to_stop() {
    // `members` narrows the call: the operator's instance may run the other members,
    // and the `existing` answer only covers the members named.
    let stop = json!({"op": "set_stop", "set": "airband"});
    let mut own = OwnJobs::default();
    own.record(
        "set_start",
        &json!({"set": "airband", "members": ["vdl2"]}),
        &json!({"result": "set_start", "value": {"set": "airband",
            "started": [{"job": "01V", "attached": null}],
            "existing": []}}),
    );
    assert!(!own.allows_stop(&stop));
    assert!(own.allows_stop(&json!({"op": "job_stop", "job": "01V"})));
}

#[test]
fn set_list_passes_the_gate() {
    assert!(gate(&json!({"op": "set_list"}), Allow::default()).is_none());
}

#[tokio::test]
async fn turn_log_keeps_reasoning_and_content_per_completion() {
    let fake = fake(vec![
        tool_call_response_with(
            &[("source_list", json!({}))],
            "Looking at the sources.",
            Some("plan: list first"),
        ),
        text_response("two sources", "stop"),
    ])
    .await;
    let agent = Agent::new(llm(&fake), RecordingExecutor::new(vec![]), cfg());

    let t = agent.run("list the sources").await.unwrap();
    let log = serde_json::to_value(&t.turn_log).unwrap();
    assert_eq!(
        log,
        json!([
            {"turn": 1, "reasoning": "plan: list first", "content": "Looking at the sources.", "calls": 1},
            {"turn": 2, "reasoning": null, "content": "two sources", "calls": 0},
        ])
    );
    assert_eq!(t.allow, Allow::default());
    assert!(
        !serde_json::to_string(&requests(&fake))
            .unwrap()
            .contains("reasoning_content")
    );
}

#[tokio::test]
async fn a_by_request_fake_answers_the_script_the_request_names() {
    let script = |answer: &str| {
        vec![
            (200, tool_call_response(&[("source_list", json!({}))])),
            (200, text_response(answer, "stop")),
        ]
    };
    let fake = FakeLlm::start_by_request(
        vec![
            ("alpha".to_string(), script("alpha answer")),
            ("beta".to_string(), script("beta answer")),
        ],
        vec![],
    )
    .await;
    let agent = Agent::new(llm(&fake), RecordingExecutor::new(vec![]), cfg());

    let t = agent.run("please alpha").await.unwrap();
    assert_eq!(t.answer.as_deref(), Some("alpha answer"));
    let t = agent.run("beta now").await.unwrap();
    assert_eq!(t.answer.as_deref(), Some("beta answer"));
    // The same request again is a new conversation, answered from the script's start.
    let t = agent.run("please alpha").await.unwrap();
    assert_eq!(t.answer.as_deref(), Some("alpha answer"));
    assert_eq!(t.calls.len(), 1);

    let err = agent.run("gamma").await.unwrap_err();
    match &err.error {
        AgentError::Llm { status: 500, body } => {
            assert!(body.contains("fake: no script for gamma"), "{body}")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_fake_llm_binary_serves_its_scripts_file() {
    use std::io::BufRead;
    let dir = tempfile::tempdir().unwrap();
    let scripts = dir.path().join("scripts.json");
    let file = json!([{"match": "alpha", "replies": [
        tool_call_response(&[("source_list", json!({}))]),
        text_response("alpha answer", "stop"),
    ]}]);
    std::fs::write(&scripts, file.to_string()).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_zeta-fake-llm"))
        .args(["--listen", "127.0.0.1:0", "--scripts"])
        .arg(&scripts)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn zeta-fake-llm");
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let url = line.trim().strip_prefix("listening ").map(str::to_string);
    let run = async move {
        let url = url.unwrap_or_else(|| panic!("zeta-fake-llm said {line:?}"));
        let mut c = LlmConfig::new(url, "floor-model");
        c.ready_timeout = Duration::from_secs(5);
        c.request_timeout = Duration::from_secs(5);
        let agent = Agent::new(
            LlmClient::new(c).unwrap(),
            RecordingExecutor::new(vec![]),
            cfg(),
        );
        let t = agent.run("please alpha").await.unwrap();
        assert_eq!(t.answer.as_deref(), Some("alpha answer"));
        assert!(agent.run("beta").await.is_err(), "no script for beta");
    };
    // Kill the child whether or not the run passed.
    let outcome = tokio::spawn(run).await;
    let _ = child.kill();
    let _ = child.wait();
    outcome.unwrap();
}

fn start_args(kind: &str) -> Value {
    json!({"kind": kind, "source": "tone", "center_hz": 1, "bandwidth_hz": 1,
           "deviation_hz": null, "sideband": null, "elastic": false, "replace": []})
}

fn started(id: &str) -> Result<Value, Value> {
    Ok(json!({"result": "job", "value": id}))
}

fn applied_ops(exec: &RecordingExecutor) -> Vec<String> {
    exec.calls
        .lock()
        .unwrap()
        .iter()
        .map(|o| o["op"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_run_stops_the_job_it_started() {
    let fake = fake(vec![
        tool_call_response(&[("job_start", start_args("measure"))]),
        tool_call_response(&[("job_stop", json!({"job": "J1"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![("job_start", started("J1"))]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("measure it").await.unwrap();
    assert_eq!(applied_ops(&exec), ["source_list", "job_start", "job_stop"]);
    assert!(t.calls[1].ok, "{}", t.calls[1].result);
}

#[tokio::test]
async fn a_run_stops_what_plan_start_gave_it() {
    let planned = json!({"result": "plan_start", "value": {
        "started": [{"job": "J1", "attached": "T1"}, {"job": "J2", "attached": null}],
        "existing": [{"job": "E1"}], "refused": [], "warnings": []}});
    let plan_args = json!({"plan": "p", "kind": "fm_demod", "channels": null, "deviation_hz": null,
        "sideband": null, "attach": null, "language": null, "source": "tone", "replace": []});
    let mut script = vec![tool_call_response(&[("plan_start", plan_args)])];
    for j in ["J1", "T1", "J2", "E1"] {
        script.push(tool_call_response(&[("job_stop", json!({"job": j}))]));
    }
    script.push(text_response("done", "stop"));
    let fake = fake(script).await;
    let exec = RecordingExecutor::new(vec![("plan_start", Ok(planned))]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("start the plan").await.unwrap();
    assert!(t.calls[1].ok && t.calls[2].ok && t.calls[3].ok);
    assert!(!t.calls[4].ok);
    assert_eq!(t.calls[4].result["error"], "not_allowed");
}

#[tokio::test]
async fn a_run_stops_the_job_it_attached() {
    let fake = fake(vec![
        tool_call_response(&[(
            "job_attach",
            json!({"kind": "transcribe", "job": "J1", "language": null}),
        )]),
        tool_call_response(&[("job_stop", json!({"job": "T1"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![("job_attach", started("T1"))]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("attach").await.unwrap();
    assert!(t.calls[1].ok, "{}", t.calls[1].result);
}

#[tokio::test]
async fn identify_verdict_job_is_its_own_once_read() {
    let ctx = json!({"result": "job_context", "value": {"identify": {"verdict": {"job": "V1"}}}});
    let fake = fake(vec![
        tool_call_response(&[("job_start", start_args("identify"))]),
        tool_call_response(&[("job_stop", json!({"job": "V1"}))]),
        tool_call_response(&[("job_context", json!({"job": "I1"}))]),
        tool_call_response(&[("job_stop", json!({"job": "V1"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![("job_start", started("I1")), ("job_context", Ok(ctx))]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("identify").await.unwrap();
    assert!(!t.calls[1].ok);
    assert_eq!(t.calls[1].result["error"], "not_allowed");
    assert!(t.calls[3].ok, "{}", t.calls[3].result);
}

#[tokio::test]
async fn a_verdict_read_from_another_jobs_context_is_not_its_own() {
    let ctx = json!({"result": "job_context", "value": {"identify": {"verdict": {"job": "V1"}}}});
    let fake = fake(vec![
        tool_call_response(&[("job_context", json!({"job": "X"}))]),
        tool_call_response(&[("job_stop", json!({"job": "V1"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![("job_context", Ok(ctx))]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("context").await.unwrap();
    assert!(!t.calls[1].ok);
    assert_eq!(t.calls[1].result["error"], "not_allowed");
}

#[tokio::test]
async fn a_failed_start_records_nothing() {
    let fake = fake(vec![
        tool_call_response(&[("job_start", start_args("measure"))]),
        tool_call_response(&[("job_stop", json!({"job": "J9"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![(
        "job_start",
        Err(json!({"error": "duplicate", "job": "J9"})),
    )]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("start").await.unwrap();
    assert!(!t.calls[1].ok);
    assert_eq!(t.calls[1].result["error"], "not_allowed");
}

#[tokio::test]
async fn another_id_is_still_refused() {
    let fake = fake(vec![
        tool_call_response(&[("job_start", start_args("measure"))]),
        tool_call_response(&[("job_stop", json!({"job": "X"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![("job_start", started("J1"))]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("start").await.unwrap();
    assert!(!t.calls[1].ok);
    assert_eq!(t.calls[1].result["error"], "not_allowed");
    assert_eq!(t.calls[1].result["field"], "op");
}

#[tokio::test]
async fn jobs_stop_and_replace_stay_refused() {
    let mut with_replace = start_args("measure");
    with_replace["replace"] = json!(["J1"]);
    let fake = fake(vec![
        tool_call_response(&[("job_start", start_args("measure"))]),
        tool_call_response(&[(
            "jobs_stop",
            json!({"source": null, "kind": null, "plan": null}),
        )]),
        tool_call_response(&[("job_start", with_replace)]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![("job_start", started("J1"))]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("start").await.unwrap();
    assert!(!t.calls[1].ok);
    assert_eq!(t.calls[1].result["op"], "jobs_stop");
    assert!(!t.calls[2].ok);
    assert_eq!(t.calls[2].result["field"], "replace");
}

#[tokio::test]
async fn a_second_run_cannot_stop_the_first_runs_job() {
    let fake = fake(vec![
        tool_call_response(&[("job_start", start_args("measure"))]),
        text_response("started", "stop"),
        tool_call_response(&[("job_stop", json!({"job": "J1"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![("job_start", started("J1"))]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    agent.run("start").await.unwrap();
    let t = agent.run("stop it").await.unwrap();
    assert!(!t.calls[0].ok);
    assert_eq!(t.calls[0].result["error"], "not_allowed");
}

#[tokio::test]
async fn capture_delete_needs_delete_captures_allowed() {
    let first = fake(vec![
        tool_call_response(&[("capture_delete", json!({"capture": "01X"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&first), &exec, cfg());
    let t = agent.run("delete it").await.unwrap();
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
    let r = &t.calls[0].result;
    assert_eq!(r["error"], "not_allowed");
    assert_eq!(r["field"], "op");
    assert_eq!(r["op"], "capture_delete");

    let second = fake(vec![
        tool_call_response(&[("capture_delete", json!({"capture": "01X"}))]),
        text_response("done", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let mut c = cfg();
    c.allow = Allow {
        delete_captures: true,
        ..Allow::default()
    };
    let agent = Agent::new(llm(&second), &exec, c);
    agent.run("delete it").await.unwrap();
    assert_eq!(applied_ops(&exec), ["source_list", "capture_delete"]);
}

#[test]
fn gate_covers_every_delete_taking_op() {
    assert!(
        gate(
            &json!({"op": "capture_delete", "capture": "a"}),
            Allow::default()
        )
        .is_some()
    );
    let allowed = Allow {
        delete_captures: true,
        ..Allow::default()
    };
    assert!(gate(&json!({"op": "capture_delete", "capture": "a"}), allowed).is_none());
    // Allowing stops does not allow deleting.
    let stops = Allow {
        stop_jobs: true,
        replace: true,
        delete_captures: false,
        accept_signatures: false,
    };
    assert!(gate(&json!({"op": "capture_delete", "capture": "a"}), stops).is_some());
    assert!(
        gate(
            &json!({"op": "capture_list", "signal": null}),
            Allow::default()
        )
        .is_none()
    );
}

/// `cfg()` with the page-only tools on, the page's ready themes `console` and `plain`.
fn page_cfg() -> AgentConfig {
    let mut c = cfg();
    c.enable_page_tools(vec!["console".to_string(), "plain".to_string()]);
    c
}

#[tokio::test]
async fn view_set_is_recorded_for_the_page_and_never_reaches_the_executor() {
    let fake = fake(vec![
        tool_call_response(&[("view_set", json!({"key": "theme", "value": "plain"}))]),
        text_response("Switched to plain.", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, page_cfg());

    let t = agent.run("switch to the plain theme").await.unwrap();
    assert_eq!(t.answer.as_deref(), Some("Switched to plain."));
    assert_eq!(t.calls.len(), 1);
    let call = &t.calls[0];
    assert_eq!(call.tool, "view_set");
    assert!(call.ok, "{call:?}");
    assert_eq!(call.arguments, json!({"key": "theme", "value": "plain"}));
    assert_eq!(
        call.result,
        json!({"page": true, "key": "theme", "value": "plain", "applied": "by the page when this answer arrives"})
    );
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
    // The tool is offered to the model.
    let reqs = requests(&fake);
    let names: Vec<&str> = reqs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"view_set"), "{names:?}");
}

#[tokio::test]
async fn view_set_refuses_a_theme_that_is_not_ready_listing_the_ready_ids() {
    let fake = fake(vec![
        tool_call_response(&[("view_set", json!({"key": "theme", "value": "neon"}))]),
        text_response("No such theme.", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, page_cfg());

    let t = agent.run("switch to the neon theme").await.unwrap();
    let call = &t.calls[0];
    assert!(!call.ok);
    assert_eq!(call.result["error"], "unknown");
    assert_eq!(call.result["field"], "value");
    assert_eq!(call.result["known"], json!(["console", "plain"]));
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
}

#[tokio::test]
async fn view_set_names_a_missing_or_unknown_field() {
    let fake = fake(vec![
        tool_call_response(&[
            ("view_set", json!({"key": "theme"})),
            (
                "view_set",
                json!({"key": "side", "value": "jobs", "tab": "1"}),
            ),
            ("view_set", json!({"key": "side", "value": "jobs"})),
        ]),
        text_response("Done.", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, page_cfg());

    let t = agent.run("show only jobs").await.unwrap();
    assert!(!t.calls[0].ok);
    assert_eq!(t.calls[0].result["field"], "value");
    assert!(!t.calls[1].ok);
    assert_eq!(t.calls[1].result["field"], "tab");
    // Other keys pass through: the page validates them.
    assert!(t.calls[2].ok, "{:?}", t.calls[2]);
    assert_eq!(t.calls[2].result["key"], "side");
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
}

#[tokio::test]
async fn view_set_is_not_gated_by_the_allowance() {
    let fake = fake(vec![
        tool_call_response(&[("view_set", json!({"key": "theme", "value": "console"}))]),
        text_response("Switched.", "stop"),
    ])
    .await;
    let mut c = page_cfg();
    c.allow = Allow {
        stop_jobs: false,
        replace: false,
        delete_captures: false,
        accept_signatures: false,
    };
    let agent = Agent::new(llm(&fake), RecordingExecutor::new(vec![]), c);
    let t = agent.run("switch to the console theme").await.unwrap();
    assert!(t.calls[0].ok, "{:?}", t.calls[0]);
    assert_eq!(t.calls[0].result["page"], true);
}

#[tokio::test]
async fn view_set_is_an_unknown_tool_without_page_tools() {
    let fake = fake(vec![
        tool_call_response(&[("view_set", json!({"key": "theme", "value": "plain"}))]),
        text_response("Cannot.", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("switch to the plain theme").await.unwrap();
    let call = &t.calls[0];
    assert!(!call.ok);
    assert_eq!(call.result["error"], "unknown");
    assert_eq!(call.result["field"], "tool");
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
    let reqs = requests(&fake);
    assert!(!reqs[0]["tools"].to_string().contains("view_set"));
}

#[tokio::test]
async fn view_set_refuses_keys_that_act_listing_the_presentation_keys() {
    let call = |key: &str, value: &str| ("view_set", json!({"key": key, "value": value}));
    let fake = fake(vec![
        tool_call_response(&[
            call("scanner", "cb"),
            call("zoom_out", "scan"),
            call("home", "-27.5,153"),
            call("auto_scan_stop", "keep"),
            call("volume", "11"),
        ]),
        text_response("Refused.", "stop"),
    ])
    .await;
    let exec = RecordingExecutor::new(vec![]);
    let agent = Agent::new(llm(&fake), &exec, page_cfg());
    let t = agent.run("scan the band from the page").await.unwrap();
    assert_eq!(t.calls.len(), 5);
    for c in &t.calls {
        assert!(!c.ok, "{c:?}");
        assert_eq!(c.result["error"], "unknown", "{c:?}");
        assert_eq!(c.result["field"], "key", "{c:?}");
        assert_eq!(
            c.result["known"],
            json!(zeta_agent::page::VIEW_KEYS),
            "{c:?}"
        );
    }
    assert_eq!(*exec.calls.lock().unwrap(), [pre_read()]);
    // The tool's schema names the allowed keys as an enum.
    let reqs = requests(&fake);
    let tool = reqs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "view_set")
        .unwrap()
        .clone();
    assert_eq!(
        tool["function"]["parameters"]["properties"]["key"]["enum"],
        json!(zeta_agent::page::VIEW_KEYS)
    );
    for key in ["main", "dock", "origin_fade_s"] {
        assert!(
            zeta_agent::page::VIEW_KEYS.contains(&key),
            "{key} is settable"
        );
    }
    for acting in ["scanner", "zoom_out", "auto_scan_stop", "home"] {
        assert!(
            !zeta_agent::page::VIEW_KEYS.contains(&acting),
            "{acting} must not be settable"
        );
    }
}

/// The page refuses the same keys before applying: its `AGENT_VIEW_KEYS` (view.js) is the
/// agent's `VIEW_KEYS`, and every key is one of the page's view policies.
#[test]
fn the_page_allows_the_same_view_keys() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../zeta-ui/web/view.js");
    let js = std::fs::read_to_string(path).unwrap();
    let start = js
        .find("export const AGENT_VIEW_KEYS = [")
        .expect("AGENT_VIEW_KEYS in view.js");
    let rest = &js[start..];
    let list = &rest[rest.find('[').unwrap() + 1..rest.find("];").unwrap()];
    let page: Vec<&str> = list
        .split(',')
        .map(|k| k.trim().trim_matches('"'))
        .filter(|k| !k.is_empty())
        .collect();
    assert_eq!(page, zeta_agent::page::VIEW_KEYS);
    for key in zeta_agent::page::VIEW_KEYS {
        assert!(
            js.contains(&format!("key: \"{key}\"")),
            "{key} is not a view policy"
        );
    }
}

// ---- Radio selection (plan 33): the pre-read, the prompt section, the gate ----

/// A node on `dir/state` with two looping file sources of the tone recording, `tone` and
/// `cb`, tunable over the given ranges (48 kHz wide at most).
async fn two_source_node_ranged(
    dir: &std::path::Path,
    tone_range: (u64, u64),
    cb_range: (u64, u64),
) -> Arc<Node> {
    let meta = write_tone(dir);
    let open = |id: &str, (min, max): (u64, u64)| {
        let opts = FileOpts {
            realtime: true,
            block_samples: 4800,
            repeat: true,
            tune_range: Some((Hz(min), Hz(max))),
        };
        Arc::new(FileSource::open(SourceId(id.to_string()), &meta, opts).expect("open source"))
    };
    let tone = open("tone", tone_range);
    let cb = open("cb", cb_range);
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    let cfg = NodeConfig {
        source_positions: Default::default(),
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 10.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: false,
        bank_factory: None,
        modes_dir: None,
        sets_dir: None,
    };
    let ports = vec![tone.clone() as Arc<dyn Port>, cb.clone() as Arc<dyn Port>];
    let node = Arc::new(Node::open(cfg, registry, ports).await.expect("open node"));
    tone.start();
    cb.start();
    node
}

/// [`two_source_node_ranged`] with both sources over 100..200 MHz.
async fn two_source_node(dir: &std::path::Path) -> Arc<Node> {
    two_source_node_ranged(dir, (100_000_000, 200_000_000), (100_000_000, 200_000_000)).await
}

/// `cfg()` with the radio context `focus` (shown on the page) under `policy`.
fn radio_cfg(policy: RadioPolicy, focus: &str) -> AgentConfig {
    let mut c = cfg();
    c.radio = RadioContext::from_parts(
        Some(SourceId(focus.to_string())),
        FocusFrom::Shown,
        Some(policy),
        vec![],
    );
    c.caller = Caller::page();
    c
}

/// Apply `op` on `node` directly (not through the agent); the executor's answer.
async fn node_apply(node: &Arc<Node>, op: Value) -> Value {
    LocalExecutor::new(node.clone())
        .apply(op)
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("op failed: {e}"))
}

/// A passthrough job on `source` at 150 MHz, 20 kHz wide, holding the band; its id.
async fn hold(node: &Arc<Node>, source: &str) -> String {
    let r = node_apply(
        node,
        json!({"op": "job_start", "kind": "passthrough", "source": source,
               "center_hz": 150_000_000u64, "bandwidth_hz": 20_000, "deviation_hz": null,
               "sideband": null, "elastic": false, "replace": []}),
    )
    .await;
    r["value"].as_str().expect("job id").to_string()
}

/// The ids of the jobs not stopped.
async fn job_ids(node: &Arc<Node>) -> Vec<String> {
    let r = node_apply(node, json!({"op": "job_list"})).await;
    r["value"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| j["state"]["status"] != "stopped")
        .map(|j| j["id"].as_str().unwrap().to_string())
        .collect()
}

/// The model's scripted `job_start` of a passthrough on `source` at 150 MHz.
fn start_on(source: &str) -> Value {
    tool_call_response(&[(
        "job_start",
        json!({"kind": "passthrough", "source": source, "center_hz": 150_000_000u64,
               "bandwidth_hz": 20_000, "deviation_hz": null, "sideband": null,
               "elastic": false, "replace": []}),
    )])
}

/// The model's scripted `source_free` for 150 MHz.
fn free_call() -> Value {
    tool_call_response(&[(
        "source_free",
        json!({"center_hz": 150_000_000u64, "bandwidth_hz": 20_000}),
    )])
}

/// The first chat request's one system message.
fn system_content(fake: &FakeLlm) -> String {
    let reqs = requests(fake);
    let messages = reqs[0]["messages"].as_array().unwrap();
    let systems: Vec<&Value> = messages.iter().filter(|m| m["role"] == "system").collect();
    assert_eq!(systems.len(), 1, "one system message: {messages:?}");
    systems[0]["content"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn the_system_message_ends_with_this_request() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    let fake = fake(vec![text_response("Two radios.", "stop")]).await;
    let agent = Agent::new(
        llm(&fake),
        LocalExecutor::new(node),
        radio_cfg(RadioPolicy::Focus, "cb"),
    );
    let request = "which radios are there?";
    let t = agent.run(request).await.unwrap();
    assert_eq!(t.answer.as_deref(), Some("Two radios."));

    let system = system_content(&fake);
    assert!(system.starts_with(SYSTEM), "{system}");
    assert_eq!(system.matches("## This request").count(), 1, "{system}");
    let at = system.find("## This request").unwrap();
    let section = &system[at..];
    // The section is the last thing in the system message.
    assert!(!section[2..].contains("\n## "), "{section}");
    assert!(section.contains("Default radio: cb"), "{section}");
    let reqs = requests(&fake);
    let users: Vec<&Value> = reqs[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .collect();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["content"], request);
    let sel = t.selection.expect("selection");
    assert_eq!(sel.radio, Some(SourceId("cb".into())));
    assert_eq!(sel.caller, Caller::page());
}

/// Records each applied op with how many chat requests the fake had seen by then.
struct OrderedExecutor {
    inner: LocalExecutor,
    seen: Arc<std::sync::Mutex<Vec<Value>>>,
    log: std::sync::Mutex<Vec<(String, usize)>>,
}

#[async_trait::async_trait]
impl Executor for OrderedExecutor {
    async fn apply(&self, op: Value) -> Result<Result<Value, Value>, AgentError> {
        let n = self.seen.lock().unwrap().len();
        self.log
            .lock()
            .unwrap()
            .push((op["op"].as_str().unwrap_or_default().to_string(), n));
        self.inner.apply(op).await
    }
}

#[tokio::test]
async fn one_source_list_is_read_before_the_first_completion() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    let fake = fake(vec![
        tool_call_response(&[("job_list", json!({}))]),
        text_response("No jobs.", "stop"),
    ])
    .await;
    let exec = OrderedExecutor {
        inner: LocalExecutor::new(node),
        seen: fake.requests.clone(),
        log: Default::default(),
    };
    let agent = Agent::new(llm(&fake), &exec, cfg());
    let t = agent.run("list the jobs").await.unwrap();
    let log = exec.log.lock().unwrap().clone();
    assert_eq!(
        log,
        [("source_list".to_string(), 0), ("job_list".to_string(), 1)]
    );
    assert_eq!(t.calls.len(), 1);
    assert_eq!(t.calls[0].tool, "job_list");
}

#[tokio::test]
async fn strict_refuses_job_start_on_another_radio() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    let fake = fake(vec![start_on("tone"), text_response("Refused.", "stop")]).await;
    let agent = Agent::new(
        llm(&fake),
        LocalExecutor::new(node.clone()),
        radio_cfg(RadioPolicy::Focus, "cb"),
    );
    let t = agent.run("start a passthrough on 150 MHz").await.unwrap();
    assert!(!t.calls[0].ok, "{:?}", t.calls[0]);
    assert_eq!(t.calls[0].result["error"], "policy");
    assert_eq!(t.calls[0].result["default"], "cb");
    let reqs = requests(&fake);
    let tool = reqs[1]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(tool["role"], "tool");
    let content = tool["content"].as_str().unwrap();
    assert!(content.contains("\"error\":\"policy\""), "{content}");
    assert!(content.contains("\"default\":\"cb\""), "{content}");
    assert!(job_ids(&node).await.is_empty());
}

#[tokio::test]
async fn focus_fallback_starts_it_and_records_off_default() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    let fake = fake(vec![
        start_on("tone"),
        text_response("Ran on tone.", "stop"),
    ])
    .await;
    let agent = Agent::new(
        llm(&fake),
        LocalExecutor::new(node.clone()),
        radio_cfg(RadioPolicy::FocusFallback, "cb"),
    );
    let t = agent.run("start a passthrough on 150 MHz").await.unwrap();
    assert!(t.calls[0].ok, "{:?}", t.calls[0]);
    let job = t.calls[0].result["value"].as_str().unwrap().to_string();
    assert_eq!(job_ids(&node).await, std::slice::from_ref(&job));
    let sel = t.selection.expect("selection");
    assert_eq!(
        sel.off_default,
        [OffDefault {
            tool: "job_start".into(),
            source: SourceId("tone".into())
        }]
    );
    node_apply(&node, json!({"op": "job_stop", "job": job})).await;
}

#[tokio::test]
async fn focus_asks_when_the_focus_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    let held = hold(&node, "cb").await;
    let before = job_ids(&node).await;
    assert_eq!(before, std::slice::from_ref(&held));
    let fake = fake(vec![
        free_call(),
        text_response("cb is held; tone is free: use it?", "stop"),
    ])
    .await;
    let agent = Agent::new(
        llm(&fake),
        LocalExecutor::new(node.clone()),
        radio_cfg(RadioPolicy::Focus, "cb"),
    );
    let t = agent.run("start a passthrough on 150 MHz").await.unwrap();
    assert_eq!(
        t.answer.as_deref(),
        Some("cb is held; tone is free: use it?")
    );
    assert!(t.calls[0].ok, "{:?}", t.calls[0]);
    assert_eq!(t.calls[0].result["value"]["free"], json!(["tone"]));
    assert_eq!(job_ids(&node).await, before);
    node_apply(&node, json!({"op": "job_stop", "job": held})).await;
}

#[tokio::test]
async fn ask_policy_answers_after_source_free() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    let fake = fake(vec![
        free_call(),
        text_response("Which radio: tone or cb?", "stop"),
    ])
    .await;
    let agent = Agent::new(
        llm(&fake),
        LocalExecutor::new(node.clone()),
        radio_cfg(RadioPolicy::Ask, "cb"),
    );
    let t = agent.run("start a measure on 150 MHz").await.unwrap();
    assert_eq!(t.answer.as_deref(), Some("Which radio: tone or cb?"));
    assert!(job_ids(&node).await.is_empty());
    let system = system_content(&fake);
    assert!(
        system.contains("No default radio (policy ask)."),
        "{system}"
    );
}

#[tokio::test]
async fn ask_policy_does_not_relax_the_prose_rule() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    let fake = fake(vec![text_response("Which radio: tone or cb?", "stop")]).await;
    let agent = Agent::new(
        llm(&fake),
        LocalExecutor::new(node),
        radio_cfg(RadioPolicy::Ask, "cb"),
    );
    let f: AgentFailure = agent.run("start a measure on 150 MHz").await.unwrap_err();
    assert!(
        matches!(f.error, AgentError::NoToolCall { .. }),
        "{:?}",
        f.error
    );
    // The selection is kept on a failure too.
    assert!(f.transcript.selection.is_some());
}

#[tokio::test]
async fn no_free_radio_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    // cb does not cover 150 MHz; tone does, and a job holds it.
    let node = two_source_node_ranged(
        dir.path(),
        (100_000_000, 200_000_000),
        (300_000_000, 400_000_000),
    )
    .await;
    let held = hold(&node, "tone").await;
    let before = job_ids(&node).await;
    let answer = format!("Nothing free: tone is held by passthrough {held}.");
    let fake = fake(vec![free_call(), text_response(&answer, "stop")]).await;
    let agent = Agent::new(
        llm(&fake),
        LocalExecutor::new(node.clone()),
        radio_cfg(RadioPolicy::FocusFallback, "cb"),
    );
    let t = agent.run("start a passthrough on 150 MHz").await.unwrap();
    assert_eq!(t.answer.as_deref(), Some(answer.as_str()));
    assert_eq!(t.calls[0].result["value"]["free"], json!([]));
    assert_eq!(job_ids(&node).await, before);
    for c in &t.calls {
        let replace = c.arguments["replace"].as_array();
        assert!(replace.is_none_or(|r| r.is_empty()), "{c:?}");
    }
    node_apply(&node, json!({"op": "job_stop", "job": held})).await;
}

#[tokio::test]
async fn a_run_without_context_states_no_radio_in_focus() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    let fake = fake(vec![text_response("Two radios.", "stop")]).await;
    let agent = Agent::new(llm(&fake), LocalExecutor::new(node), cfg());
    let t = agent.run("which radios are there?").await.unwrap();
    let system = system_content(&fake);
    assert!(
        system.contains("No default radio (no radio in focus)"),
        "{system}"
    );
    let sel = t.selection.expect("selection");
    assert_eq!(sel.policy, RadioPolicy::FreeFirst);
    assert_eq!(sel.caller, Caller::cli());
}

#[tokio::test]
async fn a_refused_fallback_start_is_not_off_default() {
    let dir = tempfile::tempdir().unwrap();
    let node = two_source_node(dir.path()).await;
    // 50 MHz is below tone's 100..200 MHz: the node refuses the start.
    let start = tool_call_response(&[(
        "job_start",
        json!({"kind": "passthrough", "source": "tone", "center_hz": 50_000_000u64,
               "bandwidth_hz": 20_000, "deviation_hz": null, "sideband": null,
               "elastic": false, "replace": []}),
    )]);
    let fake = fake(vec![
        start,
        text_response("tone cannot tune 50 MHz.", "stop"),
    ])
    .await;
    let agent = Agent::new(
        llm(&fake),
        LocalExecutor::new(node.clone()),
        radio_cfg(RadioPolicy::FocusFallback, "cb"),
    );
    let t = agent.run("start a passthrough on 50 MHz").await.unwrap();
    assert!(!t.calls[0].ok, "{:?}", t.calls[0]);
    assert_ne!(t.calls[0].result["error"], "policy", "{:?}", t.calls[0]);
    assert!(job_ids(&node).await.is_empty());
    let sel = t.selection.expect("selection");
    assert!(sel.off_default.is_empty(), "{:?}", sel.off_default);
}
