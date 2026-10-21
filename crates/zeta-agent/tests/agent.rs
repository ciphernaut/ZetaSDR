//! The agent loop driven over the fake chat server.
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use zeta_agent::fake::{FakeLlm, RecordingExecutor, text_response, tool_call_response};
use zeta_agent::{
    Agent, AgentConfig, AgentError, AgentFailure, LlmClient, LlmConfig, LocalExecutor, is_question,
};
use zeta_client::op_tools;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::SourceId;
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
        state_dir: dir.join("state"),
        ring_seconds: 10.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: false,
        bank_factory: None,
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

    let reqs = requests(&fake);
    assert_eq!(reqs.len(), 2);
    let messages = &reqs[1]["messages"];
    assert_eq!(
        messages,
        &json!([
            {"role": "system", "content": SYSTEM},
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
    assert_eq!(exec.calls.lock().unwrap().len(), 1, "tune reached the node");
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
    assert!(exec.calls.lock().unwrap().is_empty());
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
        vec![json!({"op": "source_list"})]
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
