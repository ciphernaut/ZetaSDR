//! The agent against a real model server (ignored by default).
use serde_json::json;
use zeta_agent::fake::RecordingExecutor;
use zeta_agent::{Agent, AgentConfig, LlmClient, LlmConfig};
use zeta_client::{op_tools, system_prompt};
use zeta_core::ops::{OpResult, SourceInfo};
use zeta_core::port::Tuning;
use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat, SourceId};

const AGENTS_MD: &str = include_str!("../../../AGENTS.md");

#[tokio::test]
#[ignore = "needs field-host:8080 serving gemma-4-12b"]
async fn field_host_gemma_tunes_hackrf0() {
    let url = std::env::var("ZETA_LIVE_LLM_URL")
        .unwrap_or_else(|_| "http://field-host:8080".to_string());
    let llm = LlmClient::new(LlmConfig::new(url, "gemma-4-12b")).unwrap();
    let cfg = AgentConfig::new(
        system_prompt(AGENTS_MD),
        op_tools(&zeta_core::ops::schema(), AGENTS_MD),
    );
    let sources = OpResult::Sources(vec![SourceInfo {
        id: SourceId("hackrf0".into()),
        capabilities: Capabilities {
            tune_min_hz: Claim::Asserted(Hz(1_000_000)),
            tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
            max_bandwidth_hz: Claim::Asserted(Hz(20_000_000)),
            formats: vec![SampleFormat::Ci8],
            channels: 1,
            rx: true,
            tx: true,
            remote: false,
            push: false,
        },
        current: None,
        holders: vec![],
        elastic: vec![],
        pin: None,
        fault: None,
        bank: None,
        gain: None,
        stats: None,
    }]);
    let tuning = OpResult::Tuning(Tuning {
        center_hz: Hz(101_500_000),
        bandwidth_hz: Hz(200_000),
    });
    let exec = RecordingExecutor::new(vec![
        ("source_list", Ok(serde_json::to_value(sources).unwrap())),
        ("source_ensure", Ok(serde_json::to_value(tuning).unwrap())),
    ]);
    let agent = Agent::new(llm, &exec, cfg);

    let outcome = agent
        .run("tune hackrf0 to 101.5 MHz with 200 kHz bandwidth")
        .await;
    eprintln!("outcome: {outcome:#?}");
    let calls = exec.calls.lock().unwrap().clone();
    let want = json!({"op":"source_ensure","source":"hackrf0","center_hz":101500000,"bandwidth_hz":200000,"replace":[]});
    assert!(calls.contains(&want), "executor calls: {calls:#?}");
}
