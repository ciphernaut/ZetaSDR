//! `zeta ask`: let a local model call the node's operations to satisfy a request.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use zeta_agent::{Allow, Executor, LlmConfig, LocalExecutor, RemoteExecutor, build_agent};
use zeta_client::NodeClient;
use zeta_speech::SpeechBackend;

use crate::cli::{AskArgs, LlmArgs};
use crate::sources::{PluginSpec, SourceSpec};
use crate::{agents_md, node_setup, serve_lock};

/// Run the request against the remote node, or against a local node on `state_dir`
/// with `sources`, `plugins`, the operator's mode rows in `modes_dir` and the speech
/// backend `speech`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    args: AskArgs,
    remote: Option<NodeClient>,
    state_dir: PathBuf,
    sources: &[SourceSpec],
    plugins: &[PluginSpec],
    ring_budget_mb: Option<u64>,
    modes_dir: Option<PathBuf>,
    speech: Option<Arc<dyn SpeechBackend>>,
    knowledge_acma: Option<(String, Option<f64>)>,
) -> ExitCode {
    let request = match read_request(args.request.as_deref()) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    let cfg = match llm_config(&args.llm) {
        Ok(Some(c)) => c,
        Ok(None) => return fail("--llm-url and --llm-model are required"),
        Err(e) => return fail(&e),
    };
    let max_turns = args.max_turns;
    let allow = Allow {
        stop_jobs: args.allow_stop,
        replace: args.allow_replace,
    };
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
                allow,
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
            let acma_dir = state_dir.clone();
            let opened = node_setup::open(
                state_dir,
                sources,
                plugins,
                false,
                ring_budget_mb,
                modes_dir,
                speech,
            );
            let (node, remotes) = match opened.await {
                Ok((n, r)) => (Arc::new(n), r),
                Err(e) => return fail(&e),
            };
            node_setup::attach_acma(&node, &acma_dir, knowledge_acma.as_ref()).await;
            let md = agents_md::rendering();
            let schema = node.schema();
            let code = drive(
                cfg,
                LocalExecutor::new(node),
                &schema,
                &md,
                max_turns,
                allow,
                &request,
            )
            .await;
            remotes.close().await;
            code
        }
    }
}

async fn drive<E: Executor>(
    cfg: LlmConfig,
    exec: E,
    schema: &serde_json::Value,
    md: &str,
    max_turns: u32,
    allow: Allow,
    request: &str,
) -> ExitCode {
    let agent = match build_agent(cfg, exec, schema, md, max_turns, allow) {
        Ok(a) => a,
        Err(e) => return fail(&e.to_string()),
    };
    match agent.run(request).await {
        Ok(t) => print(serde_json::to_string_pretty(&t), 0),
        Err(f) => print(serde_json::to_string_pretty(&f), 1),
    }
}

/// The model the flags name: `None` without `--llm-url` and `--llm-model`,
/// an error with only one of them.
pub fn llm_config(args: &LlmArgs) -> Result<Option<LlmConfig>, String> {
    let (url, model) = match (&args.llm_url, &args.llm_model) {
        (Some(url), Some(model)) => (url, model),
        (None, None) => return Ok(None),
        (Some(_), None) => return Err("--llm-url needs --llm-model".into()),
        (None, Some(_)) => return Err("--llm-model needs --llm-url".into()),
    };
    let mut cfg = LlmConfig::new(url.clone(), model.clone());
    cfg.key = args.llm_key.clone();
    cfg.max_tokens = args.llm_max_tokens;
    cfg.thinking = args.llm_thinking;
    cfg.status_url = args.llm_status_url.clone();
    cfg.ready_timeout = Duration::from_secs(args.llm_ready_timeout_s);
    Ok(Some(cfg))
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
