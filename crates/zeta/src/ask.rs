//! `zeta ask`: let a local model call the node's operations to satisfy a request.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use zeta_agent::{
    Agent, AgentConfig, Executor, LlmClient, LlmConfig, LocalExecutor, RemoteExecutor,
};
use zeta_client::{NodeClient, op_tools, system_prompt};

use crate::cli::AskArgs;
use crate::sources::{PluginSpec, SourceSpec};
use crate::{agents_md, node_setup, serve_lock};

/// Run the request against the remote node, or against a local node on `state_dir`
/// with `sources` and `plugins`.
pub async fn run(
    args: AskArgs,
    remote: Option<NodeClient>,
    state_dir: PathBuf,
    sources: &[SourceSpec],
    plugins: &[PluginSpec],
) -> ExitCode {
    let request = match read_request(args.request.as_deref()) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    let cfg = llm_config(&args);
    let max_turns = args.max_turns;
    match remote {
        Some(client) => {
            let (schema, md) = match (client.schema().await, client.agents_md().await) {
                (Ok(s), Ok(m)) => (s, m),
                (Err(e), _) | (_, Err(e)) => return fail(&e.to_string()),
            };
            drive(
                cfg,
                RemoteExecutor(client),
                &schema,
                &md,
                max_turns,
                &request,
            )
            .await
        }
        None => {
            if let Some(pid) = serve_lock::owner(&state_dir) {
                return fail(&format!(
                    "zeta serve (pid {pid}) owns this state dir; use --node"
                ));
            }
            let node = match node_setup::open(state_dir, sources, plugins, false).await {
                Ok(n) => Arc::new(n),
                Err(e) => return fail(&e),
            };
            let md = agents_md::rendering();
            let schema = node.schema();
            drive(cfg, LocalExecutor(node), &schema, &md, max_turns, &request).await
        }
    }
}

async fn drive<E: Executor>(
    cfg: LlmConfig,
    exec: E,
    schema: &serde_json::Value,
    md: &str,
    max_turns: u32,
    request: &str,
) -> ExitCode {
    let llm = match LlmClient::new(cfg) {
        Ok(l) => l,
        Err(e) => return fail(&e.to_string()),
    };
    let mut agent_cfg = AgentConfig::new(system_prompt(md), op_tools(schema, md));
    agent_cfg.max_turns = max_turns;
    match Agent::new(llm, exec, agent_cfg).run(request).await {
        Ok(t) => print(serde_json::to_string_pretty(&t), 0),
        Err(f) => print(serde_json::to_string_pretty(&f), 1),
    }
}

fn llm_config(args: &AskArgs) -> LlmConfig {
    let mut cfg = LlmConfig::new(args.llm_url.clone(), args.llm_model.clone());
    cfg.key = args.llm_key.clone();
    cfg.max_tokens = args.llm_max_tokens;
    cfg.thinking = args.llm_thinking;
    cfg.status_url = args.llm_status_url.clone();
    cfg.ready_timeout = Duration::from_secs(args.llm_ready_timeout_s);
    cfg
}

/// The request text: the argument, or stdin when it is absent or `-`; trimmed, never empty.
fn read_request(arg: Option<&str>) -> Result<String, String> {
    let text = match arg {
        Some(a) if a != "-" => a.to_string(),
        _ => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| format!("reading stdin: {e}"))?;
            s
        }
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("empty request".into());
    }
    Ok(text.to_string())
}

fn print(json: serde_json::Result<String>, code: u8) -> ExitCode {
    match json {
        Ok(s) => {
            println!("{s}");
            ExitCode::from(code)
        }
        Err(e) => fail(&format!("serialising output: {e}")),
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("zeta: {msg}");
    ExitCode::FAILURE
}
